// Looker's entry points into FFmpeg (built into looker_video.dll by the looker-video overlay port, beside the
// FFmpeg DLLs). Looker loads this one DLL with LoadLibraryExW and calls only what's here, so Rust never binds
// FFmpeg's structs (their layouts change between versions). See native/PLAN.md N6.
//
// Files are read through a custom AVIO over a CreateFileW handle shared for read, write and delete: Looker can
// rename, move or recycle a video while it is open.
//
// Every function returns 0 or a negative AVERROR code (looker_video_error names it) and never lets a crash out
// of a bad file that FFmpeg itself would survive.

#define WIN32_LEAN_AND_MEAN
#include <windows.h>

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/display.h>
#include <libavutil/imgutils.h>
#include <libavutil/parseutils.h>
#include <libavutil/pixdesc.h>
#include <libswscale/swscale.h>

#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define EXPORT __declspec(dllexport)

// Bumped whenever a signature or LvInfo changes; Looker refuses a DLL whose version differs.
#define LOOKER_VIDEO_API 1

typedef struct LvInfo
{
    int width, height;             // display size: upright, square pixels
    int coded_width, coded_height; // as stored
    int rotation;                  // clockwise degrees to show it upright: 0, 90, 180 or 270
    long long duration_us;         // 0 when unknown
    int fps_num, fps_den;          // 0/0 when unknown
    long long bit_rate;            // bits per second, 0 when unknown
    int bit_depth;
    int has_alpha;
    int primaries, transfer, matrix, full_range; // AVCOL_PRI_*, AVCOL_TRC_*, AVCOL_SPC_*, 0/1
    int audio_channels, audio_rate;              // 0 without audio
    long long created;                           // Unix seconds from the container's creation_time, 0 if none
    char codec[32];                              // FFmpeg's short codec name: "h264", "hevc", "prores"...
    char profile[48];                            // "High", "Main 10", "422 HQ"... or ""
    char pix_fmt[32];
    char audio_codec[32];
} LvInfo;

// ---- file input -----------------------------------------------------------------------------------------------

typedef struct Input
{
    HANDLE file;
    AVFormatContext *fmt;
    AVIOContext *io;
} Input;

static int io_read(void *opaque, uint8_t *buf, int size)
{
    DWORD got = 0;
    if (!ReadFile((HANDLE)opaque, buf, (DWORD)size, &got, NULL))
        return AVERROR(EIO);
    return got ? (int)got : AVERROR_EOF;
}

static int64_t io_seek(void *opaque, int64_t offset, int whence)
{
    HANDLE h = (HANDLE)opaque;
    if (whence & AVSEEK_SIZE)
    {
        LARGE_INTEGER size;
        return GetFileSizeEx(h, &size) ? size.QuadPart : AVERROR(EIO);
    }
    DWORD method = FILE_BEGIN;
    switch (whence & ~AVSEEK_FORCE)
    {
    case SEEK_CUR:
        method = FILE_CURRENT;
        break;
    case SEEK_END:
        method = FILE_END;
        break;
    }
    LARGE_INTEGER to, at;
    to.QuadPart = offset;
    return SetFilePointerEx(h, to, &at, method) ? at.QuadPart : AVERROR(EIO);
}

static void input_close(Input *in)
{
    if (in->fmt)
        avformat_close_input(&in->fmt);
    if (in->io)
    {
        av_freep(&in->io->buffer);
        avio_context_free(&in->io);
    }
    if (in->file != INVALID_HANDLE_VALUE && in->file)
        CloseHandle(in->file);
    in->file = INVALID_HANDLE_VALUE;
}

static void quiet_once(void)
{
    static LONG done = 0;
    if (InterlockedExchange(&done, 1) == 0)
        av_log_set_level(AV_LOG_QUIET);
}

