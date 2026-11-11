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

#include <ass/ass.h>

#include <limits.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define EXPORT __declspec(dllexport)

// Bumped whenever a signature or LvInfo changes; Looker refuses a DLL whose version differs.
#define LOOKER_VIDEO_API 12

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

// One of a file's audio, video or subtitle streams, for the track menus (lv_audio_tracks, lv_video_tracks,
// lv_sub_tracks).
typedef struct LvTrack
{
    int stream;       // its stream index: what lv_open_streams takes
    int channels;
    int is_default;   // the container's default track
    int playing;      // the one this player plays
    char lang[16];    // its language tag ("eng"), or ""
    char title[64];   // its title, or ""
    char codec[32];   // FFmpeg's short codec name
    int width;        // a video track's picture size (0 for audio)
    int height;
    int flags;        // LV_TRACK_*: a subtitle track's kind and dispositions
} LvTrack;

enum
{
    LV_TRACK_FORCED = 1,           // the container's forced flag: shown even with subtitles off
    LV_TRACK_HEARING_IMPAIRED = 2, // SDH / captions
    LV_TRACK_TEXT = 4,             // plain text (SubRip, WebVTT, mov_text...): Looker lays it out itself
    LV_TRACK_ASS = 8,              // ASS/SSA: drawn by libass
    LV_TRACK_BITMAP = 16,          // pictures (PGS, VobSub, DVB)
};

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

// Raw elementary streams (.264, .hevc, .m1v, .obu, .thd, .mlp...) carry no timestamps, and FFmpeg gives them no
// duration: count the packets (all of them in a small file, else the first 4 MB's and scale by the file size), then
// rewind. Without a duration the player shows no length and doesn't loop. For video, a duration guessed from the bit
// rate is no better: MPEG-1's sequence header says 104 Mbit/s for "variable", which made a 2 s .m1v 9 ms long.
// (Constant-rate audio, AC-3 and DTS, is guessed right.)
static void raw_duration(AVFormatContext *fmt)
{
    if (!fmt->iformat || !(fmt->iformat->flags & AVFMT_NOTIMESTAMPS) || !fmt->pb)
        return;
    int si = av_find_best_stream(fmt, AVMEDIA_TYPE_VIDEO, -1, -1, NULL, 0);
    int video = si >= 0;
    if (!video)
        si = av_find_best_stream(fmt, AVMEDIA_TYPE_AUDIO, -1, -1, NULL, 0);
    if (si < 0)
        return;
    AVStream *st = fmt->streams[si];
    int guessed = video && fmt->duration_estimation_method == AVFMT_DURATION_FROM_BITRATE;
    if ((fmt->duration > 0 || st->duration > 0) && !guessed)
        return;
    AVRational fps = st->avg_frame_rate.num > 0 ? st->avg_frame_rate : st->r_frame_rate;
    if (video && (fps.num <= 0 || fps.den <= 0))
        return;
    AVPacket *pkt = av_packet_alloc();
    if (!pkt)
        return;
    // Video counts frames; audio adds up the packets' durations (the parser sets them), in the stream's time base.
    int64_t units = 0, bytes = 0, size = avio_size(fmt->pb);
    int eof = 0;
    while (bytes < (4 << 20))
    {
        if (av_read_frame(fmt, pkt) < 0)
        {
            eof = 1;
            break;
        }
        if (pkt->stream_index == si)
        {
            units += video ? 1 : (pkt->duration > 0 ? pkt->duration : 0);
            bytes += pkt->size;
        }
        av_packet_unref(pkt);
    }
    av_packet_free(&pkt);
    if (!eof && bytes > 0 && size > bytes)
        units = (int64_t)((double)units * size / bytes);
    if (units > 0)
    {
        fmt->duration = video ? av_rescale(units, (int64_t)fps.den * AV_TIME_BASE, fps.num)
                              : av_rescale_q(units, st->time_base, AV_TIME_BASE_Q);
        st->duration = av_rescale_q(fmt->duration, AV_TIME_BASE_Q, st->time_base);
        fmt->duration_estimation_method = AVFMT_DURATION_FROM_STREAM; // counted: not again
    }
    if (av_seek_frame(fmt, si, st->start_time != AV_NOPTS_VALUE ? st->start_time : 0, AVSEEK_FLAG_BACKWARD) < 0)
    {
        avio_seek(fmt->pb, 0, SEEK_SET);
        avformat_flush(fmt);
    }
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
    if (!full)
        return 0;
    r = avformat_find_stream_info(in->fmt, NULL);
    if (r >= 0)
        raw_duration(in->fmt);
    return r;
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
    // The DV demuxer reports its time base (60000/1) as the average rate: past 1000 fps, trust the real rate.
    if (fps.den > 0 && fps.num > 1000LL * fps.den && st->r_frame_rate.num > 0 && st->r_frame_rate.den > 0 &&
        st->r_frame_rate.num <= 1000LL * st->r_frame_rate.den)
        fps = st->r_frame_rate;
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

static int is_picture_stream(const AVFormatContext *fmt, int i)
{
    if (i < 0 || (unsigned)i >= fmt->nb_streams)
        return 0;
    const AVStream *s = fmt->streams[i];
    return s->codecpar->codec_type == AVMEDIA_TYPE_VIDEO && !(s->disposition & AV_DISPOSITION_ATTACHED_PIC);
}

// The video stream to show (`want` when it's one, else the best; never cover art) and the audio that goes with it.
static int pick_streams(AVFormatContext *fmt, int want, int *video, int *audio)
{
    int v = -1;
    for (unsigned i = 0; i < fmt->nb_streams; i++)
        fmt->streams[i]->discard = AVDISCARD_ALL;
    int best = av_find_best_stream(fmt, AVMEDIA_TYPE_VIDEO, -1, -1, NULL, 0);
    if (is_picture_stream(fmt, want))
        v = want;
    else if (best >= 0 && !(fmt->streams[best]->disposition & AV_DISPOSITION_ATTACHED_PIC))
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
        r = pick_streams(in.fmt, -1, &vi, &ai);
    if (r >= 0)
        fill_info(in.fmt, in.fmt->streams[vi], ai >= 0 ? in.fmt->streams[ai] : NULL, info);
    input_close(&in);
    return r < 0 ? r : 0;
}

// ---- HDR as SDR -----------------------------------------------------------------------------------------------
// A PQ or HLG picture made into an SDR one (the still shown before playback, thumbnails, the seek bar's previews):
// linear light with SDR white (BT.2408's 203 nits) at 1, Rec. 709's primaries, the highlights rolled off into SDR
// white, sRGB-encoded. The same maths as the player's shader (Looker's src/yuv.rs) with a headroom of 1: below 0.75
// as it is, above it the brightest channel eased towards 1, the hue kept.

static int is_hdr_trc(int trc)
{
    return trc == AVCOL_TRC_SMPTE2084 || trc == AVCOL_TRC_ARIB_STD_B67;
}

// SMPTE ST 2084: a 0..1 signal to linear light, SDR white = 1.
static float pq_rel(float e)
{
    const float m1 = 2610.0f / 16384.0f, m2 = 2523.0f / 4096.0f * 128.0f;
    const float c1 = 3424.0f / 4096.0f, c2 = 2413.0f / 4096.0f * 32.0f, c3 = 2392.0f / 4096.0f * 32.0f;
    float p = powf(e, 1.0f / m2);
    return 10000.0f / 203.0f * powf(fmaxf(p - c1, 0.0f) / (c2 - c3 * p), 1.0f / m1);
}

// BT.2100 HLG: a 0..1 signal to scene light 0..1 (the inverse OETF).
static float hlg_scene(float e)
{
    const float a = 0.17883277f, b = 0.28466892f, c = 0.55991073f;
    return e <= 0.5f ? e * e / 3.0f : (expf((e - c) / a) + b) / 12.0f;
}

// Full-range RGBA, 16 bits a channel, PQ/HLG-encoded in `primaries` (unspecified = BT.2020), to straight 8-bit sRGB
// RGBA in Rec. 709. -1 when out of memory.
static int hdr_to_sdr(const uint16_t *src, unsigned char *dst, size_t n, int trc, int primaries)
{
    static const float BT2020_TO_709[9] = {1.6605f, -0.5876f, -0.0728f, -0.1246f, 1.1329f,
                                           -0.0083f, -0.0182f, -0.1006f, 1.1187f};
    static const float P3_TO_709[9] = {1.2249f, -0.2247f, 0.0f, -0.0420f, 1.0419f, 0.0f, -0.0197f, -0.0786f, 1.0979f};
    const float *m = primaries == AVCOL_PRI_BT709                                        ? NULL
                     : primaries == AVCOL_PRI_SMPTE431 || primaries == AVCOL_PRI_SMPTE432 ? P3_TO_709
                                                                                          : BT2020_TO_709;
    const int hlg = trc == AVCOL_TRC_ARIB_STD_B67;
    // 12-bit tables: signal to linear light (scene light for HLG), and linear 0..1 to sRGB's 8 bits.
    float *lin = malloc(4096 * sizeof(float));
    unsigned char *enc = malloc(4096);
    if (!lin || !enc)
    {
        free(lin);
        free(enc);
        return -1;
    }
    for (int i = 0; i < 4096; i++)
    {
        float e = i / 4095.0f;
        lin[i] = hlg ? hlg_scene(e) : pq_rel(e);
        float c = e <= 0.0031308f ? e * 12.92f : 1.055f * powf(e, 1.0f / 2.4f) - 0.055f;
        enc[i] = (unsigned char)lrintf(c * 255.0f);
    }
    const float knee = 0.75f, room = 0.25f;
    for (size_t i = 0; i < n; i++)
    {
        const uint16_t *s = src + i * 4;
        float r = lin[s[0] >> 4], g = lin[s[1] >> 4], b = lin[s[2] >> 4];
        if (hlg)
        {
            // The OOTF of BT.2100's nominal 1000-nit display (system gamma 1.2), in SDR whites.
            float y = 0.2627f * r + 0.6780f * g + 0.0593f * b;
            float k = 1000.0f / 203.0f * powf(fmaxf(y, 1e-6f), 0.2f);
            r *= k;
            g *= k;
            b *= k;
        }
        if (m)
        {
            float r2 = m[0] * r + m[1] * g + m[2] * b;
            float g2 = m[3] * r + m[4] * g + m[5] * b;
            float b2 = m[6] * r + m[7] * g + m[8] * b;
            r = r2;
            g = g2;
            b = b2;
        }
        float mx = fmaxf(r, fmaxf(g, b));
        if (mx > knee)
        {
            float k = (knee + room * (1.0f - expf(-(mx - knee) / room))) / mx;
            r *= k;
            g *= k;
            b *= k;
        }
#define ENC(v) enc[(int)(fminf(fmaxf((v), 0.0f), 1.0f) * 4095.0f + 0.5f)]
        unsigned char *d = dst + i * 4;
        d[0] = ENC(r);
        d[1] = ENC(g);
        d[2] = ENC(b);
        d[3] = (unsigned char)(s[3] >> 8);
#undef ENC
    }
    free(lin);
    free(enc);
    return 0;
}

// A picture square-pixelled, fitted into box_w x box_h (0 x 0 = full size; never enlarged) and turned upright, as
// straight RGBA in a buffer freed by looker_video_free. `trc` and `primaries` are the stream's (AVCOL_*): PQ and
// HLG pictures are made SDR (hdr_to_sdr).
static int fit_rgba(const AVFrame *frame, AVRational sar, int rot, int box_w, int box_h, int sws_flags, int trc,
                    int primaries, int *out_w, int *out_h, unsigned char **out_rgba)
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
    const int hdr = is_hdr_trc(trc);
    struct SwsContext *sws = sws_getContext(fw, fh, (enum AVPixelFormat)frame->format, sw, sh,
                                            hdr ? AV_PIX_FMT_RGBA64LE : AV_PIX_FMT_RGBA, sws_flags, NULL, NULL, NULL);
    if (!sws)
        return AVERROR(EINVAL);
    // HDR without a matrix tag is BT.2020's.
    enum AVColorSpace space =
        hdr && frame->colorspace == AVCOL_SPC_UNSPECIFIED ? AVCOL_SPC_BT2020_NCL : frame->colorspace;
    const int *coeffs = sws_getCoefficients(sws_space(space, fh));
    // Fails (harmlessly) for RGB sources.
    sws_setColorspaceDetails(sws, coeffs, is_full_range(frame), sws_getCoefficients(SWS_CS_DEFAULT), 1, 0, 1 << 16, 1 << 16);
    unsigned char *rgba = malloc((size_t)sw * sh * 4);
    uint16_t *deep = hdr ? malloc((size_t)sw * sh * 8) : NULL;
    int r = 0;
    if (!rgba || (hdr && !deep))
        r = AVERROR(ENOMEM);
    else
    {
        uint8_t *dst[4] = {hdr ? (uint8_t *)deep : rgba, NULL, NULL, NULL};
        int dst_stride[4] = {sw * (hdr ? 8 : 4), 0, 0, 0};
        r = sws_scale(sws, (const uint8_t *const *)frame->data, frame->linesize, 0, fh, dst, dst_stride);
        r = r > 0 ? 0 : r < 0 ? r : AVERROR(EINVAL);
        if (r == 0 && hdr && hdr_to_sdr(deep, rgba, (size_t)sw * sh, trc, primaries) < 0)
            r = AVERROR(ENOMEM);
    }
    free(deep);
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
    r = pick_streams(in.fmt, -1, &vi, &ai);
    if (r < 0 || in.fmt->streams[vi]->codecpar->width <= 0 || in.fmt->streams[vi]->codecpar->codec_id == AV_CODEC_ID_NONE)
    {
        // Headers that don't say (MPEG-TS/PS, some AVIs): probe properly.
        for (unsigned i = 0; i < in.fmt->nb_streams; i++)
            in.fmt->streams[i]->discard = AVDISCARD_DEFAULT;
        if ((r = avformat_find_stream_info(in.fmt, NULL)) < 0 || (r = pick_streams(in.fmt, -1, &vi, &ai)) < 0)
            goto done;
    }
    AVStream *st = in.fmt->streams[vi];
    st->discard = AVDISCARD_DEFAULT;
    raw_duration(in.fmt);
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
                 info->rotation, box_w, box_h, SWS_BICUBIC | SWS_ACCURATE_RND | SWS_FULL_CHR_H_INT, info->transfer,
                 info->primaries, out_w, out_h, out_rgba);

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
    LV_NO_AUDIO = 1, // lv_open: pictures only
    LV_KEYFRAMES = 2, // lv_open: keyframes only, each handed out as soon as it's decoded (the seek bar's previews)
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
    volatile LONG interrupt; // lv_interrupt: the decode in progress gives up (a newer seek is waiting)
    int keyframes;           // LV_KEYFRAMES: pictures go to the CPU even from the GPU (lv_frame_rgba fits them)
    long long *frame_us;     // every picture's time from the container's index, sorted (lv_frame_times)
    int n_frames;            // 0 when the container doesn't list every picture (Matroska: keyframes only)
    LvTrack *tracks;         // the file's audio streams (lv_audio_tracks)
    int n_tracks;
    LvTrack *vtracks;        // its video streams, cover art left out (lv_video_tracks)
    int n_vtracks;
    LvTrack *stracks;        // its subtitle streams (lv_sub_tracks)
    int n_stracks;
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

