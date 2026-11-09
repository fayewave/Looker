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
#define COBJMACROS
#include <windows.h>
#include <d3d11.h>

#include <libavcodec/avcodec.h>
#include <libavfilter/avfilter.h>
#include <libavfilter/buffersink.h>
#include <libavfilter/buffersrc.h>
#include <libavformat/avformat.h>
#include <libavutil/opt.h>
#include <libavutil/channel_layout.h>
#include <libavutil/display.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_d3d11va.h>
#include <libavutil/imgutils.h>
#include <libavutil/parseutils.h>
#include <libavutil/pixdesc.h>
#include <libswresample/swresample.h>
#include <libswscale/swscale.h>

#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define EXPORT __declspec(dllexport)

// Bumped whenever a signature or LvInfo changes; Looker refuses a DLL whose version differs.
#define LOOKER_VIDEO_API 4

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
    if ((fps.num <= 0 || fps.den <= 0) && st->time_base.num > 0 && st->time_base.den >= 10 * st->time_base.num &&
        st->time_base.den <= 300 * st->time_base.num)
        fps = av_inv_q(st->time_base);
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

// The display size from the picture as decoded: square pixels, upright.
static void set_display(LvInfo *info, int w, int h, AVRational sar)
{
    info->coded_width = w;
    info->coded_height = h;
    if (sar.num > 0 && sar.den > 0 && sar.num != sar.den)
        w = (int)lround((double)w * sar.num / sar.den);
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

// A picture square-pixelled, fitted into box_w x box_h (0 x 0 = full size; never enlarged) and turned upright, as
// straight RGBA in a buffer freed by looker_video_free.
static int fit_rgba(const AVFrame *frame, AVRational sar, int rot, int box_w, int box_h, int sws_flags, int *out_w,
                    int *out_h, unsigned char **out_rgba)
{
    *out_rgba = NULL;
    *out_w = *out_h = 0;
    int fw = frame->width, fh = frame->height;
    double dw = fw, dh = fh;
    if (sar.num > 0 && sar.den > 0 && sar.num != sar.den)
        dw = dw * sar.num / sar.den;
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
    struct SwsContext *sws =
        sws_getContext(fw, fh, (enum AVPixelFormat)frame->format, sw, sh, AV_PIX_FMT_RGBA, sws_flags, NULL, NULL, NULL);
    if (!sws)
        return AVERROR(EINVAL);
    const int *coeffs = sws_getCoefficients(sws_space(frame->colorspace, fh));
    // Fails (harmlessly) for RGB sources.
    sws_setColorspaceDetails(sws, coeffs, is_full_range(frame), sws_getCoefficients(SWS_CS_DEFAULT), 1, 0, 1 << 16, 1 << 16);
    unsigned char *rgba = malloc((size_t)sw * sh * 4);
    int r = 0;
    if (!rgba)
        r = AVERROR(ENOMEM);
    else
    {
        uint8_t *dst[4] = {rgba, NULL, NULL, NULL};
        int dst_stride[4] = {sw * 4, 0, 0, 0};
        r = sws_scale(sws, (const uint8_t *const *)frame->data, frame->linesize, 0, fh, dst, dst_stride);
        r = r > 0 ? 0 : r < 0 ? r : AVERROR(EINVAL);
    }
    sws_freeContext(sws);
    if (r == 0 && rot)
    {
        unsigned char *turned = rotate_rgba(rgba, sw, sh, rot);
        if (!turned)
            r = AVERROR(ENOMEM);
        else
        {
            free(rgba);
            rgba = turned;
            if (rot != 180)
            {
                int t = sw;
                sw = sh;
                sh = t;
            }
        }
    }
    if (r < 0)
    {
        free(rgba);
        return r;
    }
    *out_rgba = rgba;
    *out_w = sw;
    *out_h = sh;
    return 0;
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
    // The decoded picture is the truth for its size and format (the headers may not have said).
    set_display(info, frame->width, frame->height,
                frame->sample_aspect_ratio.num > 0 ? frame->sample_aspect_ratio : st->sample_aspect_ratio);
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

    r = fit_rgba(frame, frame->sample_aspect_ratio.num > 0 ? frame->sample_aspect_ratio : st->sample_aspect_ratio,
                 info->rotation, box_w, box_h, SWS_BICUBIC | SWS_ACCURATE_RND | SWS_FULL_CHR_H_INT, out_w, out_h, out_rgba);

done:
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

// ---- playback -------------------------------------------------------------------------------------------------
//
// One player per open video, driven from one thread (Looker's decode thread): lv_next hands back the next video
// picture or block of audio in file order. Pictures decoded on the GPU (D3D11VA on Looker's own device) come back
// as a slice of the decoder's texture array (NV12 or P010); everything else as CPU planes, or as RGBA for the odd
// packed formats. Audio is always interleaved float stereo at 48 kHz. Times are microseconds from the video
// stream's start. A frame stays valid until lv_frame_free, which any thread may call.

enum
{
    LV_VIDEO = 1,
    LV_AUDIO = 2,
};
enum
{
    LV_NO_AUDIO = 1, // lv_open: pictures only (the seek bar's previews)
};
enum
{
    LV_TEXTURE = 1, // texture + texture_index: an NV12 (8-bit) or P010 (10-bit, MSB-aligned) array slice
    LV_PLANAR = 2,  // data[0..nplanes]: Y, U, V (+ A), or Y and interleaved UV when nplanes == 2
    LV_RGBA = 3,    // data[0]: straight RGBA, 8 bits per channel
};

typedef struct LvFrame
{
    int kind, layout;
    long long pts_us, duration_us;
    int width, height;    // the picture, as stored (before rotation)
    int bit_depth, shift; // samples wider than 8 bits sit in 16-bit words, shifted left by `shift`
    int chroma_w_shift, chroma_h_shift;
    int nplanes;
    int matrix, full_range, transfer, primaries;
    void *texture;
    int texture_index;
    const unsigned char *data[4];
    int linesize[4];
    const float *samples; // audio: valid until the next lv_next
    int nb_samples;
    void *opaque; // the AVFrame behind a picture
} LvFrame;

typedef struct LvPlayer
{
    Input in;
    int vi, ai;
    long long start_us; // the video stream's start time, the zero of every time handed out
    AVCodecContext *vdec, *adec;
    int hw;
    SwrContext *swr;
    struct SwsContext *sws;
    AVPacket *pkt;
    long long skip_until_us; // an exact seek drops what comes before its target
    int demux_eof, v_done, a_done;
    long long next_vpts; // for pictures without a timestamp
    float *abuf;
    int abuf_cap;
    AVFrame *held;        // an exact seek's best picture so far (at or before the target)
    AVFrame *pending;     // the picture after it, handed out next
    int seek_first;       // no picture yet since the seek
    int seek_retries;     // landed past the target: sought further back this many times
    long long seek_target;
    int audio_skip;       // an exact seek: audio that ends before the target is dropped
    // lv_next_audio read past video packets to reach the sound: they wait here, in order, for lv_next.
    AVPacket **vq;
    int vq_head, vq_n, vq_cap;
    long long vq_bytes;
    int v_flush; // the end was reached by lv_next_audio: the video decoder still needs its flush after vq
} LvPlayer;

// How far lv_next_audio may read ahead of the pictures: packets and bytes.
#define VQ_MAX_PACKETS 2000
#define VQ_MAX_BYTES (192LL << 20)

static void vq_clear(LvPlayer *p)
{
    while (p->vq_n > 0)
    {
        av_packet_free(&p->vq[p->vq_head]);
        p->vq_head = (p->vq_head + 1) % p->vq_cap;
        p->vq_n--;
    }
    p->vq_head = 0;
    p->vq_bytes = 0;
    p->v_flush = 0;
}

// Takes `pkt`'s reference into the queue (lv_next_audio checks the limits before it reads).
static int vq_push(LvPlayer *p, AVPacket *pkt)
{
    if (p->vq_n == p->vq_cap)
    {
        int cap = p->vq_cap ? p->vq_cap * 2 : 64;
        AVPacket **q = malloc(sizeof *q * cap);
        if (q == NULL)
            return AVERROR(ENOMEM);
        for (int i = 0; i < p->vq_n; i++)
            q[i] = p->vq[(p->vq_head + i) % p->vq_cap];
        free(p->vq);
        p->vq = q;
        p->vq_cap = cap;
        p->vq_head = 0;
    }
    AVPacket *k = av_packet_alloc();
    if (k == NULL)
        return AVERROR(ENOMEM);
    av_packet_move_ref(k, pkt);
    p->vq[(p->vq_head + p->vq_n) % p->vq_cap] = k;
    p->vq_n++;
    p->vq_bytes += k->size;
    return 0;
}

static AVPacket *vq_pop(LvPlayer *p)
{
    if (p->vq_n == 0)
        return NULL;
    AVPacket *k = p->vq[p->vq_head];
    p->vq_head = (p->vq_head + 1) % p->vq_cap;
    p->vq_n--;
    p->vq_bytes -= k->size;
    return k;
}

#define LV_RATE 48000

static const GUID IID_VideoDevice = {0x10ec4d5b, 0x975a, 0x4689, {0xb9, 0xe4, 0xd0, 0xaa, 0xc3, 0x0f, 0xe3, 0x33}};
static const GUID AV1_PROFILE0 = {0xb8be4ccb, 0xcf53, 0x46ba, {0x8d, 0x59, 0xd6, 0xb8, 0xa6, 0xda, 0x5d, 0x2a}};

// Whether the GPU decodes AV1 (FFmpeg's own AV1 decoder only drives the GPU; dav1d is the CPU one).
static int gpu_decodes_av1(ID3D11Device *dev)
{
    ID3D11VideoDevice *vd = NULL;
    if (FAILED(ID3D11Device_QueryInterface(dev, &IID_VideoDevice, (void **)&vd)) || vd == NULL)
        return 0;
    int found = 0;
    UINT n = ID3D11VideoDevice_GetVideoDecoderProfileCount(vd);
    for (UINT i = 0; i < n && found == 0; i++)
    {
        GUID g;
        if (SUCCEEDED(ID3D11VideoDevice_GetVideoDecoderProfile(vd, i, &g)) && IsEqualGUID(&g, &AV1_PROFILE0))
            found = 1;
    }
    ID3D11VideoDevice_Release(vd);
    return found;
}

static enum AVPixelFormat pick_format(AVCodecContext *c, const enum AVPixelFormat *fmts)
{
    (void)c;
    for (const enum AVPixelFormat *p = fmts; *p != AV_PIX_FMT_NONE; p++)
        if (*p == AV_PIX_FMT_D3D11)
            return *p;
    for (const enum AVPixelFormat *p = fmts; *p != AV_PIX_FMT_NONE; p++)
    {
        const AVPixFmtDescriptor *d = av_pix_fmt_desc_get(*p);
        if (d && (d->flags & AV_PIX_FMT_FLAG_HWACCEL) == 0)
            return *p;
    }
    return AV_PIX_FMT_NONE;
}

static int open_decoder(AVStream *st, const AVCodec *codec, AVBufferRef *hw, AVCodecContext **out)
{
    *out = NULL;
    AVCodecContext *c = avcodec_alloc_context3(codec);
    if (c == NULL)
        return AVERROR(ENOMEM);
    int r = avcodec_parameters_to_context(c, st->codecpar);
    if (r >= 0)
    {
        c->pkt_timebase = st->time_base;
        c->thread_count = 0;
        c->thread_type = FF_THREAD_FRAME | FF_THREAD_SLICE;
        if (hw)
        {
            c->hw_device_ctx = av_buffer_ref(hw);
            c->get_format = pick_format;
            c->extra_hw_frames = 10; // Looker's queue and the picture on screen hold surfaces too
            c->thread_count = 1;     // frame threads and D3D11VA don't mix
        }
        r = avcodec_open2(c, codec, NULL);
    }
    if (r < 0)
        avcodec_free_context(&c);
    *out = c;
    return r;
}

EXPORT void lv_close(LvPlayer *p)
{
    if (p == NULL)
        return;
    av_frame_free(&p->held);
    av_frame_free(&p->pending);
    vq_clear(p);
    free(p->vq);
    avcodec_free_context(&p->vdec);
    avcodec_free_context(&p->adec);
    swr_free(&p->swr);
    sws_freeContext(p->sws);
    av_packet_free(&p->pkt);
    input_close(&p->in);
    av_free(p->abuf);
    free(p);
}

// Opens `path` for playback. `d3d11` (an ID3D11Device made with D3D11_CREATE_DEVICE_VIDEO_SUPPORT, or NULL)
// decodes on that GPU where it can. `flags`: LV_NO_AUDIO.
EXPORT int lv_open(const wchar_t *path, void *d3d11, int flags, LvInfo *info, LvPlayer **out)
{
    *out = NULL;
    LvPlayer *p = calloc(1, sizeof *p);
    if (p == NULL)
        return AVERROR(ENOMEM);
    p->in.file = INVALID_HANDLE_VALUE;
    p->vi = p->ai = -1;
    p->skip_until_us = INT64_MIN;
    AVBufferRef *hw = NULL;
    int r = input_open(&p->in, path, 0);
    if (r < 0)
        goto fail;
    AVFormatContext *fmt = p->in.fmt;
    r = pick_streams(fmt, &p->vi, &p->ai);
    LvInfo quick;
    if (r >= 0)
        fill_info(fmt, fmt->streams[p->vi], NULL, &quick);
    // The headers must give the picture's size, the audio's rate and the frame rate (ASF doesn't), else probe.
    if (r < 0 || fmt->streams[p->vi]->codecpar->width <= 0 || quick.fps_num <= 0 ||
        (p->ai >= 0 && fmt->streams[p->ai]->codecpar->sample_rate <= 0))
    {
        for (unsigned i = 0; i < fmt->nb_streams; i++)
            fmt->streams[i]->discard = AVDISCARD_DEFAULT;
        if ((r = avformat_find_stream_info(fmt, NULL)) < 0 || (r = pick_streams(fmt, &p->vi, &p->ai)) < 0)
            goto fail;
    }
    AVStream *vs = fmt->streams[p->vi];
    vs->discard = AVDISCARD_DEFAULT;
    fill_info(fmt, vs, p->ai >= 0 ? fmt->streams[p->ai] : NULL, info);
    p->start_us = vs->start_time != AV_NOPTS_VALUE ? av_rescale_q(vs->start_time, vs->time_base, AV_TIME_BASE_Q) : 0;

    if (d3d11)
    {
        hw = av_hwdevice_ctx_alloc(AV_HWDEVICE_TYPE_D3D11VA);
        if (hw)
        {
            AVD3D11VADeviceContext *dc = ((AVHWDeviceContext *)hw->data)->hwctx;
            ID3D11Device_AddRef((ID3D11Device *)d3d11);
            dc->device = (ID3D11Device *)d3d11;
            if (av_hwdevice_ctx_init(hw) < 0)
                av_buffer_unref(&hw);
        }
    }
    const AVCodec *vc = NULL;
    if (vs->codecpar->codec_id == AV_CODEC_ID_AV1 && (hw == NULL || gpu_decodes_av1((ID3D11Device *)d3d11) == 0))
        vc = avcodec_find_decoder_by_name("libdav1d");
    if (vc == NULL)
        vc = avcodec_find_decoder(vs->codecpar->codec_id);
    if (vc == NULL)
    {
        r = AVERROR_DECODER_NOT_FOUND;
        goto fail;
    }
    int can_hw = 0;
    for (int i = 0; hw && can_hw == 0; i++)
    {
        const AVCodecHWConfig *c = avcodec_get_hw_config(vc, i);
        if (c == NULL)
            break;
        can_hw = (c->methods & AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX) && c->device_type == AV_HWDEVICE_TYPE_D3D11VA;
    }
    if ((r = open_decoder(vs, vc, can_hw ? hw : NULL, &p->vdec)) < 0)
        goto fail;
    p->hw = can_hw;
    if (p->ai >= 0 && (flags & LV_NO_AUDIO))
        p->ai = -1;
    if (p->ai >= 0)
    {
        AVStream *as = fmt->streams[p->ai];
        const AVCodec *ac = avcodec_find_decoder(as->codecpar->codec_id);
        if (ac && open_decoder(as, ac, NULL, &p->adec) >= 0)
            as->discard = AVDISCARD_DEFAULT;
        else
            p->ai = -1; // plays silent rather than not at all
    }
    if (p->ai < 0)
        p->a_done = 1;
    p->pkt = av_packet_alloc();
    if (p->pkt == NULL)
    {
        r = AVERROR(ENOMEM);
        goto fail;
    }
    av_buffer_unref(&hw);
    *out = p;
    return 0;
fail:
    av_buffer_unref(&hw);
    lv_close(p);
    return r;
}

EXPORT int lv_hw(const LvPlayer *p)
{
    return p ? p->hw : 0;
}

static long long to_us(LvPlayer *p, int stream, int64_t ts)
{
    return av_rescale_q(ts, p->in.fmt->streams[stream]->time_base, AV_TIME_BASE_Q) - p->start_us;
}

// A decoded picture into `out`; 1 when handed out, 0 when dropped (before an exact seek's target, or unusable).
static int video_out(LvPlayer *p, AVFrame *f, LvFrame *out)
{
    AVStream *vs = p->in.fmt->streams[p->vi];
    int64_t ts = f->best_effort_timestamp != AV_NOPTS_VALUE ? f->best_effort_timestamp : f->pts;
    long long dur = f->duration > 0 ? av_rescale_q(f->duration, vs->time_base, AV_TIME_BASE_Q) : 0;
    long long pts = ts != AV_NOPTS_VALUE ? to_us(p, p->vi, ts) : p->next_vpts;
    if (dur <= 0)
    {
        AVRational fr = vs->avg_frame_rate.num > 0 ? vs->avg_frame_rate : av_inv_q(vs->time_base);
        dur = fr.num > 0 && fr.den > 0 ? av_rescale(1000000, fr.den, fr.num) : 40000;
        if (dur > 1000000)
            dur = 40000;
    }
    p->next_vpts = pts + dur;
    if (p->skip_until_us != INT64_MIN)
    {
        long long target = p->skip_until_us;
        int first = p->seek_first;
        p->seek_first = 0;
        if (first && pts > target && p->seek_retries < 4)
        {
            // Landed past the target: go further back (1, 2, 4, 8 s) and decode forward again.
            p->seek_retries++;
            av_frame_free(&f);
            AVStream *s = p->in.fmt->streams[p->vi];
            long long back = target - (1000000LL << (p->seek_retries - 1));
            int64_t t = av_rescale_q_rnd((back > 0 ? back : 0) + p->start_us, AV_TIME_BASE_Q, s->time_base, AV_ROUND_DOWN);
            // Back to the start of an MPEG-TS/PS: by byte (a timestamp seek there can land a keyframe late even at
            // the start). Other containers seek by time (AVI's byte seek doesn't move its streams).
            int by_byte = back <= 0 && (p->in.fmt->iformat->flags & AVFMT_TS_DISCONT);
            int sought = by_byte ? avformat_seek_file(p->in.fmt, -1, INT64_MIN, 0, 0, AVSEEK_FLAG_BYTE)
                                 : av_seek_frame(p->in.fmt, p->vi, t, AVSEEK_FLAG_BACKWARD);
            if (back <= 0)
                p->seek_retries = 4; // nowhere further back to go
            if (sought >= 0)
            {
                avcodec_flush_buffers(p->vdec);
                if (p->adec)
                    avcodec_flush_buffers(p->adec);
                swr_free(&p->swr);
                vq_clear(p);
                p->demux_eof = p->v_done = 0;
                p->a_done = p->adec ? 0 : 1;
                p->seek_first = 1;
            }
            return 0;
        }
        if (pts <= target)
        {
            av_frame_free(&p->held); // a later one at or before the target
            p->held = f;
            return 0;
        }
        // Past the target: the one before it is the picture showing at the target.
        p->skip_until_us = INT64_MIN;
        if (p->held)
        {
            p->pending = f;
            f = p->held;
            p->held = NULL;
            ts = f->best_effort_timestamp != AV_NOPTS_VALUE ? f->best_effort_timestamp : f->pts;
            pts = ts != AV_NOPTS_VALUE ? to_us(p, p->vi, ts) : target;
            long long d2 = f->duration > 0 ? av_rescale_q(f->duration, vs->time_base, AV_TIME_BASE_Q) : dur;
            dur = d2 > 0 ? d2 : dur;
        }
    }
    memset(out, 0, sizeof *out);
    out->kind = LV_VIDEO;
    out->pts_us = pts;
    out->duration_us = dur;
    out->width = f->width;
    out->height = f->height;
    out->matrix = f->colorspace;
    out->full_range = is_full_range(f);
    out->transfer = f->color_trc;
    out->primaries = f->color_primaries;

    if (f->format == AV_PIX_FMT_D3D11 && f->hw_frames_ctx)
    {
        enum AVPixelFormat sw = ((AVHWFramesContext *)f->hw_frames_ctx->data)->sw_format;
        if (sw == AV_PIX_FMT_NV12 || sw == AV_PIX_FMT_P010)
        {
            out->layout = LV_TEXTURE;
            out->texture = f->data[0];
            out->texture_index = (int)(intptr_t)f->data[1];
            out->bit_depth = sw == AV_PIX_FMT_P010 ? 10 : 8;
            out->shift = sw == AV_PIX_FMT_P010 ? 6 : 0;
            out->chroma_w_shift = out->chroma_h_shift = 1;
            out->nplanes = 2;
            out->opaque = f;
            return 1;
        }
        // 4:2:2 / 4:4:4 surfaces: read back and take the CPU path.
        AVFrame *cpu = av_frame_alloc();
        if (cpu == NULL || av_hwframe_transfer_data(cpu, f, 0) < 0 || av_frame_copy_props(cpu, f) < 0)
        {
            av_frame_free(&cpu);
            av_frame_free(&f);
            return 0;
        }
        av_frame_free(&f);
        f = cpu;
    }

    const AVPixFmtDescriptor *d = av_pix_fmt_desc_get((enum AVPixelFormat)f->format);
    int ok = d && (d->flags & (AV_PIX_FMT_FLAG_RGB | AV_PIX_FMT_FLAG_BE | AV_PIX_FMT_FLAG_PAL | AV_PIX_FMT_FLAG_HWACCEL)) == 0 &&
             d->nb_components >= 3 && d->comp[0].depth <= 16;
    for (int i = 0; ok && i < d->nb_components; i++)
        if (d->comp[i].depth != d->comp[0].depth)
            ok = 0;
    if (ok)
    {
        int bytes = d->comp[0].depth > 8 ? 2 : 1;
        int separate = d->comp[0].plane == 0 && d->comp[1].plane == 1 && d->comp[2].plane == 2 &&
                       (d->nb_components == 3 || d->comp[3].plane == 3) && d->comp[0].step == bytes && d->comp[1].step == bytes;
        int semi = d->nb_components == 3 && d->comp[0].plane == 0 && d->comp[1].plane == 1 && d->comp[2].plane == 1 &&
                   d->comp[1].offset < d->comp[2].offset && d->comp[0].step == bytes && d->comp[1].step == 2 * bytes;
        if (separate || semi)
        {
            out->layout = LV_PLANAR;
            out->nplanes = semi ? 2 : d->nb_components;
            out->bit_depth = d->comp[0].depth;
            out->shift = d->comp[0].shift;
            out->chroma_w_shift = d->log2_chroma_w;
            out->chroma_h_shift = d->log2_chroma_h;
            for (int i = 0; i < out->nplanes; i++)
            {
                out->data[i] = f->data[i];
                out->linesize[i] = f->linesize[i];
            }
            out->opaque = f;
            return 1;
        }
    }

    // Anything else (RGB, packed 4:2:2, paletted...): straight RGBA.
    AVFrame *rgba = av_frame_alloc();
    if (rgba == NULL)
    {
        av_frame_free(&f);
        return 0;
    }
    rgba->format = AV_PIX_FMT_RGBA;
    rgba->width = f->width;
    rgba->height = f->height;
    p->sws = sws_getCachedContext(p->sws, f->width, f->height, (enum AVPixelFormat)f->format, f->width, f->height,
                                  AV_PIX_FMT_RGBA, SWS_BICUBIC, NULL, NULL, NULL);
    if (p->sws == NULL || av_frame_get_buffer(rgba, 0) < 0)
    {
        av_frame_free(&rgba);
        av_frame_free(&f);
        return 0;
    }
    sws_setColorspaceDetails(p->sws, sws_getCoefficients(sws_space(f->colorspace, f->height)), is_full_range(f),
                             sws_getCoefficients(SWS_CS_DEFAULT), 1, 0, 1 << 16, 1 << 16);
    sws_scale(p->sws, (const uint8_t *const *)f->data, f->linesize, 0, f->height, rgba->data, rgba->linesize);
    av_frame_free(&f);
    out->layout = LV_RGBA;
    out->bit_depth = 8;
    out->nplanes = 1;
    out->data[0] = rgba->data[0];
    out->linesize[0] = rgba->linesize[0];
    out->opaque = rgba;
    return 1;
}

// A decoded block of audio into `out` as float stereo at 48 kHz; 1 when handed out, 0 when there is nothing to
// hand out yet (the resampler holds it) or it was dropped.
static int audio_out(LvPlayer *p, AVFrame *f, LvFrame *out)
{
    int64_t ts = f->best_effort_timestamp != AV_NOPTS_VALUE ? f->best_effort_timestamp : f->pts;
    long long pts = ts != AV_NOPTS_VALUE ? to_us(p, p->ai, ts) : 0;
    long long dur = f->sample_rate > 0 ? (long long)f->nb_samples * 1000000 / f->sample_rate : 0;
    if (p->audio_skip && pts + dur <= p->seek_target)
    {
        av_frame_free(&f);
        return 0;
    }
    p->audio_skip = 0;
    if (p->swr == NULL)
    {
        AVChannelLayout stereo = AV_CHANNEL_LAYOUT_STEREO;
        if (swr_alloc_set_opts2(&p->swr, &stereo, AV_SAMPLE_FMT_FLT, LV_RATE, &f->ch_layout, (enum AVSampleFormat)f->format,
                                f->sample_rate, 0, NULL) < 0 ||
            swr_init(p->swr) < 0)
        {
            swr_free(&p->swr);
            av_frame_free(&f);
            p->a_done = 1;
            return 0;
        }
    }
    int cap = swr_get_out_samples(p->swr, f->nb_samples);
    if (cap > p->abuf_cap)
    {
        av_free(p->abuf);
        p->abuf = av_malloc((size_t)cap * 2 * sizeof(float));
        p->abuf_cap = p->abuf ? cap : 0;
    }
    uint8_t *dst = (uint8_t *)p->abuf;
    int n = p->abuf ? swr_convert(p->swr, &dst, cap, (const uint8_t **)f->extended_data, f->nb_samples) : 0;
    av_frame_free(&f);
    if (n <= 0)
        return 0;
    memset(out, 0, sizeof *out);
    out->kind = LV_AUDIO;
    out->pts_us = pts;
    out->duration_us = (long long)n * 1000000 / LV_RATE;
    out->samples = p->abuf;
    out->nb_samples = n;
    return 1;
}

// The next picture or block of audio in file order: 0 with `out` filled, AVERROR_EOF at the end of both streams.
EXPORT int lv_next(LvPlayer *p, LvFrame *out)
{
    for (;;)
    {
        if (p->pending)
        {
            AVFrame *f = p->pending;
            p->pending = NULL;
            if (video_out(p, f, out))
                return 0;
            continue;
        }
        if (p->v_done == 0)
        {
            AVFrame *f = av_frame_alloc();
            if (f == NULL)
                return AVERROR(ENOMEM);
            int r = avcodec_receive_frame(p->vdec, f);
            if (r == 0)
            {
                if (video_out(p, f, out))
                    return 0;
                continue;
            }
            av_frame_free(&f);
            if (r == AVERROR_EOF)
            {
                p->v_done = 1;
                if (p->held)
                {
                    AVFrame *h = p->held;
                    p->held = NULL;
                    p->skip_until_us = INT64_MIN;
                    if (video_out(p, h, out))
                        return 0;
                }
            }
            else if (r != AVERROR(EAGAIN))
                return r;
        }
        if (p->a_done == 0)
        {
            AVFrame *f = av_frame_alloc();
            if (f == NULL)
                return AVERROR(ENOMEM);
            int r = avcodec_receive_frame(p->adec, f);
            if (r == 0)
            {
                if (audio_out(p, f, out))
                    return 0;
                continue;
            }
            av_frame_free(&f);
            if (r != AVERROR(EAGAIN))
                p->a_done = 1; // the end, or a broken audio stream (which shouldn't stop the picture)
        }
        if (p->v_done && p->a_done)
            return AVERROR_EOF;
        if (p->vq_n > 0)
        {
            AVPacket *k = vq_pop(p);
            if (p->v_done == 0)
                avcodec_send_packet(p->vdec, k);
            av_packet_free(&k);
            continue;
        }
        if (p->v_flush)
        {
            p->v_flush = 0;
            avcodec_send_packet(p->vdec, NULL);
            continue;
        }
        if (p->demux_eof)
        {
            // Both decoders were flushed and neither gave a frame: a drained decoder saying EAGAIN has no more.
            p->v_done = p->a_done = 1;
            continue;
        }
        int r = av_read_frame(p->in.fmt, p->pkt);
        if (r == AVERROR_EOF || r == AVERROR(EIO))
        {
            p->demux_eof = 1;
            avcodec_send_packet(p->vdec, NULL);
            if (p->adec && p->a_done == 0)
                avcodec_send_packet(p->adec, NULL);
            continue;
        }
        if (r < 0)
            return r;
        if (p->pkt->stream_index == p->vi && p->v_done == 0)
            avcodec_send_packet(p->vdec, p->pkt);
        else if (p->pkt->stream_index == p->ai && p->a_done == 0)
            avcodec_send_packet(p->adec, p->pkt);
        av_packet_unref(p->pkt);
    }
}

// Jumps to `us`: to the keyframe at or before it, or with `exact` to the picture showing at `us` (decoding from
// that keyframe and dropping what comes before).
EXPORT int lv_seek(LvPlayer *p, long long us, int exact)
{
    AVStream *vs = p->in.fmt->streams[p->vi];
    // Rounded down: rounded to the nearest, a time in the second half of a frame named the next one, which may be a
    // keyframe past the target.
    int64_t ts = av_rescale_q_rnd(us + p->start_us, AV_TIME_BASE_Q, vs->time_base, AV_ROUND_DOWN);
    int r = avformat_seek_file(p->in.fmt, p->vi, INT64_MIN, ts, ts, 0);
    if (r < 0)
        r = av_seek_frame(p->in.fmt, p->vi, ts, AVSEEK_FLAG_BACKWARD);
    if (r < 0)
        return r;
    avcodec_flush_buffers(p->vdec);
    if (p->adec)
        avcodec_flush_buffers(p->adec);
    swr_free(&p->swr);
    vq_clear(p);
    p->demux_eof = p->v_done = 0;
    p->a_done = p->adec ? 0 : 1;
    p->next_vpts = us;
    p->skip_until_us = exact ? us : INT64_MIN;
    av_frame_free(&p->held);
    av_frame_free(&p->pending);
    p->seek_first = 1;
    p->seek_retries = 0;
    p->seek_target = us;
    p->audio_skip = exact;
    return 0;
}

EXPORT void lv_frame_free(LvFrame *f)
{
    if (f && f->opaque)
    {
        AVFrame *a = f->opaque;
        av_frame_free(&a);
        f->opaque = NULL;
    }
}

// ---- playback speed with sound ----------------------------------------------------------------------------------
// The atempo filter over the player's own audio (48 kHz float stereo): the same pitch at another speed. Output
// sample n stands for input sample n * tempo (atempo keeps them aligned), which is how Looker keeps its clock.

typedef struct LvTempo
{
    AVFilterGraph *graph;
    AVFilterContext *src, *sink;
    AVFrame *frame;
    float *out;
    int out_cap;
    long long pts;
} LvTempo;

EXPORT void lv_tempo_close(LvTempo *t)
{
    if (t == NULL)
        return;
    avfilter_graph_free(&t->graph);
    av_frame_free(&t->frame);
    av_free(t->out);
    free(t);
}

// `tempo`: 0.25 to 4 (two atempo stages below 0.5).
EXPORT int lv_tempo_open(double tempo, LvTempo **out)
{
    *out = NULL;
    LvTempo *t = calloc(1, sizeof *t);
    if (t == NULL)
        return AVERROR(ENOMEM);
    int r = AVERROR(ENOMEM);
    t->graph = avfilter_graph_alloc();
    t->frame = av_frame_alloc();
    if (t->graph == NULL || t->frame == NULL)
        goto fail;
    char args[160];
    snprintf(args, sizeof args, "time_base=1/%d:sample_rate=%d:sample_fmt=flt:channel_layout=stereo", LV_RATE, LV_RATE);
    if ((r = avfilter_graph_create_filter(&t->src, avfilter_get_by_name("abuffer"), "in", args, NULL, t->graph)) < 0)
        goto fail;
    if ((r = avfilter_graph_create_filter(&t->sink, avfilter_get_by_name("abuffersink"), "out", NULL, NULL, t->graph)) < 0)
        goto fail;
    AVFilterContext *prev = t->src;
    double left = tempo;
    for (int i = 0; i < 3; i++)
    {
        double step = left < 0.5 ? 0.5 : left;
        if (i == 2)
            step = left;
        AVFilterContext *f = NULL;
        char name[16], a[32];
        snprintf(name, sizeof name, "tempo%d", i);
        snprintf(a, sizeof a, "tempo=%.6f", step);
        if ((r = avfilter_graph_create_filter(&f, avfilter_get_by_name("atempo"), name, a, NULL, t->graph)) < 0)
            goto fail;
        if ((r = avfilter_link(prev, 0, f, 0)) < 0)
            goto fail;
        prev = f;
        left /= step;
        if (fabs(left - 1.0) < 1e-6)
            break;
    }
    AVFilterContext *fmtc = NULL;
    if ((r = avfilter_graph_create_filter(&fmtc, avfilter_get_by_name("aformat"), "fmt", "sample_fmts=flt:channel_layouts=stereo", NULL, t->graph)) < 0)
        goto fail;
    if ((r = avfilter_link(prev, 0, fmtc, 0)) < 0 || (r = avfilter_link(fmtc, 0, t->sink, 0)) < 0)
        goto fail;
    if ((r = avfilter_graph_config(t->graph, NULL)) < 0)
        goto fail;
    *out = t;
    return 0;
fail:
    lv_tempo_close(t);
    return r;
}

// Pushes `n` frames (interleaved stereo) and returns how many came out (or a negative error); `*out` points at
// them until the next call.
EXPORT int lv_tempo_process(LvTempo *t, const float *in, int n, const float **out)
{
    *out = NULL;
    if (n > 0)
    {
        AVFrame *f = av_frame_alloc();
        if (f == NULL)
            return AVERROR(ENOMEM);
        f->format = AV_SAMPLE_FMT_FLT;
        f->sample_rate = LV_RATE;
        f->nb_samples = n;
        av_channel_layout_default(&f->ch_layout, 2);
        f->pts = t->pts;
        t->pts += n;
        int r = av_frame_get_buffer(f, 0);
        if (r >= 0)
        {
            memcpy(f->data[0], in, (size_t)n * 2 * sizeof(float));
            r = av_buffersrc_add_frame_flags(t->src, f, AV_BUFFERSRC_FLAG_KEEP_REF);
        }
        av_frame_free(&f);
        if (r < 0)
            return r;
    }
    int got = 0;
    for (;;)
    {
        int r = av_buffersink_get_frame(t->sink, t->frame);
        if (r == AVERROR(EAGAIN) || r == AVERROR_EOF)
            break;
        if (r < 0)
            return r;
        int m = t->frame->nb_samples;
        if (got + m > t->out_cap)
        {
            int cap = (got + m) * 2;
            float *b = av_realloc(t->out, (size_t)cap * 2 * sizeof(float));
            if (b == NULL)
            {
                av_frame_unref(t->frame);
                return AVERROR(ENOMEM);
            }
            t->out = b;
            t->out_cap = cap;
        }
        memcpy(t->out + (size_t)got * 2, t->frame->data[0], (size_t)m * 2 * sizeof(float));
        got += m;
        av_frame_unref(t->frame);
    }
    *out = t->out;
    return got;
}

// A CPU picture from lv_next (not a texture) as straight RGBA fitted into box_w x box_h and turned `rotation`
// degrees, in a buffer freed by looker_video_free: the seek bar's previews.
EXPORT int lv_frame_rgba(const LvFrame *f, int rotation, int box_w, int box_h, int *w, int *h, unsigned char **rgba)
{
    *rgba = NULL;
    *w = *h = 0;
    const AVFrame *a = f ? (const AVFrame *)f->opaque : NULL;
    if (a == NULL || f->layout == LV_TEXTURE)
        return AVERROR(EINVAL);
    return fit_rgba(a, a->sample_aspect_ratio, rotation, box_w, box_h, SWS_BILINEAR, w, h, rgba);
}

// The next block of audio only, reading past video packets (kept for lv_next, undecoded) to reach it: for when
// the pictures queued are plenty but the sound is running dry (files that store their sound late). 0 with `out`
// filled, AVERROR(EAGAIN) when it can't read further ahead, AVERROR_EOF when the sound has ended.
EXPORT int lv_next_audio(LvPlayer *p, LvFrame *out)
{
    for (;;)
    {
        if (p->a_done)
            return AVERROR_EOF;
        AVFrame *f = av_frame_alloc();
        if (f == NULL)
            return AVERROR(ENOMEM);
        int r = avcodec_receive_frame(p->adec, f);
        if (r == 0)
        {
            if (audio_out(p, f, out))
                return 0;
            continue;
        }
        av_frame_free(&f);
        if (r != AVERROR(EAGAIN))
        {
            p->a_done = 1;
            return AVERROR_EOF;
        }
        if (p->demux_eof)
        {
            p->a_done = 1; // flushed and drained
            return AVERROR_EOF;
        }
        if (p->vq_n >= VQ_MAX_PACKETS || p->vq_bytes >= VQ_MAX_BYTES)
            return AVERROR(EAGAIN);
        r = av_read_frame(p->in.fmt, p->pkt);
        if (r == AVERROR_EOF || r == AVERROR(EIO))
        {
            p->demux_eof = 1;
            p->v_flush = 1;
            avcodec_send_packet(p->adec, NULL);
            continue;
        }
        if (r < 0)
            return r;
        if (p->pkt->stream_index == p->vi && p->v_done == 0)
        {
            if ((r = vq_push(p, p->pkt)) < 0)
            {
                av_packet_unref(p->pkt);
                return r;
            }
        }
        else if (p->pkt->stream_index == p->ai)
            avcodec_send_packet(p->adec, p->pkt);
        av_packet_unref(p->pkt);
    }
}