// Opens the container; `full` also runs avformat_find_stream_info, which decodes a run of frames to fill in what
// the headers left out. A still skips it when the headers already give the picture's codec and size (MP4, MOV,
// Matroska, MXF, AVI do): on a 1080p60 H.264 recording that saves ~50 ms.
static int input_open(Input *in, const wchar_t *path, int full)
{
    quiet_once();
    memset(in, 0, sizeof *in);
    in->file = CreateFileW(path, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, NULL, OPEN_EXISTING,
                           FILE_ATTRIBUTE_NORMAL, NULL);
    if (in->file == INVALID_HANDLE_VALUE)
        return AVERROR(ENOENT);
    const int size = 1 << 16;
    unsigned char *buf = av_malloc(size);
    if (!buf)
        return AVERROR(ENOMEM);
    in->io = avio_alloc_context(buf, size, 0, in->file, io_read, NULL, io_seek);
    if (!in->io)
    {
        av_free(buf);
        return AVERROR(ENOMEM);
    }
    in->fmt = avformat_alloc_context();
    if (!in->fmt)
        return AVERROR(ENOMEM);
    in->fmt->pb = in->io;
    in->fmt->flags |= AVFMT_FLAG_CUSTOM_IO;
    // The name only helps the probe (by extension); the bytes come through the AVIO.
    char name[1024] = "";
    WideCharToMultiByte(CP_UTF8, 0, path, -1, name, sizeof name, NULL, NULL);
    int r = avformat_open_input(&in->fmt, name, NULL, NULL);
    if (r < 0)
        return r; // avformat_open_input freed the context
    return full ? avformat_find_stream_info(in->fmt, NULL) : 0;
}

// ---- stream info ----------------------------------------------------------------------------------------------

static void copy_str(char *dst, size_t cap, const char *src)
{
    if (!src)
        src = "";
    strncpy(dst, src, cap - 1);
    dst[cap - 1] = 0;
}

static int rotation_of(const AVStream *st)
{
    const AVPacketSideData *sd =
        av_packet_side_data_get(st->codecpar->coded_side_data, st->codecpar->nb_coded_side_data, AV_PKT_DATA_DISPLAYMATRIX);
    if (!sd || sd->size < 9 * 4)
        return 0;
    double ccw = av_display_rotation_get((const int32_t *)sd->data);
    if (isnan(ccw))
        return 0;
    int cw = (int)lround(-ccw);
    cw = ((cw % 360) + 360) % 360;
    return ((cw + 45) / 90 % 4) * 90;
}

static void fill_info(const AVFormatContext *fmt, const AVStream *st, const AVStream *audio, LvInfo *info)
{
    memset(info, 0, sizeof *info);
    const AVCodecParameters *p = st->codecpar;
    info->coded_width = p->width;
    info->coded_height = p->height;
    AVRational sar = av_guess_sample_aspect_ratio((AVFormatContext *)fmt, (AVStream *)st, NULL);
    int w = p->width, h = p->height;
    if (sar.num > 0 && sar.den > 0 && sar.num != sar.den)
        w = (int)lround((double)w * sar.num / sar.den);
    info->rotation = rotation_of(st);
    if (info->rotation == 90 || info->rotation == 270)
    {
        info->width = h;
        info->height = w;
    }
    else
    {
        info->width = w;
        info->height = h;
    }
    if (fmt->duration > 0)
        info->duration_us = fmt->duration;
    else if (st->duration > 0)
        info->duration_us = av_rescale_q(st->duration, st->time_base, AV_TIME_BASE_Q);
    AVRational fps = st->avg_frame_rate.num > 0 ? st->avg_frame_rate : st->r_frame_rate;
    if (fps.num > 0 && fps.den > 0)
    {
        info->fps_num = fps.num;
        info->fps_den = fps.den;
    }
    info->bit_rate = p->bit_rate > 0 ? p->bit_rate : fmt->bit_rate;
    const AVPixFmtDescriptor *pd = av_pix_fmt_desc_get((enum AVPixelFormat)p->format);
    if (pd)
    {
        info->bit_depth = pd->comp[0].depth;
        info->has_alpha = (pd->flags & AV_PIX_FMT_FLAG_ALPHA) != 0;
        copy_str(info->pix_fmt, sizeof info->pix_fmt, pd->name);
    }
    info->primaries = p->color_primaries;
    info->transfer = p->color_trc;
    info->matrix = p->color_space;
    info->full_range = p->color_range == AVCOL_RANGE_JPEG;
    const AVCodecDescriptor *cd = avcodec_descriptor_get(p->codec_id);
    copy_str(info->codec, sizeof info->codec, cd ? cd->name : "");
    copy_str(info->profile, sizeof info->profile, avcodec_profile_name(p->codec_id, p->profile));
    if (audio)
    {
        info->audio_channels = audio->codecpar->ch_layout.nb_channels;
        info->audio_rate = audio->codecpar->sample_rate;
        const AVCodecDescriptor *ad = avcodec_descriptor_get(audio->codecpar->codec_id);
        copy_str(info->audio_codec, sizeof info->audio_codec, ad ? ad->name : "");
    }
    const AVDictionaryEntry *t = av_dict_get(fmt->metadata, "creation_time", NULL, 0);
    if (!t)
        t = av_dict_get(st->metadata, "creation_time", NULL, 0);
    int64_t us = 0;
    if (t && av_parse_time(&us, t->value, 0) == 0 && us > 0)
        info->created = us / 1000000;
}