// `keyframes`: only keyframes are decoded (the rest are dropped unread by the decoder), on slice threads so each
// comes out as soon as it's decoded.
static int open_decoder(AVStream *st, const AVCodec *codec, AVBufferRef *hw, int keyframes, AVCodecContext **out)
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
        // Frame threads hold a picture back until a picture per thread is in flight (~16): after every seek, 16
        // pictures were decoded for the one shown (4K ProRes: 115 ms a seek). Codecs whose every picture stands
        // alone (ProRes, DNx, MJPEG) and that split a picture across threads decode one picture at a time on all
        // of them instead: the picture after a seek is the first decoded, and playback keeps up.
        const AVCodecDescriptor *d = avcodec_descriptor_get(codec->id);
        int intra = d && (d->props & AV_CODEC_PROP_INTRA_ONLY);
        if ((keyframes || intra) && (codec->capabilities & AV_CODEC_CAP_SLICE_THREADS))
            c->thread_type = FF_THREAD_SLICE;
        else if (keyframes)
            c->thread_count = 1; // no slice threads: one picture at a time all the same
        if (keyframes)
            c->skip_frame = AVDISCARD_NONKEY;
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
    free(p->frame_us);
    free(p->tracks);
    free(p->vtracks);
    free(p->stracks);
    free(p);
}

// The file's audio streams, the one playing marked (before the decoder is opened: `ai` is the pick).
static void list_tracks(LvPlayer *p)
{
    AVFormatContext *fmt = p->in.fmt;
    int n = 0;
    for (unsigned i = 0; i < fmt->nb_streams; i++)
        n += fmt->streams[i]->codecpar->codec_type == AVMEDIA_TYPE_AUDIO;
    if (n == 0 || (p->tracks = calloc((size_t)n, sizeof *p->tracks)) == NULL)
        return;
    for (unsigned i = 0; i < fmt->nb_streams; i++)
    {
        const AVStream *s = fmt->streams[i];
        if (s->codecpar->codec_type != AVMEDIA_TYPE_AUDIO)
            continue;
        LvTrack *t = &p->tracks[p->n_tracks++];
        t->stream = (int)i;
        t->channels = s->codecpar->ch_layout.nb_channels;
        t->is_default = (s->disposition & AV_DISPOSITION_DEFAULT) != 0;
        t->playing = (int)i == p->ai;
        const AVDictionaryEntry *e = av_dict_get(s->metadata, "language", NULL, 0);
        if (e && strcmp(e->value, "und") != 0)
            snprintf(t->lang, sizeof t->lang, "%s", e->value);
        e = av_dict_get(s->metadata, "title", NULL, 0);
        if (e == NULL)
            e = av_dict_get(s->metadata, "handler_name", NULL, 0);
        // MP4's handler names are often the muxer's boilerplate: those aren't titles.
        if (e && strstr(e->value, "Handler") == NULL && strstr(e->value, "handler") == NULL)
            snprintf(t->title, sizeof t->title, "%s", e->value);
        snprintf(t->codec, sizeof t->codec, "%s", avcodec_get_name(s->codecpar->codec_id));
    }
}