// The video stream to show (never cover art) and the audio that goes with it.
static int pick_streams(AVFormatContext *fmt, int *video, int *audio)
{
    int v = -1;
    for (unsigned i = 0; i < fmt->nb_streams; i++)
        fmt->streams[i]->discard = AVDISCARD_ALL;
    int best = av_find_best_stream(fmt, AVMEDIA_TYPE_VIDEO, -1, -1, NULL, 0);
    if (best >= 0 && !(fmt->streams[best]->disposition & AV_DISPOSITION_ATTACHED_PIC))
        v = best;
    for (unsigned i = 0; v < 0 && i < fmt->nb_streams; i++)
    {
        const AVStream *s = fmt->streams[i];
        if (s->codecpar->codec_type == AVMEDIA_TYPE_VIDEO && !(s->disposition & AV_DISPOSITION_ATTACHED_PIC))
            v = (int)i;
    }
    if (v < 0)
        return AVERROR_STREAM_NOT_FOUND;
    *video = v;
    *audio = av_find_best_stream(fmt, AVMEDIA_TYPE_AUDIO, -1, v, NULL, 0);
    return 0;
}

// AV1's native decoder only drives the GPU; software AV1 is dav1d.
static const AVCodec *decoder_for(const AVCodecParameters *p)
{
    if (p->codec_id == AV_CODEC_ID_AV1)
    {
        const AVCodec *d = avcodec_find_decoder_by_name("libdav1d");
        if (d)
            return d;
    }
    return avcodec_find_decoder(p->codec_id);
}

// ---- one still frame ------------------------------------------------------------------------------------------

static int sws_space(enum AVColorSpace s, int height)
{
    switch (s)
    {
    case AVCOL_SPC_BT709:
        return SWS_CS_ITU709;
    case AVCOL_SPC_BT470BG:
    case AVCOL_SPC_SMPTE170M:
        return SWS_CS_ITU601;
    case AVCOL_SPC_SMPTE240M:
        return SWS_CS_SMPTE240M;
    case AVCOL_SPC_FCC:
        return SWS_CS_FCC;
    case AVCOL_SPC_BT2020_NCL:
    case AVCOL_SPC_BT2020_CL:
        return SWS_CS_BT2020;
    default:
        return height >= 720 ? SWS_CS_ITU709 : SWS_CS_ITU601; // what players assume for untagged video
    }
}

static int is_full_range(const AVFrame *f)
{
    switch ((enum AVPixelFormat)f->format)
    {
    case AV_PIX_FMT_YUVJ420P:
    case AV_PIX_FMT_YUVJ422P:
    case AV_PIX_FMT_YUVJ444P:
    case AV_PIX_FMT_YUVJ440P:
    case AV_PIX_FMT_YUVJ411P:
        return 1;
    default:
        return f->color_range == AVCOL_RANGE_JPEG;
    }
}

// Rotates straight RGBA by 90, 180 or 270 degrees clockwise into a new buffer.
static unsigned char *rotate_rgba(const unsigned char *src, int w, int h, int cw)
{
    unsigned char *dst = malloc((size_t)w * h * 4);
    if (!dst)
        return NULL;
    const uint32_t *s = (const uint32_t *)src;
    uint32_t *d = (uint32_t *)dst;
    for (int y = 0; y < h; y++)
        for (int x = 0; x < w; x++)
        {
            uint32_t px = s[(size_t)y * w + x];
            if (cw == 90) // new size h x w: (x, y) -> (h - 1 - y, x)
                d[(size_t)x * h + (h - 1 - y)] = px;
            else if (cw == 180)
                d[(size_t)(h - 1 - y) * w + (w - 1 - x)] = px;
            else // 270: (x, y) -> (y, w - 1 - x)
                d[(size_t)(w - 1 - x) * h + y] = px;
        }
    return dst;
}