// The file's video streams, the one showing marked (cover art isn't one).
static void list_video_tracks(LvPlayer *p)
{
    AVFormatContext *fmt = p->in.fmt;
    int n = 0;
    for (unsigned i = 0; i < fmt->nb_streams; i++)
        n += is_picture_stream(fmt, (int)i);
    if (n == 0 || (p->vtracks = calloc((size_t)n, sizeof *p->vtracks)) == NULL)
        return;
    for (unsigned i = 0; i < fmt->nb_streams; i++)
    {
        if (!is_picture_stream(fmt, (int)i))
            continue;
        const AVStream *s = fmt->streams[i];
        LvTrack *t = &p->vtracks[p->n_vtracks++];
        t->stream = (int)i;
        t->width = s->codecpar->width;
        t->height = s->codecpar->height;
        t->is_default = (s->disposition & AV_DISPOSITION_DEFAULT) != 0;
        t->playing = (int)i == p->vi;
        const AVDictionaryEntry *e = av_dict_get(s->metadata, "language", NULL, 0);
        if (e && strcmp(e->value, "und") != 0)
            snprintf(t->lang, sizeof t->lang, "%s", e->value);
        e = av_dict_get(s->metadata, "title", NULL, 0);
        if (e == NULL)
            e = av_dict_get(s->metadata, "handler_name", NULL, 0);
        if (e && strstr(e->value, "Handler") == NULL && strstr(e->value, "handler") == NULL)
            snprintf(t->title, sizeof t->title, "%s", e->value);
        snprintf(t->codec, sizeof t->codec, "%s", avcodec_get_name(s->codecpar->codec_id));
    }
}

static int sub_kind(enum AVCodecID id)
{
    if (id == AV_CODEC_ID_ASS || id == AV_CODEC_ID_SSA)
        return LV_TRACK_ASS;
    const AVCodecDescriptor *d = avcodec_descriptor_get(id);
    return d && (d->props & AV_CODEC_PROP_BITMAP_SUB) ? LV_TRACK_BITMAP : LV_TRACK_TEXT;
}

// The file's subtitle streams that something here decodes (a stream with no decoder isn't offered).
static void list_sub_tracks(LvPlayer *p)
{
    AVFormatContext *fmt = p->in.fmt;
    int n = 0;
    for (unsigned i = 0; i < fmt->nb_streams; i++)
        n += fmt->streams[i]->codecpar->codec_type == AVMEDIA_TYPE_SUBTITLE;
    if (n == 0 || (p->stracks = calloc((size_t)n, sizeof *p->stracks)) == NULL)
        return;
    for (unsigned i = 0; i < fmt->nb_streams; i++)
    {
        const AVStream *s = fmt->streams[i];
        if (s->codecpar->codec_type != AVMEDIA_TYPE_SUBTITLE)
            continue;
        int kind = sub_kind(s->codecpar->codec_id);
        if (kind != LV_TRACK_ASS && avcodec_find_decoder(s->codecpar->codec_id) == NULL)
            continue;
        LvTrack *t = &p->stracks[p->n_stracks++];
        t->stream = (int)i;
        t->is_default = (s->disposition & AV_DISPOSITION_DEFAULT) != 0;
        t->flags = kind | ((s->disposition & AV_DISPOSITION_FORCED) ? LV_TRACK_FORCED : 0) |
                   ((s->disposition & AV_DISPOSITION_HEARING_IMPAIRED) ? LV_TRACK_HEARING_IMPAIRED : 0);
        const AVDictionaryEntry *e = av_dict_get(s->metadata, "language", NULL, 0);
        if (e && strcmp(e->value, "und") != 0)
            snprintf(t->lang, sizeof t->lang, "%s", e->value);
        e = av_dict_get(s->metadata, "title", NULL, 0);
        if (e == NULL)
            e = av_dict_get(s->metadata, "handler_name", NULL, 0);
        if (e && strstr(e->value, "Handler") == NULL && strstr(e->value, "handler") == NULL)
            snprintf(t->title, sizeof t->title, "%s", e->value);
        snprintf(t->codec, sizeof t->codec, "%s", avcodec_get_name(s->codecpar->codec_id));
    }
}

// The file's subtitle streams; valid until lv_close.
EXPORT int lv_sub_tracks(const LvPlayer *p, const LvTrack **out)
{
    *out = p ? p->stracks : NULL;
    return p ? p->n_stracks : 0;
}

// The zero of the player's clock: the video stream's start time in the file, in microseconds (lv_subs_open takes
// it so a subtitle stream's times line up with the pictures').
EXPORT long long lv_start_us(const LvPlayer *p)
{
    return p ? p->start_us : 0;
}

// The file's video streams (cover art left out); valid until lv_close.
EXPORT int lv_video_tracks(const LvPlayer *p, const LvTrack **out)
{
    *out = p ? p->vtracks : NULL;
    return p ? p->n_vtracks : 0;
}

// The file's audio streams (0 when it has none); valid until lv_close.
EXPORT int lv_audio_tracks(const LvPlayer *p, const LvTrack **out)
{
    *out = p ? p->tracks : NULL;
    return p ? p->n_tracks : 0;
}

static int cmp_ll(const void *a, const void *b)
{
    long long x = *(const long long *)a, y = *(const long long *)b;
    return (x > y) - (x < y);
}

// Every picture's time, from the index the demuxer built at open (MP4/MOV list each sample; AVI too): sorted, on
// the same clock as the pictures (0 = the first). Left empty when the index lists far fewer entries than the
// duration and frame rate imply (Matroska's cues are keyframes only): the frame count would be a guess then.
static void build_frame_table(LvPlayer *p, const LvInfo *info)
{
    AVStream *vs = p->in.fmt->streams[p->vi];
    int n = avformat_index_get_entries_count(vs);
    if (n <= 1)
        return;
    double expected = info->fps_num > 0 && info->fps_den > 0 ? info->duration_us / 1e6 * info->fps_num / info->fps_den : 0;
    if (expected > 0 && n < expected / 2)
        return;
    long long *t = malloc((size_t)n * sizeof *t);
    if (t == NULL)
        return;
    int k = 0;
    for (int i = 0; i < n; i++)
    {
        const AVIndexEntry *e = avformat_index_get_entry(vs, i);
        if (e == NULL || (e->flags & AVINDEX_DISCARD_FRAME) || e->timestamp == AV_NOPTS_VALUE)
            continue;
        t[k++] = e->timestamp;
    }
    if (k <= 1)
    {
        free(t);
        return;
    }
    // Sorted, then shifted so the first is 0: the entries are decode times (B-frames come out of order and a
    // constant reorder delay later), and a picture's number is its place in presentation order.
    qsort(t, (size_t)k, sizeof *t, cmp_ll);
    int64_t first = t[0];
    for (int i = 0; i < k; i++)
        t[i] = av_rescale_q(t[i] - first, vs->time_base, AV_TIME_BASE_Q);
    p->frame_us = t;
    p->n_frames = k;
}

// The table build_frame_table made at open: its length (0 when there's none), and the times in *out (owned by the
// player: valid until lv_close).
EXPORT int lv_frame_times(const LvPlayer *p, const long long **out)
{
    *out = p ? p->frame_us : NULL;
    return p ? p->n_frames : 0;
}

EXPORT int lv_open_streams(const wchar_t *path, void *d3d11, int flags, int video_stream, int audio_stream, LvInfo *info, LvPlayer **out);

// Opens `path` for playback. `d3d11` (an ID3D11Device made with D3D11_CREATE_DEVICE_VIDEO_SUPPORT, or NULL)
// decodes on that GPU where it can. `flags`: LV_NO_AUDIO, LV_KEYFRAMES.
EXPORT int lv_open(const wchar_t *path, void *d3d11, int flags, LvInfo *info, LvPlayer **out)
{
    return lv_open_streams(path, d3d11, flags, -1, -1, info, out);
}

// lv_open playing the audio stream `audio_stream` (an index lv_audio_tracks gave), or the best one when it's -1
// or not an audio stream.
EXPORT int lv_open_audio(const wchar_t *path, void *d3d11, int flags, int audio_stream, LvInfo *info, LvPlayer **out)
{
    return lv_open_streams(path, d3d11, flags, -1, audio_stream, info, out);
}