static int decode_first(AVFormatContext *fmt, int vi, AVCodecContext *dec, AVFrame *frame)
{
    AVPacket *pkt = av_packet_alloc();
    if (!pkt)
        return AVERROR(ENOMEM);
    int r, sent_eof = 0;
    // A cap, so a stream that never yields a picture can't spin forever.
    for (int packets = 0; packets < 4000;)
    {
        r = avcodec_receive_frame(dec, frame);
        if (r == 0)
            break;
        if (r != AVERROR(EAGAIN))
            break; // AVERROR_EOF after the flush, or a real error
        if (sent_eof)
        {
            r = AVERROR_EOF;
            break;
        }
        r = av_read_frame(fmt, pkt);
        if (r == AVERROR_EOF)
        {
            avcodec_send_packet(dec, NULL);
            sent_eof = 1;
            continue;
        }
        if (r < 0)
            break;
        if (pkt->stream_index == vi)
        {
            packets++;
            int s = avcodec_send_packet(dec, pkt);
            if (s < 0 && s != AVERROR(EAGAIN) && s != AVERROR_INVALIDDATA)
                r = s;
        }
        av_packet_unref(pkt);
        if (r < 0)
            break;
    }
    av_packet_free(&pkt);
    return r;
}

EXPORT int looker_video_api(void)
{
    return LOOKER_VIDEO_API;
}

// Stream info without decoding anything.
EXPORT int looker_video_probe(const wchar_t *path, LvInfo *info)
{
    Input in;
    int r = input_open(&in, path, 1);
    int vi = -1, ai = -1;
    if (r >= 0)
        r = pick_streams(in.fmt, &vi, &ai);
    if (r >= 0)
        fill_info(in.fmt, in.fmt->streams[vi], ai >= 0 ? in.fmt->streams[ai] : NULL, info);
    input_close(&in);
    return r < 0 ? r : 0;
}

// The first picture, upright, square-pixelled and fitted into box_w x box_h (0 x 0 = full size; never enlarged), as
// straight RGBA in a buffer freed by looker_video_free; plus the stream info.
EXPORT int looker_video_still(const wchar_t *path, int box_w, int box_h, LvInfo *info, int *out_w, int *out_h,
                              unsigned char **out_rgba)
{
    *out_rgba = NULL;
    *out_w = *out_h = 0;
    Input in;
    AVCodecContext *dec = NULL;
    AVFrame *frame = NULL;
    struct SwsContext *sws = NULL;
    unsigned char *rgba = NULL;
    int vi = -1, ai = -1;
    int r = input_open(&in, path, 0);
    if (r < 0)
        goto done;
    r = pick_streams(in.fmt, &vi, &ai);
    if (r < 0 || in.fmt->streams[vi]->codecpar->width <= 0 || in.fmt->streams[vi]->codecpar->codec_id == AV_CODEC_ID_NONE)
    {
        // Headers that don't say (MPEG-TS/PS, some AVIs): probe properly.
        for (unsigned i = 0; i < in.fmt->nb_streams; i++)
            in.fmt->streams[i]->discard = AVDISCARD_DEFAULT;
        if ((r = avformat_find_stream_info(in.fmt, NULL)) < 0 || (r = pick_streams(in.fmt, &vi, &ai)) < 0)
            goto done;
    }
    AVStream *st = in.fmt->streams[vi];
    st->discard = AVDISCARD_DEFAULT;
    fill_info(in.fmt, st, ai >= 0 ? in.fmt->streams[ai] : NULL, info);

    const AVCodec *codec = decoder_for(st->codecpar);
    if (!codec)
    {
        r = AVERROR_DECODER_NOT_FOUND;
        goto done;
    }
    dec = avcodec_alloc_context3(codec);
    frame = av_frame_alloc();
    if (!dec || !frame)
    {
        r = AVERROR(ENOMEM);
        goto done;
    }
    if ((r = avcodec_parameters_to_context(dec, st->codecpar)) < 0)
        goto done;
    dec->pkt_timebase = st->time_base;
    // Slice threads: frame threads would hold the first picture back until several are in flight.
    dec->thread_count = 0;
    dec->thread_type = FF_THREAD_SLICE;
    if ((r = avcodec_open2(dec, codec, NULL)) < 0)
        goto done;
    if ((r = decode_first(in.fmt, vi, dec, frame)) < 0)
        goto done;
    // The decoded picture is the truth for its format (the headers may not have said).
    const AVPixFmtDescriptor *pd = av_pix_fmt_desc_get((enum AVPixelFormat)frame->format);
    if (pd)
    {
        info->bit_depth = pd->comp[0].depth;
        info->has_alpha = (pd->flags & AV_PIX_FMT_FLAG_ALPHA) != 0;
        copy_str(info->pix_fmt, sizeof info->pix_fmt, pd->name);
    }
    if (frame->color_primaries != AVCOL_PRI_UNSPECIFIED)
        info->primaries = frame->color_primaries;
    if (frame->color_trc != AVCOL_TRC_UNSPECIFIED)
        info->transfer = frame->color_trc;
    if (frame->colorspace != AVCOL_SPC_UNSPECIFIED)
        info->matrix = frame->colorspace;

    // Square pixels, then fitted to the box in the stored orientation (the rotation comes after).
    int fw = frame->width, fh = frame->height;
    AVRational sar = frame->sample_aspect_ratio.num > 0 ? frame->sample_aspect_ratio : st->sample_aspect_ratio;
    double dw = fw, dh = fh;
    if (sar.num > 0 && sar.den > 0 && sar.num != sar.den)
        dw = dw * sar.num / sar.den;
    int rot = info->rotation;
    double bw = box_w, bh = box_h;
    if (rot == 90 || rot == 270)
    {
        double t = bw;
        bw = bh;
        bh = t;
    }
    double scale = 1.0;
    if (box_w > 0 && box_h > 0)
    {
        scale = fmin(bw / dw, bh / dh);
        if (scale > 1.0)
            scale = 1.0;
    }
    int sw = (int)lround(dw * scale), sh = (int)lround(dh * scale);
    if (sw < 1)
        sw = 1;
    if (sh < 1)
        sh = 1;

    sws = sws_getContext(fw, fh, (enum AVPixelFormat)frame->format, sw, sh, AV_PIX_FMT_RGBA,
                         SWS_BICUBIC | SWS_ACCURATE_RND | SWS_FULL_CHR_H_INT, NULL, NULL, NULL);
    if (!sws)
    {
        r = AVERROR(EINVAL);
        goto done;
    }
    const int *coeffs = sws_getCoefficients(sws_space(frame->colorspace, fh));
    // Fails (harmlessly) for RGB sources.
    sws_setColorspaceDetails(sws, coeffs, is_full_range(frame), sws_getCoefficients(SWS_CS_DEFAULT), 1, 0, 1 << 16, 1 << 16);
    rgba = malloc((size_t)sw * sh * 4);
    if (!rgba)
    {
        r = AVERROR(ENOMEM);
        goto done;
    }
    uint8_t *dst[4] = {rgba, NULL, NULL, NULL};
    int dst_stride[4] = {sw * 4, 0, 0, 0};
    if ((r = sws_scale(sws, (const uint8_t *const *)frame->data, frame->linesize, 0, fh, dst, dst_stride)) <= 0)
    {
        r = r < 0 ? r : AVERROR(EINVAL);
        goto done;
    }
    r = 0;
    if (rot)
    {
        unsigned char *turned = rotate_rgba(rgba, sw, sh, rot);
        if (!turned)
        {
            r = AVERROR(ENOMEM);
            goto done;
        }
        free(rgba);
        rgba = turned;
        if (rot != 180)
        {
            int t = sw;
            sw = sh;
            sh = t;
        }
    }
    *out_rgba = rgba;
    *out_w = sw;
    *out_h = sh;
    rgba = NULL;

done:
    free(rgba);
    sws_freeContext(sws);
    av_frame_free(&frame);
    avcodec_free_context(&dec);
    input_close(&in);
    return r;
}

EXPORT void looker_video_free(void *p)
{
    free(p);
}

EXPORT void looker_video_error(int code, char *buf, int len)
{
    if (av_strerror(code, buf, (size_t)len) < 0)
        snprintf(buf, (size_t)len, "error %d", code);
}