// lv_open_audio showing the video stream `video_stream` (an index lv_video_tracks gave), or the best one when
// it's -1 or not a video stream.
EXPORT int lv_open_streams(const wchar_t *path, void *d3d11, int flags, int video_stream, int audio_stream, LvInfo *info, LvPlayer **out)
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
    r = pick_streams(fmt, video_stream, &p->vi, &p->ai);
    LvInfo quick;
    if (r >= 0)
        fill_info(fmt, fmt->streams[p->vi], NULL, &quick);
    // The headers must give the picture's size, the audio's rate and the frame rate (ASF doesn't), else probe.
    if (r < 0 || fmt->streams[p->vi]->codecpar->width <= 0 || quick.fps_num <= 0 ||
        (p->ai >= 0 && fmt->streams[p->ai]->codecpar->sample_rate <= 0))
    {
        for (unsigned i = 0; i < fmt->nb_streams; i++)
            fmt->streams[i]->discard = AVDISCARD_DEFAULT;
        if ((r = avformat_find_stream_info(fmt, NULL)) < 0 || (r = pick_streams(fmt, video_stream, &p->vi, &p->ai)) < 0)
            goto fail;
    }
    AVStream *vs = fmt->streams[p->vi];
    vs->discard = AVDISCARD_DEFAULT;
    raw_duration(fmt);
    fill_info(fmt, vs, p->ai >= 0 ? fmt->streams[p->ai] : NULL, info);
    p->start_us = vs->start_time != AV_NOPTS_VALUE ? av_rescale_q(vs->start_time, vs->time_base, AV_TIME_BASE_Q) : 0;
    if (audio_stream >= 0 && (unsigned)audio_stream < fmt->nb_streams && fmt->streams[audio_stream]->codecpar->codec_type == AVMEDIA_TYPE_AUDIO)
    {
        p->ai = audio_stream;
        fill_info(fmt, vs, fmt->streams[p->ai], info);
    }
    list_tracks(p);
    list_video_tracks(p);
    list_sub_tracks(p);

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
    if ((r = open_decoder(vs, vc, can_hw ? hw : NULL, (flags & LV_KEYFRAMES) != 0, &p->vdec)) < 0)
        goto fail;
    p->hw = can_hw;
    p->keyframes = (flags & LV_KEYFRAMES) != 0;
    if (p->ai >= 0 && (flags & LV_NO_AUDIO))
        p->ai = -1;
    if (p->ai >= 0)
    {
        AVStream *as = fmt->streams[p->ai];
        const AVCodec *ac = avcodec_find_decoder(as->codecpar->codec_id);
        if (ac && open_decoder(as, ac, NULL, 0, &p->adec) >= 0)
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
    if (!p->keyframes)
        build_frame_table(p, info);
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
        if ((sw == AV_PIX_FMT_NV12 || sw == AV_PIX_FMT_P010) && !p->keyframes)
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
        // 4:2:2 / 4:4:4 surfaces, and previews (fitted on the CPU): read back and take the CPU path.
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
// Any thread: lv_next or lv_next_audio, running or next called, returns AVERROR_EXIT at its next step, until
// lv_seek. An exact seek decodes from the keyframe to its target inside lv_next (a long GOP: hundreds of pictures),
// and a newer seek no longer waits for that.
EXPORT void lv_interrupt(LvPlayer *p)
{
    if (p)
        InterlockedExchange(&p->interrupt, 1);
}

// Clears an lv_interrupt that came too late to matter (the seek it was for had already been taken): decoding goes
// on from where it is.
EXPORT void lv_resume(LvPlayer *p)
{
    if (p)
        InterlockedExchange(&p->interrupt, 0);
}

EXPORT int lv_next(LvPlayer *p, LvFrame *out)
{
    for (;;)
    {
        if (p->interrupt)
            return AVERROR_EXIT;
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
    // Cleared first: a seek that fails leaves the player usable.
    InterlockedExchange(&p->interrupt, 0);
    // Exact, ahead of everything decoded and close (a slow drag on the seek bar): no seek and no flush. The decoder
    // carries on from where it is and drops what comes before the target, so each small step forward costs only the
    // pictures it moves over, not a restart from the keyframe.
    if (exact && !p->v_done && !p->demux_eof && us >= p->next_vpts && us - p->next_vpts <= 2000000 &&
        (p->skip_until_us == INT64_MIN || us >= p->skip_until_us))
    {
        p->skip_until_us = us;
        p->seek_first = 0; // decoding on from before the target: nothing lands past it
        p->seek_target = us;
        p->audio_skip = 1;
        return 0;
    }
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
    return fit_rgba(a, a->sample_aspect_ratio, rotation, box_w, box_h, SWS_BILINEAR, a->color_trc, a->color_primaries,
                    w, h, rgba);
}

// The next block of audio only, reading past video packets (kept for lv_next, undecoded) to reach it: for when
// the pictures queued are plenty but the sound is running dry (files that store their sound late). 0 with `out`
// filled, AVERROR(EAGAIN) when it can't read further ahead, AVERROR_EOF when the sound has ended.
EXPORT int lv_next_audio(LvPlayer *p, LvFrame *out)
{
    for (;;)
    {
        if (p->interrupt)
            return AVERROR_EXIT;
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

// ---- audio files ----------------------------------------------------------------------------------------------
//
// The audio view's reader (Looker's imaging/audio.rs): one audio stream at its own rate and channel count, as
// interleaved float, each block placed in frames (one sample per channel) from the stream's start. la_seek is
// exact: the next block starts at the frame asked for. A reader belongs to one thread (the player and the waveform
// scan each open their own); la_interrupt may come from any.

typedef struct LaInfo
{
    long long duration_us; // 0 when unknown
    long long frames;      // from the duration: an estimate until a scan has counted them
    long long bit_rate;    // bits per second, 0 when unknown
    int rate, channels;
    int bits;      // bits per sample as stored (16, 24...), 0 for lossy codecs
    int lossless;  // the codec is lossless (FLAC, ALAC, PCM, WavPack, APE...)
    int has_cover; // an attached picture: la_cover
    int stream;    // the stream read
    char codec[32];     // FFmpeg's short codec name: "mp3", "flac", "aac"...
    char profile[32];   // "LC", "HE-AAC"... or ""
    char layout[48];    // "mono", "stereo", "5.1(side)"...
    char container[48]; // the demuxer: "mp3", "mov,mp4,m4a,3gp,3g2,mj2", "ogg"...
} LaInfo;

typedef struct LaReader
{
    Input in;
    int ai;
    AVCodecContext *dec;
    SwrContext *swr;
    AVPacket *pkt;
    AVFrame *frame;
    float *buf;
    int buf_cap;
    int rate, channels;
    int lossless;
    int64_t start;  // the stream's first timestamp, in its time base
    long long pos;  // the frame the next block starts at
    long long want; // after a seek: what comes before this frame is dropped (-1: nothing)
    int eof;
    volatile LONG interrupt;
} LaReader;

EXPORT void la_close(LaReader *r)
{
    if (!r)
        return;
    av_free(r->buf);
    swr_free(&r->swr);
    av_frame_free(&r->frame);
    av_packet_free(&r->pkt);
    avcodec_free_context(&r->dec);
    input_close(&r->in);
    free(r);
}

// Opens `path`'s audio stream `stream` (-1: the best one).
EXPORT int la_open(const wchar_t *path, int stream, LaInfo *info, LaReader **out)
{
    *out = NULL;
    memset(info, 0, sizeof *info);
    LaReader *r = calloc(1, sizeof *r);
    if (!r)
        return AVERROR(ENOMEM);
    int e = input_open(&r->in, path, 1);
    if (e < 0)
    {
        la_close(r);
        return e;
    }
    AVFormatContext *fmt = r->in.fmt;
    int ai = -1;
    if (stream >= 0 && (unsigned)stream < fmt->nb_streams && fmt->streams[stream]->codecpar->codec_type == AVMEDIA_TYPE_AUDIO)
        ai = stream;
    else
        ai = av_find_best_stream(fmt, AVMEDIA_TYPE_AUDIO, -1, -1, NULL, 0);
    if (ai < 0)
    {
        la_close(r);
        return AVERROR_STREAM_NOT_FOUND;
    }
    r->ai = ai;
    for (unsigned i = 0; i < fmt->nb_streams; i++)
        fmt->streams[i]->discard = (int)i == ai ? AVDISCARD_DEFAULT : AVDISCARD_ALL;
    AVStream *st = fmt->streams[ai];
    const AVCodec *codec = avcodec_find_decoder(st->codecpar->codec_id);
    if (!codec)
    {
        la_close(r);
        return AVERROR_DECODER_NOT_FOUND;
    }
    r->dec = avcodec_alloc_context3(codec);
    r->pkt = av_packet_alloc();
    r->frame = av_frame_alloc();
    if (!r->dec || !r->pkt || !r->frame)
    {
        la_close(r);
        return AVERROR(ENOMEM);
    }
    if ((e = avcodec_parameters_to_context(r->dec, st->codecpar)) < 0)
    {
        la_close(r);
        return e;
    }
    r->dec->pkt_timebase = st->time_base;
    if ((e = avcodec_open2(r->dec, codec, NULL)) < 0)
    {
        la_close(r);
        return e;
    }
    r->rate = r->dec->sample_rate > 0 ? r->dec->sample_rate : st->codecpar->sample_rate;
    r->channels = r->dec->ch_layout.nb_channels > 0 ? r->dec->ch_layout.nb_channels : st->codecpar->ch_layout.nb_channels;
    if (r->rate <= 0 || r->channels <= 0)
    {
        la_close(r);
        return AVERROR_INVALIDDATA;
    }
    r->start = st->start_time != AV_NOPTS_VALUE ? st->start_time : 0;
    r->want = -1;

    const AVCodecParameters *p = st->codecpar;
    info->rate = r->rate;
    info->channels = r->channels;
    info->stream = ai;
    if (fmt->duration > 0)
        info->duration_us = fmt->duration;
    else if (st->duration > 0)
        info->duration_us = av_rescale_q(st->duration, st->time_base, AV_TIME_BASE_Q);
    if (st->duration > 0)
        info->frames = av_rescale_q(st->duration, st->time_base, (AVRational){1, r->rate});
    else
        info->frames = info->duration_us * r->rate / 1000000;
    info->bit_rate = p->bit_rate > 0 ? p->bit_rate : fmt->bit_rate;
    const AVCodecDescriptor *cd = avcodec_descriptor_get(p->codec_id);
    info->lossless = r->lossless = cd && (cd->props & AV_CODEC_PROP_LOSSLESS) && !(cd->props & AV_CODEC_PROP_LOSSY);
    if (info->lossless)
    {
        info->bits = p->bits_per_raw_sample > 0 ? p->bits_per_raw_sample : p->bits_per_coded_sample;
        if (info->bits <= 0)
            info->bits = av_get_bytes_per_sample(r->dec->sample_fmt) * 8;
    }
    for (unsigned i = 0; i < fmt->nb_streams; i++)
        if ((fmt->streams[i]->disposition & AV_DISPOSITION_ATTACHED_PIC) && fmt->streams[i]->attached_pic.size > 0)
            info->has_cover = 1;
    copy_str(info->codec, sizeof info->codec, cd ? cd->name : "");
    copy_str(info->profile, sizeof info->profile, avcodec_profile_name(p->codec_id, p->profile));
    av_channel_layout_describe(&r->dec->ch_layout, info->layout, sizeof info->layout);
    copy_str(info->container, sizeof info->container, fmt->iformat ? fmt->iformat->name : "");
    *out = r;
    return 0;
}

// The file's cover picture (its encoded bytes: JPEG or PNG, as stored), valid while the reader is open; the
// front cover when several are tagged.
EXPORT int la_cover(const LaReader *r, const unsigned char **data, int *size)
{
    const AVFormatContext *fmt = r->in.fmt;
    const AVStream *best = NULL;
    for (unsigned i = 0; i < fmt->nb_streams; i++)
    {
        const AVStream *s = fmt->streams[i];
        if (!(s->disposition & AV_DISPOSITION_ATTACHED_PIC) || s->attached_pic.size <= 0)
            continue;
        const AVDictionaryEntry *c = av_dict_get(s->metadata, "comment", NULL, 0);
        if (!best || (c && strstr(c->value, "front")))
            best = s;
    }
    if (!best)
        return AVERROR_STREAM_NOT_FOUND;
    *data = best->attached_pic.data;
    *size = best->attached_pic.size;
    return 0;
}

static int append_tags(const AVDictionary *d, char *buf, int cap, int at)
{
    const AVDictionaryEntry *t = NULL;
    while ((t = av_dict_get(d, "", t, AV_DICT_IGNORE_SUFFIX)))
    {
        int k = (int)strlen(t->key), v = (int)strlen(t->value);
        if (at + k + v + 2 < cap)
        {
            memcpy(buf + at, t->key, k);
            buf[at + k] = 0x1f;
            memcpy(buf + at + k + 1, t->value, v);
            buf[at + k + 1 + v] = 0x1e;
        }
        at += k + v + 2;
    }
    return at;
}

// The container's tags then the stream's (Ogg and Opus keep theirs on the stream), UTF-8, each "key 0x1F value
// 0x1E". Returns the length they need; writes them only when that fits in `cap`.
EXPORT int la_tags(const LaReader *r, char *buf, int cap)
{
    int n = append_tags(r->in.fmt->metadata, buf, cap, 0);
    n = append_tags(r->in.fmt->streams[r->ai]->metadata, buf, cap, n);
    if (n < cap)
        buf[n] = 0;
    return n;
}

EXPORT void la_interrupt(LaReader *r)
{
    InterlockedExchange(&r->interrupt, 1);
}

// The decoded frame as interleaved float: 0 when it's dropped (before a seek's target), else its block.
static int la_convert(LaReader *r, AVFrame *f, const float **samples, int *frames, long long *pos)
{
    if (r->want >= 0)
    {
        AVStream *st = r->in.fmt->streams[r->ai];
        int64_t ts = f->best_effort_timestamp != AV_NOPTS_VALUE ? f->best_effort_timestamp : f->pts;
        if (ts != AV_NOPTS_VALUE)
            r->pos = av_rescale_q(ts - r->start, st->time_base, (AVRational){1, r->rate});
    }
    if (r->swr == NULL)
    {
        AVChannelLayout out_layout;
        if (av_channel_layout_copy(&out_layout, &f->ch_layout) < 0 || out_layout.nb_channels != r->channels)
            av_channel_layout_default(&out_layout, r->channels);
        int e = swr_alloc_set_opts2(&r->swr, &out_layout, AV_SAMPLE_FMT_FLT, r->rate, &f->ch_layout,
                                    (enum AVSampleFormat)f->format, f->sample_rate, 0, NULL);
        av_channel_layout_uninit(&out_layout);
        if (e < 0 || swr_init(r->swr) < 0)
        {
            swr_free(&r->swr);
            return AVERROR(EINVAL);
        }
    }
    int cap = swr_get_out_samples(r->swr, f->nb_samples);
    if (cap > r->buf_cap)
    {
        av_free(r->buf);
        r->buf = av_malloc((size_t)cap * r->channels * sizeof(float));
        r->buf_cap = r->buf ? cap : 0;
        if (!r->buf)
            return AVERROR(ENOMEM);
    }
    uint8_t *dst = (uint8_t *)r->buf;
    int n = swr_convert(r->swr, &dst, cap, (const uint8_t **)f->extended_data, f->nb_samples);
    if (n <= 0)
        return 0;
    long long at = r->pos;
    r->pos += n;
    int skip = 0;
    if (r->want >= 0)
    {
        if (at + n <= r->want)
            return 0;
        if (at < r->want)
            skip = (int)(r->want - at);
        r->want = -1;
    }
    *samples = r->buf + (size_t)skip * r->channels;
    *frames = n - skip;
    *pos = at + skip;
    return 1;
}

// The next block: `*frames` frames of interleaved float at `*samples` (valid until the next call on this reader),
// starting at frame `*pos`. AVERROR_EOF at the end; AVERROR_EXIT after la_interrupt, until la_seek.
EXPORT int la_read(LaReader *r, const float **samples, int *frames, long long *pos)
{
    for (;;)
    {
        if (r->interrupt)
            return AVERROR_EXIT;
        int e = avcodec_receive_frame(r->dec, r->frame);
        if (e == 0)
        {
            int got = la_convert(r, r->frame, samples, frames, pos);
            av_frame_unref(r->frame);
            if (got < 0)
                return got;
            if (got)
                return 0;
            continue;
        }
        if (e == AVERROR_EOF)
            return AVERROR_EOF;
        if (e != AVERROR(EAGAIN))
            return e;
        if (r->eof)
            return AVERROR_EOF;
        e = av_read_frame(r->in.fmt, r->pkt);
        if (e < 0)
        {
            r->eof = 1;
            avcodec_send_packet(r->dec, NULL); // drains what the decoder holds
            continue;
        }
        if (r->pkt->stream_index == r->ai)
            avcodec_send_packet(r->dec, r->pkt); // a damaged packet is skipped
        av_packet_unref(r->pkt);
    }
}

// Goes to `frame`: the next block starts exactly there. Lossy codecs start decoding a little before (their
// frames lean on the ones before: MP3's bit reservoir, Opus's pre-roll) so the first block heard is whole.
EXPORT int la_seek(LaReader *r, long long frame)
{
    InterlockedExchange(&r->interrupt, 0);
    if (frame < 0)
        frame = 0;
    AVFormatContext *fmt = r->in.fmt;
    AVStream *st = fmt->streams[r->ai];
    long long from = r->lossless ? frame : frame - r->rate / 10;
    if (from < 0)
        from = 0;
    int64_t ts = r->start + av_rescale_q_rnd(from, (AVRational){1, r->rate}, st->time_base, AV_ROUND_DOWN);
    int e = avformat_seek_file(fmt, r->ai, INT64_MIN, ts, ts, 0);
    if (e < 0)
        e = av_seek_frame(fmt, r->ai, ts, AVSEEK_FLAG_BACKWARD);
    if (e < 0)
        return e;
    avcodec_flush_buffers(r->dec);
    swr_free(&r->swr);
    r->eof = 0;
    r->pos = from;
    r->want = frame;
    return 0;
}

// ---- subtitles --------------------------------------------------------------------------------------------------
// A subtitle stream read on its own: a video's embedded stream through a demuxer of its own (so it can read ahead
// of the pictures, and back from before a seek, without touching the player), or a sidecar file. Looker's reader
// thread pulls events with lv_subs_next and keeps them; the UI thread draws them. Text events come as the ASS text
// FFmpeg's decoders make of every text format; ASS/SSA goes to libass, which the UI thread asks for a picture of
// any moment (lv_subs_render); bitmap subtitles come as BGRA pictures placed on their canvas. Times are
// microseconds on the player's clock (the video stream's start is 0).

enum
{
    LV_SUB_TEXT = 1,
    LV_SUB_ASS = 2,
    LV_SUB_BITMAP = 3,
};

typedef struct LvSubInfo
{
    int kind;     // LV_SUB_*
    int stream;   // the stream read
    int fonts;    // fonts the file carries (Matroska attachments), handed to libass
    int canvas_w; // a bitmap stream's canvas as the headers give it (0 when unknown: events carry their own)
    int canvas_h;
    char codec[32];
} LvSubInfo;

typedef struct LvSubRect
{
    int x, y, w, h; // on the event's canvas
    int stride;
    const unsigned char *bgra; // premultiplied
} LvSubRect;

typedef struct LvSubEvent
{
    long long start_us;
    long long end_us;  // -1: until the next event (PGS shows a picture until the next display set)
    long long read_us; // where the reader is: the time of the packet this came from
    const char *text;  // text: the ASS dialogue's text field; ASS: the whole packet (ReadOrder,Layer,...,Text)
    int n_rects;
    const LvSubRect *rects;
    int canvas_w, canvas_h; // what a bitmap event's rects are placed on
    int read_order;         // ASS: the event's ReadOrder
} LvSubEvent;

typedef struct LvSubImage
{
    int x, y, w, h; // in the frame lv_subs_render was asked for
    int stride;
    const unsigned char *bgra; // premultiplied, NULL when nothing shows
} LvSubImage;

typedef struct LvSubs
{
    HANDLE file;
    AVFormatContext *fmt;
    AVIOContext *io;
    const unsigned char *mem; // lv_subs_open_mem: the text, owned here
    int64_t mem_size, mem_pos;
    volatile LONG interrupt;
    int si;
    int kind;
    long long zero_us;
    AVCodecContext *dec;
    AVPacket *pkt;
    AVSubtitle sub;
    int has_sub;
    char *text;
    size_t text_cap;
    LvSubRect *rects;
    unsigned char **rect_px;
    int n_rects, rects_cap;
    // libass: the reader adds events, the UI thread renders; `lock` keeps them apart.
    CRITICAL_SECTION lock;
    ASS_Library *ass;
    ASS_Renderer *renderer;
    ASS_Track *track;
    int fonts;
    unsigned char *img;
    size_t img_cap;
    LvSubImage last;
    int last_w, last_h;
    double last_scale;
} LvSubs;

static int subs_read(void *opaque, uint8_t *buf, int size)
{
    LvSubs *s = opaque;
    if (s->interrupt)
        return AVERROR_EXIT;
    if (s->mem)
    {
        int64_t left = s->mem_size - s->mem_pos;
        if (left <= 0)
            return AVERROR_EOF;
        int n = (int)(left < size ? left : size);
        memcpy(buf, s->mem + s->mem_pos, (size_t)n);
        s->mem_pos += n;
        return n;
    }
    return io_read(s->file, buf, size);
}

static int64_t subs_seek_io(void *opaque, int64_t offset, int whence)
{
    LvSubs *s = opaque;
    if (!s->mem)
        return io_seek(s->file, offset, whence);
    if (whence & AVSEEK_SIZE)
        return s->mem_size;
    int64_t to = offset;
    switch (whence & ~AVSEEK_FORCE)
    {
    case SEEK_CUR:
        to = s->mem_pos + offset;
        break;
    case SEEK_END:
        to = s->mem_size + offset;
        break;
    }
    if (to < 0 || to > s->mem_size)
        return AVERROR(EINVAL);
    s->mem_pos = to;
    return to;
}

static void subs_clear_event(LvSubs *s)
{
    if (s->has_sub)
        avsubtitle_free(&s->sub);
    s->has_sub = 0;
    for (int i = 0; i < s->n_rects; i++)
        free(s->rect_px[i]);
    s->n_rects = 0;
    if (s->text)
        s->text[0] = 0;
}

EXPORT void lv_subs_close(LvSubs *s)
{
    if (s == NULL)
        return;
    subs_clear_event(s);
    free(s->rects);
    free(s->rect_px);
    free(s->text);
    free(s->img);
    av_packet_free(&s->pkt);
    avcodec_free_context(&s->dec);
    if (s->track)
        ass_free_track(s->track);
    if (s->renderer)
        ass_renderer_done(s->renderer);
    if (s->ass)
        ass_library_done(s->ass);
    if (s->fmt)
        avformat_close_input(&s->fmt);
    if (s->io)
    {
        av_freep(&s->io->buffer);
        avio_context_free(&s->io);
    }
    if (s->file != INVALID_HANDLE_VALUE && s->file)
        CloseHandle(s->file);
    free((void *)s->mem);
    DeleteCriticalSection(&s->lock);
    free(s);
}

// Any thread: the read in progress (or the next) gives up with AVERROR_EXIT, until lv_subs_seek. For closing
// while the reader is deep in a long stretch of a Matroska file with no subtitles.
EXPORT void lv_subs_interrupt(LvSubs *s)
{
    if (s)
        InterlockedExchange(&s->interrupt, 1);
}

static int is_font(const AVStream *st)
{
    if (st->codecpar->codec_type != AVMEDIA_TYPE_ATTACHMENT || st->codecpar->extradata_size <= 0)
        return 0;
    const AVDictionaryEntry *m = av_dict_get(st->metadata, "mimetype", NULL, 0);
    if (m && (strstr(m->value, "font") || strstr(m->value, "truetype") || strstr(m->value, "opentype")))
        return 1;
    const AVDictionaryEntry *f = av_dict_get(st->metadata, "filename", NULL, 0);
    if (f)
    {
        const char *dot = strrchr(f->value, '.');
        if (dot && (_stricmp(dot, ".ttf") == 0 || _stricmp(dot, ".otf") == 0 || _stricmp(dot, ".ttc") == 0 || _stricmp(dot, ".otc") == 0))
            return 1;
    }
    return 0;
}

// The stream picked, its decoder or libass, the fonts. `stream` < 0: the first subtitle stream (a sidecar).
// libass's messages: Looker's builds keep FFmpeg quiet too (av_log_set_level(AV_LOG_QUIET)).
static void ass_quiet(int level, const char *fmt, va_list args, void *data)
{
    (void)level;
    (void)fmt;
    (void)args;
    (void)data;
}

static int subs_setup(LvSubs *s, int stream, LvSubInfo *info)
{
    AVFormatContext *fmt = s->fmt;
    s->si = -1;
    for (unsigned i = 0; i < fmt->nb_streams; i++)
    {
        fmt->streams[i]->discard = AVDISCARD_ALL;
        if (fmt->streams[i]->codecpar->codec_type == AVMEDIA_TYPE_SUBTITLE && (stream < 0 ? s->si < 0 : (int)i == stream))
            s->si = (int)i;
    }
    if (s->si < 0)
        return AVERROR_STREAM_NOT_FOUND;
    AVStream *st = fmt->streams[s->si];
    st->discard = AVDISCARD_DEFAULT;
    int kind = sub_kind(st->codecpar->codec_id);
    s->kind = kind == LV_TRACK_ASS ? LV_SUB_ASS : kind == LV_TRACK_BITMAP ? LV_SUB_BITMAP : LV_SUB_TEXT;
    if ((s->pkt = av_packet_alloc()) == NULL)
        return AVERROR(ENOMEM);
    if (s->kind == LV_SUB_ASS)
    {
        if ((s->ass = ass_library_init()) == NULL)
            return AVERROR(ENOMEM);
        ass_set_message_cb(s->ass, ass_quiet, NULL);
        ass_set_extract_fonts(s->ass, 1); // an .ass file's own [Fonts]
        for (unsigned i = 0; i < fmt->nb_streams; i++)
        {
            const AVStream *a = fmt->streams[i];
            if (!is_font(a))
                continue;
            const AVDictionaryEntry *f = av_dict_get(a->metadata, "filename", NULL, 0);
            ass_add_font(s->ass, f ? f->value : "font", (const char *)a->codecpar->extradata, a->codecpar->extradata_size);
            s->fonts++;
        }
        if ((s->renderer = ass_renderer_init(s->ass)) == NULL || (s->track = ass_new_track(s->ass)) == NULL)
            return AVERROR(ENOMEM);
        // After the fonts are added, so the embedded ones join DirectWrite's. Arial when a style's font is nowhere.
        ass_set_fonts(s->renderer, NULL, "Arial", ASS_FONTPROVIDER_AUTODETECT, NULL, 1);
        ass_set_shaper(s->renderer, ASS_SHAPING_COMPLEX);
        if (st->codecpar->extradata_size > 0)
            ass_process_codec_private(s->track, (char *)st->codecpar->extradata, st->codecpar->extradata_size);
    }
    else
    {
        const AVCodec *c = avcodec_find_decoder(st->codecpar->codec_id);
        if (c == NULL)
            return AVERROR_DECODER_NOT_FOUND;
        if ((s->dec = avcodec_alloc_context3(c)) == NULL)
            return AVERROR(ENOMEM);
        int r = avcodec_parameters_to_context(s->dec, st->codecpar);
        if (r < 0)
            return r;
        s->dec->pkt_timebase = st->time_base;
        if ((r = avcodec_open2(s->dec, c, NULL)) < 0)
            return r;
    }
    memset(info, 0, sizeof *info);
    info->kind = s->kind;
    info->stream = s->si;
    info->fonts = s->fonts;
    info->canvas_w = s->dec && s->dec->width > 0 ? s->dec->width : st->codecpar->width;
    info->canvas_h = s->dec && s->dec->height > 0 ? s->dec->height : st->codecpar->height;
    snprintf(info->codec, sizeof info->codec, "%s", avcodec_get_name(st->codecpar->codec_id));
    return 0;
}

static LvSubs *subs_alloc(void)
{
    LvSubs *s = calloc(1, sizeof *s);
    if (s == NULL)
        return NULL;
    s->file = INVALID_HANDLE_VALUE;
    InitializeCriticalSection(&s->lock);
    return s;
}

static int subs_open_io(LvSubs *s, const char *name, double fps)
{
    const int size = 1 << 16;
    unsigned char *buf = av_malloc(size);
    if (!buf)
        return AVERROR(ENOMEM);
    s->io = avio_alloc_context(buf, size, 0, s, subs_read, NULL, subs_seek_io);
    if (!s->io)
    {
        av_free(buf);
        return AVERROR(ENOMEM);
    }
    if ((s->fmt = avformat_alloc_context()) == NULL)
        return AVERROR(ENOMEM);
    s->fmt->pb = s->io;
    s->fmt->flags |= AVFMT_FLAG_CUSTOM_IO;
    AVDictionary *opts = NULL;
    if (fps > 0)
    {
        // MicroDVD counts frames: the video's rate when the file doesn't say.
        char rate[48];
        AVRational q = av_d2q(fps, 1001000);
        snprintf(rate, sizeof rate, "%d/%d", q.num, q.den);
        av_dict_set(&opts, "subfps", rate, 0);
    }
    int r = avformat_open_input(&s->fmt, name, NULL, &opts);
    av_dict_free(&opts);
    return r; // on failure avformat_open_input freed the context
}

// A video's embedded subtitle stream `stream` (an LvTrack::stream from lv_sub_tracks), through a demuxer of its
// own; or with `stream` < 0 a binary sidecar file (VobSub's .idx, which opens its .sub itself). `zero_us`: the
// player's lv_start_us (0 for a sidecar). `fps`: the video's frame rate (MicroDVD), or 0.
EXPORT int lv_subs_open(const wchar_t *path, int stream, long long zero_us, double fps, LvSubs **out, LvSubInfo *info)
{
    quiet_once();
    *out = NULL;
    LvSubs *s = subs_alloc();
    if (s == NULL)
        return AVERROR(ENOMEM);
    s->zero_us = zero_us;
    char name[1024] = "";
    WideCharToMultiByte(CP_UTF8, 0, path, -1, name, sizeof name, NULL, NULL);
    int r;
    if (stream >= 0)
    {
        s->file = CreateFileW(path, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, NULL, OPEN_EXISTING,
                              FILE_ATTRIBUTE_NORMAL, NULL);
        r = s->file == INVALID_HANDLE_VALUE ? AVERROR(ENOENT) : subs_open_io(s, name, fps);
    }
    else
    {
        r = avformat_open_input(&s->fmt, name, NULL, NULL);
        if (r >= 0)
            r = avformat_find_stream_info(s->fmt, NULL);
    }
    if (r >= 0)
        r = subs_setup(s, stream, info);
    if (r < 0)
    {
        lv_subs_close(s);
        return r;
    }
    *out = s;
    return 0;
}

// A text sidecar file Looker has read and turned into UTF-8 (`data`, `size` bytes, copied): `name` (the file's
// name) helps the probe pick the format by its extension.
EXPORT int lv_subs_open_mem(const unsigned char *data, int size, const wchar_t *name, double fps, LvSubs **out, LvSubInfo *info)
{
    quiet_once();
    *out = NULL;
    LvSubs *s = subs_alloc();
    if (s == NULL)
        return AVERROR(ENOMEM);
    unsigned char *copy = malloc((size_t)size + 1);
    if (copy == NULL)
    {
        lv_subs_close(s);
        return AVERROR(ENOMEM);
    }
    memcpy(copy, data, (size_t)size);
    copy[size] = 0;
    s->mem = copy;
    s->mem_size = size;
    char n8[1024] = "";
    WideCharToMultiByte(CP_UTF8, 0, name, -1, n8, sizeof n8, NULL, NULL);
    int r = subs_open_io(s, n8, fps);
    if (r >= 0)
        r = subs_setup(s, -1, info);
    if (r < 0)
    {
        lv_subs_close(s);
        return r;
    }
    *out = s;
    return 0;
}

// Reading goes on from `us` (the player's clock): the keyframe at or before it, as the container's index has it.
EXPORT int lv_subs_seek(LvSubs *s, long long us)
{
    InterlockedExchange(&s->interrupt, 0);
    int64_t ts = us + s->zero_us;
    if (ts < 0)
        ts = 0;
    int r = avformat_seek_file(s->fmt, -1, INT64_MIN, ts, ts, 0);
    if (r < 0)
        r = av_seek_frame(s->fmt, -1, ts, AVSEEK_FLAG_BACKWARD);
    if (r < 0)
        return r;
    if (s->dec)
        avcodec_flush_buffers(s->dec);
    return 0;
}

// Appends `t` to the event's text, lines joined by ASS's \N.
static int text_append(LvSubs *s, const char *t, size_t add)
{
    size_t have = s->text ? strlen(s->text) : 0;
    size_t need = have + add + 3;
    if (need > s->text_cap)
    {
        char *b = realloc(s->text, need * 2);
        if (b == NULL)
            return AVERROR(ENOMEM);
        if (s->text == NULL)
            b[0] = 0;
        s->text = b;
        s->text_cap = need * 2;
    }
    if (have > 0)
    {
        memcpy(s->text + have, "\\N", 2);
        have += 2;
    }
    memcpy(s->text + have, t, add);
    s->text[have + add] = 0;
    return 0;
}

// An ASS dialogue line as FFmpeg's decoders make it ("ReadOrder,Layer,Style,Name,MarginL,MarginR,MarginV,Effect,
// Text"): its text, the part after the eighth comma.
static const char *dialogue_text(const char *line)
{
    const char *p = line;
    for (int commas = 0; commas < 8; commas++)
    {
        p = strchr(p, ',');
        if (p == NULL)
            return line;
        p++;
    }
    return p;
}

static int add_rect(LvSubs *s, const AVSubtitleRect *r)
{
    if (r->w <= 0 || r->h <= 0 || r->data[0] == NULL || r->data[1] == NULL)
        return 0;
    if (s->n_rects == s->rects_cap)
    {
        int cap = s->rects_cap ? s->rects_cap * 2 : 4;
        LvSubRect *a = realloc(s->rects, sizeof *a * cap);
        if (a == NULL)
            return AVERROR(ENOMEM);
        s->rects = a;
        unsigned char **b = realloc(s->rect_px, sizeof *b * cap);
        if (b == NULL)
            return AVERROR(ENOMEM);
        s->rect_px = b;
        s->rects_cap = cap;
    }
    unsigned char *px = malloc((size_t)r->w * r->h * 4);
    if (px == NULL)
        return AVERROR(ENOMEM);
    const uint32_t *pal = (const uint32_t *)r->data[1]; // 0xAARRGGBB
    for (int y = 0; y < r->h; y++)
    {
        const uint8_t *src = r->data[0] + (size_t)y * r->linesize[0];
        unsigned char *dst = px + (size_t)y * r->w * 4;
        for (int x = 0; x < r->w; x++)
        {
            uint32_t c = pal[src[x]];
            unsigned a = c >> 24;
            dst[4 * x + 0] = (unsigned char)((c & 0xFF) * a / 255);
            dst[4 * x + 1] = (unsigned char)(((c >> 8) & 0xFF) * a / 255);
            dst[4 * x + 2] = (unsigned char)(((c >> 16) & 0xFF) * a / 255);
            dst[4 * x + 3] = (unsigned char)a;
        }
    }
    LvSubRect *o = &s->rects[s->n_rects];
    s->rect_px[s->n_rects] = px;
    s->n_rects++;
    o->x = r->x;
    o->y = r->y;
    o->w = r->w;
    o->h = r->h;
    o->stride = r->w * 4;
    o->bgra = px;
    return 0;
}

// The next event: 0 with `out` filled (valid until the next call), AVERROR_EOF at the end, AVERROR_EXIT after
// lv_subs_interrupt. Packets that make no event (a decoder still collecting, an empty text) are read past.
EXPORT int lv_subs_next(LvSubs *s, LvSubEvent *out)
{
    memset(out, 0, sizeof *out);
    for (;;)
    {
        subs_clear_event(s);
        int r = av_read_frame(s->fmt, s->pkt);
        if (r < 0)
            return r == AVERROR(EIO) ? AVERROR_EOF : r;
        AVPacket *pkt = s->pkt;
        if (pkt->stream_index != s->si)
        {
            av_packet_unref(pkt);
            continue;
        }
        AVStream *st = s->fmt->streams[s->si];
        int64_t ts = pkt->pts != AV_NOPTS_VALUE ? pkt->pts : pkt->dts;
        long long at = ts != AV_NOPTS_VALUE ? av_rescale_q(ts, st->time_base, AV_TIME_BASE_Q) - s->zero_us : 0;
        long long dur = pkt->duration > 0 ? av_rescale_q(pkt->duration, st->time_base, AV_TIME_BASE_Q) : 0;
        out->read_us = at;
        if (s->kind == LV_SUB_ASS)
        {
            // Matroska's ASS packets (and FFmpeg's ASS demuxer's) are what libass takes: ReadOrder,Layer,...,Text.
            // libass drops a ReadOrder it already has, so reading a stretch again after a seek adds nothing.
            if (pkt->size > 0)
            {
                EnterCriticalSection(&s->lock);
                ass_process_chunk(s->track, (char *)pkt->data, pkt->size, at / 1000, dur / 1000);
                LeaveCriticalSection(&s->lock);
                text_append(s, (const char *)pkt->data, (size_t)pkt->size);
                out->read_order = atoi(s->text);
            }
            av_packet_unref(pkt);
            out->start_us = at;
            out->end_us = at + (dur > 0 ? dur : 5000000);
            out->text = s->text ? s->text : "";
            return 0;
        }
        int got = 0;
        r = avcodec_decode_subtitle2(s->dec, &s->sub, &got, pkt);
        av_packet_unref(pkt);
        if (r < 0 || !got)
            continue;
        s->has_sub = 1;
        AVSubtitle *sub = &s->sub;
        long long base = sub->pts != AV_NOPTS_VALUE ? sub->pts - s->zero_us : at;
        out->start_us = base + (long long)sub->start_display_time * 1000;
        if (sub->end_display_time > sub->start_display_time && sub->end_display_time < 0x7FFFFFFF)
            out->end_us = base + (long long)sub->end_display_time * 1000;
        else if (s->kind == LV_SUB_TEXT && dur > 0)
            out->end_us = at + dur;
        else
            out->end_us = -1;
        for (unsigned i = 0; i < sub->num_rects; i++)
        {
            const AVSubtitleRect *rc = sub->rects[i];
            if (rc->type == SUBTITLE_BITMAP)
                add_rect(s, rc);
            else if (rc->type == SUBTITLE_ASS && rc->ass)
            {
                const char *t = dialogue_text(rc->ass);
                text_append(s, t, strlen(t));
            }
            else if (rc->type == SUBTITLE_TEXT && rc->text)
                text_append(s, rc->text, strlen(rc->text));
        }
        // A text event with nothing to say is nothing; an empty bitmap one ends what shows (PGS's clear).
        if (s->kind == LV_SUB_TEXT && (s->text == NULL || s->text[0] == 0))
            continue;
        out->text = s->text ? s->text : "";
        out->n_rects = s->n_rects;
        out->rects = s->rects;
        out->canvas_w = s->dec->width;
        out->canvas_h = s->dec->height;
        return 0;
    }
}

// libass's picture of moment `us` in a frame `w` x `h` pixels showing a video stored `video_w` x `video_h`, its
// text scaled by `scale`: the part with ink, composited into one premultiplied BGRA image (`out`, valid until the
// next call). `*changed`: 0 when it's the same as the last call's (and `out` is that image again). UI thread.
EXPORT int lv_subs_render(LvSubs *s, long long us, int w, int h, int video_w, int video_h, double scale, int *changed,
                          LvSubImage *out)
{
    memset(out, 0, sizeof *out);
    *changed = 1;
    if (s->kind != LV_SUB_ASS || w <= 0 || h <= 0)
        return AVERROR(EINVAL);
    if (scale <= 0)
        scale = 1.0;
    EnterCriticalSection(&s->lock);
    ass_set_frame_size(s->renderer, w, h);
    ass_set_storage_size(s->renderer, video_w > 0 ? video_w : w, video_h > 0 ? video_h : h);
    ass_set_font_scale(s->renderer, scale);
    int ch = 0;
    ASS_Image *img = ass_render_frame(s->renderer, s->track, us / 1000, &ch);
    if (ch == 0 && s->last_w == w && s->last_h == h && s->last_scale == scale)
    {
        *changed = 0;
        *out = s->last;
        LeaveCriticalSection(&s->lock);
        return 0;
    }
    s->last_w = w;
    s->last_h = h;
    s->last_scale = scale;
    int x0 = INT_MAX, y0 = INT_MAX, x1 = INT_MIN, y1 = INT_MIN;
    for (ASS_Image *i = img; i; i = i->next)
    {
        if (i->w <= 0 || i->h <= 0)
            continue;
        x0 = min(x0, i->dst_x);
        y0 = min(y0, i->dst_y);
        x1 = max(x1, i->dst_x + i->w);
        y1 = max(y1, i->dst_y + i->h);
    }
    x0 = max(x0, 0);
    y0 = max(y0, 0);
    x1 = min(x1, w);
    y1 = min(y1, h);
    memset(&s->last, 0, sizeof s->last);
    if (x1 <= x0 || y1 <= y0)
    {
        *out = s->last;
        LeaveCriticalSection(&s->lock);
        return 0;
    }
    int bw = x1 - x0, bh = y1 - y0;
    size_t need = (size_t)bw * bh * 4;
    if (need > s->img_cap)
    {
        free(s->img);
        s->img = malloc(need);
        s->img_cap = s->img ? need : 0;
        if (s->img == NULL)
        {
            LeaveCriticalSection(&s->lock);
            return AVERROR(ENOMEM);
        }
    }
    memset(s->img, 0, need);
    // Source over, premultiplied BGRA: libass's colour is RGBT (T: transparency), each image an alpha mask.
    for (ASS_Image *i = img; i; i = i->next)
    {
        unsigned r = (i->color >> 24) & 0xFF, g = (i->color >> 16) & 0xFF, b = (i->color >> 8) & 0xFF;
        unsigned opacity = 255 - (i->color & 0xFF);
        for (int y = 0; y < i->h; y++)
        {
            int dy = i->dst_y + y;
            if (dy < y0 || dy >= y1)
                continue;
            const unsigned char *m = i->bitmap + (size_t)y * i->stride;
            unsigned char *d = s->img + (size_t)(dy - y0) * bw * 4;
            for (int x = 0; x < i->w; x++)
            {
                int dx = i->dst_x + x;
                if (dx < x0 || dx >= x1 || m[x] == 0)
                    continue;
                unsigned a = m[x] * opacity / 255;
                unsigned char *p = d + (size_t)(dx - x0) * 4;
                unsigned k = 255 - a;
                p[0] = (unsigned char)((b * a + p[0] * k + 127) / 255);
                p[1] = (unsigned char)((g * a + p[1] * k + 127) / 255);
                p[2] = (unsigned char)((r * a + p[2] * k + 127) / 255);
                p[3] = (unsigned char)(a + (p[3] * k + 127) / 255);
            }
        }
    }
    s->last.x = x0;
    s->last.y = y0;
    s->last.w = bw;
    s->last.h = bh;
    s->last.stride = bw * 4;
    s->last.bgra = s->img;
    *out = s->last;
    LeaveCriticalSection(&s->lock);
    return 0;
}
