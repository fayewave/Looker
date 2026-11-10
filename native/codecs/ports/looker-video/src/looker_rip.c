// DVD ripping (native/PLAN.md N8): Looker reads a title off a DVD's VOB files itself (src/dvd.rs: the IFOs, the
// title's cells in play order, angle 1 of an interleaved block) and hands this file the bytes as one MPEG program
// stream through a read callback. Out goes Matroska (every stream copied; DVD LPCM, which Matroska can't hold,
// rewrapped losslessly as PCM; VobSub subtitles with the disc's palette) or MP4 (H.264 through Windows' Media
// Foundation encoder, deinterlaced with bwdif, every audio track AAC). Chapters and track languages come from the
// IFOs. Everything here is LGPL: no libdvdcss, no x264. An encrypted disc never gets this far (dvd.rs refuses it).
//
// Timestamps jump where a title's cells come from different VOBs; they are stitched into one timeline the way
// the ffmpeg tool does for discontinuous formats (an offset moved whenever a stream jumps back, or ahead by more
// than ten seconds).

#define WIN32_LEAN_AND_MEAN
#include <windows.h>

#include <libavcodec/avcodec.h>
#include <libavfilter/avfilter.h>
#include <libavfilter/buffersink.h>
#include <libavfilter/buffersrc.h>
#include <libavformat/avformat.h>
#include <libavutil/avstring.h>
#include <libavutil/channel_layout.h>
#include <libavutil/opt.h>

#include <stdio.h>
#include <string.h>

#define EXPORT __declspec(dllexport)

enum
{
    LV_RIP_MKV = 0, // every stream copied
    LV_RIP_MP4 = 1, // H.264 + AAC
};

// Fills `buf` with up to `size` bytes of the title, returns how many (0 at the end) or a negative AVERROR.
typedef int (*LvRipRead)(void *ctx, unsigned char *buf, int size);

typedef struct LvRipStream
{
    int id;       // its id in the program stream: 0x80+n AC-3, 0x88+n DTS, 0xA0+n LPCM, 0x1C0+n MPEG, 0x20+n subpicture
    char lang[4]; // ISO 639-2 ("eng"), or ""
} LvRipStream;

typedef struct LvRipJob
{
    LvRipRead read;
    void *ctx;
    const wchar_t *out;
    int mode;
    int audio_count;
    const LvRipStream *audio;
    int sub_count;
    const LvRipStream *subs;
    const char *sub_extradata; // VobSub's "size: ...\npalette: ...\n", or NULL
    int chapter_count;
    const long long *chapters_us; // each chapter's start
    long long duration_us;        // the title's, for the last chapter's end
    int dar_num, dar_den;         // display aspect ratio: 4:3 or 16:9
    int video_kbps;               // MP4's H.264 bit rate
    const volatile LONG *cancel;  // non-zero: stop (returns AVERROR_EXIT)
} LvRipJob;

enum
{
    KIND_VIDEO,
    KIND_AUDIO,
    KIND_SUB,
};

typedef struct Out
{
    int kind;
    int in_id;            // the program stream id it takes packets from
    AVRational in_tb;     // that stream's time base, once seen
    AVStream *st;         // the output stream
    int copy;             // packets go straight through
    AVCodecContext *dec;  // else: decoded,
    AVFilterGraph *graph; // filtered,
    AVFilterContext *src, *sink;
    AVCodecContext *enc;  // and encoded
    long long next_us;    // when the next packet is due (discontinuity check), AV_NOPTS_VALUE before the first
    long long last_dts;   // the last written, in the output time base (kept strictly rising)
} Out;

typedef struct Rip
{
    const LvRipJob *job;
    AVFormatContext *in;
    AVIOContext *io;
    AVFormatContext *oc;
    Out outs[64];
    int nouts;
    int map[256];         // input stream index -> outs index, -1 unmapped, -2 ignored
    long long offset_us;  // added to every input time: the start moved to 0, cell jumps stitched
    AVFrame *frame, *filtered;
    AVPacket *enc_pkt;
} Rip;

static int cancelled(const Rip *r)
{
    return r->job->cancel && *r->job->cancel;
}

static int rip_read(void *opaque, uint8_t *buf, int size)
{
    Rip *r = (Rip *)opaque;
    if (cancelled(r))
        return AVERROR_EXIT;
    int n = r->job->read(r->job->ctx, buf, size);
    if (n < 0)
        return n;
    return n ? n : AVERROR_EOF;
}

// ---- setup ----------------------------------------------------------------------------------------------------

static int open_input(Rip *r)
{
    const int bufsize = 1 << 16;
    unsigned char *buf = av_malloc(bufsize);
    if (!buf)
        return AVERROR(ENOMEM);
    r->io = avio_alloc_context(buf, bufsize, 0, r, rip_read, NULL, NULL);
    if (!r->io)
    {
        av_free(buf);
        return AVERROR(ENOMEM);
    }
    r->io->seekable = 0;
    r->in = avformat_alloc_context();
    if (!r->in)
        return AVERROR(ENOMEM);
    r->in->pb = r->io;
    r->in->flags |= AVFMT_FLAG_CUSTOM_IO | AVFMT_FLAG_GENPTS;
    // DVDs often hold the extra audio tracks back a little: look far enough to meet every one.
    r->in->probesize = 64 << 20;
    r->in->max_analyze_duration = 15 * (long long)AV_TIME_BASE;
    const AVInputFormat *ps = av_find_input_format("mpeg");
    int e = avformat_open_input(&r->in, NULL, ps, NULL);
    if (e < 0)
        return e;
    return avformat_find_stream_info(r->in, NULL);
}

static AVStream *input_with_id(Rip *r, int id, enum AVMediaType type)
{
    for (unsigned i = 0; i < r->in->nb_streams; i++)
    {
        AVStream *st = r->in->streams[i];
        if (st->codecpar->codec_type == type && (id < 0 || st->id == id))
            return st;
    }
    return NULL;
}

static void set_lang(AVStream *st, const char *lang)
{
    if (lang && lang[0])
        av_dict_set(&st->metadata, "language", lang, 0);
}

static int open_decoder(AVStream *st, AVCodecContext **out)
{
    const AVCodec *c = avcodec_find_decoder(st->codecpar->codec_id);
    if (!c)
        return AVERROR_DECODER_NOT_FOUND;
    AVCodecContext *d = avcodec_alloc_context3(c);
    if (!d)
        return AVERROR(ENOMEM);
    int e = avcodec_parameters_to_context(d, st->codecpar);
    if (e >= 0)
    {
        d->pkt_timebase = st->time_base;
        d->thread_count = 0;
        e = avcodec_open2(d, c, NULL);
    }
    if (e < 0)
    {
        avcodec_free_context(&d);
        return e;
    }
    *out = d;
    return 0;
}

// buffer/abuffer -> `chain` -> buffersink/abuffersink.
static int build_graph(Out *o, const char *src_args, const char *chain)
{
    int video = o->kind == KIND_VIDEO;
    o->graph = avfilter_graph_alloc();
    if (!o->graph)
        return AVERROR(ENOMEM);
    o->graph->nb_threads = 0;
    int e = avfilter_graph_create_filter(&o->src, avfilter_get_by_name(video ? "buffer" : "abuffer"), "in", src_args, NULL, o->graph);
    if (e < 0)
        return e;
    e = avfilter_graph_create_filter(&o->sink, avfilter_get_by_name(video ? "buffersink" : "abuffersink"), "out", NULL, NULL, o->graph);
    if (e < 0)
        return e;
    AVFilterInOut *outputs = avfilter_inout_alloc(), *inputs = avfilter_inout_alloc();
    if (!outputs || !inputs)
    {
        avfilter_inout_free(&outputs);
        avfilter_inout_free(&inputs);
        return AVERROR(ENOMEM);
    }
    outputs->name = av_strdup("in");
    outputs->filter_ctx = o->src;
    inputs->name = av_strdup("out");
    inputs->filter_ctx = o->sink;
    e = avfilter_graph_parse_ptr(o->graph, chain, &inputs, &outputs, NULL);
    avfilter_inout_free(&outputs);
    avfilter_inout_free(&inputs);
    if (e < 0)
        return e;
    return avfilter_graph_config(o->graph, NULL);
}

static AVRational sar_for(const LvRipJob *job, int w, int h)
{
    AVRational sar = {0, 1};
    if (job->dar_num > 0 && job->dar_den > 0 && w > 0 && h > 0)
        av_reduce(&sar.num, &sar.den, (long long)job->dar_num * h, (long long)job->dar_den * w, 1 << 16);
    return sar;
}

static int add_video(Rip *r, AVStream *in)
{
    Out *o = &r->outs[r->nouts];
    memset(o, 0, sizeof *o);
    o->kind = KIND_VIDEO;
    o->in_id = in->id;
    o->in_tb = in->time_base;
    o->st = avformat_new_stream(r->oc, NULL);
    if (!o->st)
        return AVERROR(ENOMEM);
    AVRational sar = sar_for(r->job, in->codecpar->width, in->codecpar->height);
    if (r->job->mode == LV_RIP_MKV)
    {
        o->copy = 1;
        int e = avcodec_parameters_copy(o->st->codecpar, in->codecpar);
        if (e < 0)
            return e;
        o->st->codecpar->codec_tag = 0;
        if (sar.num)
            o->st->codecpar->sample_aspect_ratio = o->st->sample_aspect_ratio = sar;
        o->st->time_base = in->time_base;
        o->st->avg_frame_rate = av_guess_frame_rate(r->in, in, NULL);
        r->nouts++;
        return 0;
    }
    int e = open_decoder(in, &o->dec);
    if (e < 0)
        return e;
    if (!sar.num)
        sar = o->dec->sample_aspect_ratio.num ? o->dec->sample_aspect_ratio : (AVRational){1, 1};
    char args[256];
    snprintf(args, sizeof args, "video_size=%dx%d:pix_fmt=%d:time_base=%d/%d:pixel_aspect=%d/%d", o->dec->width,
             o->dec->height, o->dec->pix_fmt == AV_PIX_FMT_NONE ? AV_PIX_FMT_YUV420P : o->dec->pix_fmt,
             in->time_base.num, in->time_base.den, sar.num, sar.den);
    // bwdif touches only the pictures flagged interlaced; a progressive film transfer passes through untouched.
    // NV12: what the GPUs' encoders take (NVIDIA's refuses yuv420p); Windows' software one takes it too.
    const char *chain = avfilter_get_by_name("bwdif") ? "bwdif=mode=send_frame:deint=interlaced,format=nv12"
                                                      : "format=nv12";
    e = build_graph(o, args, chain);
    if (e < 0)
        return e;
    const AVCodec *c = avcodec_find_encoder_by_name("h264_mf");
    if (!c)
        return AVERROR_ENCODER_NOT_FOUND;
    AVRational fps = av_guess_frame_rate(r->in, in, NULL);
    // A hardware encoder first; Windows' software one if there is none.
    for (int hw = 1; hw >= 0; hw--)
    {
        o->enc = avcodec_alloc_context3(c);
        if (!o->enc)
            return AVERROR(ENOMEM);
        o->enc->width = av_buffersink_get_w(o->sink);
        o->enc->height = av_buffersink_get_h(o->sink);
        o->enc->pix_fmt = av_buffersink_get_format(o->sink);
        o->enc->sample_aspect_ratio = av_buffersink_get_sample_aspect_ratio(o->sink);
        o->enc->time_base = av_buffersink_get_time_base(o->sink);
        if (fps.num > 0)
            o->enc->framerate = fps;
        o->enc->bit_rate = (long long)(r->job->video_kbps > 0 ? r->job->video_kbps : 2500) * 1000;
        o->enc->color_range = AVCOL_RANGE_MPEG;
        o->enc->profile = AV_PROFILE_H264_HIGH; // NVIDIA's MFT otherwise picks Constrained Baseline
        if (r->oc->oformat->flags & AVFMT_GLOBALHEADER)
            o->enc->flags |= AV_CODEC_FLAG_GLOBAL_HEADER;
        AVDictionary *opts = NULL;
        av_dict_set(&opts, "hw_encoding", hw ? "1" : "0", 0);
        av_dict_set(&opts, "rate_control", "u_vbr", 0);
        av_dict_set(&opts, "scenario", "archive", 0);
        e = avcodec_open2(o->enc, c, &opts);
        av_dict_free(&opts);
        if (e >= 0)
            break;
        avcodec_free_context(&o->enc);
    }
    if (e < 0)
        return e;
    e = avcodec_parameters_from_context(o->st->codecpar, o->enc);
    if (e < 0)
        return e;
    o->st->time_base = o->enc->time_base;
    o->st->sample_aspect_ratio = o->enc->sample_aspect_ratio;
    if (fps.num > 0)
        o->st->avg_frame_rate = fps;
    r->nouts++;
    return 0;
}

// "mono|stereo|5.1..." from what the encoder takes, for aformat.
static void layouts_of(const AVCodecContext *enc_ctx, const AVCodec *c, char *buf, int cap)
{
    const void *cfg = NULL;
    int n = 0;
    buf[0] = 0;
    if (avcodec_get_supported_config(enc_ctx, c, AV_CODEC_CONFIG_CHANNEL_LAYOUT, 0, &cfg, &n) < 0 || !cfg)
        return;
    const AVChannelLayout *l = cfg;
    for (int i = 0; i < n; i++)
    {
        char name[64];
        if (av_channel_layout_describe(&l[i], name, sizeof name) < 0)
            continue;
        if (buf[0])
            av_strlcat(buf, "|", cap);
        av_strlcat(buf, name, cap);
    }
}

static int add_audio(Rip *r, AVStream *in, const char *lang)
{
    Out *o = &r->outs[r->nouts];
    memset(o, 0, sizeof *o);
    o->kind = KIND_AUDIO;
    o->in_id = in->id;
    o->in_tb = in->time_base;
    o->st = avformat_new_stream(r->oc, NULL);
    if (!o->st)
        return AVERROR(ENOMEM);
    set_lang(o->st, lang);
    int mkv = r->job->mode == LV_RIP_MKV;
    if (mkv && in->codecpar->codec_id != AV_CODEC_ID_PCM_DVD)
    {
        o->copy = 1;
        int e = avcodec_parameters_copy(o->st->codecpar, in->codecpar);
        if (e < 0)
            return e;
        o->st->codecpar->codec_tag = 0;
        o->st->time_base = in->time_base;
        r->nouts++;
        return 0;
    }
    int e = open_decoder(in, &o->dec);
    if (e < 0)
        return e;
    if (!o->dec->ch_layout.nb_channels)
        return AVERROR_INVALIDDATA;
    if (o->dec->ch_layout.order == AV_CHANNEL_ORDER_UNSPEC)
        av_channel_layout_default(&o->dec->ch_layout, o->dec->ch_layout.nb_channels);
    // MKV: DVD LPCM as plain PCM, bit for bit. MP4: AAC.
    int pcm24 = o->dec->sample_fmt == AV_SAMPLE_FMT_S32 || o->dec->sample_fmt == AV_SAMPLE_FMT_S32P;
    const AVCodec *c = avcodec_find_encoder_by_name(mkv ? (pcm24 ? "pcm_s24le" : "pcm_s16le") : "aac");
    if (!c)
        return AVERROR_ENCODER_NOT_FOUND;
    char layout[128], args[512], chain[1024];
    av_channel_layout_describe(&o->dec->ch_layout, layout, sizeof layout);
    snprintf(args, sizeof args, "time_base=%d/%d:sample_rate=%d:sample_fmt=%s:channel_layout=%s", in->time_base.num,
             in->time_base.den, o->dec->sample_rate, av_get_sample_fmt_name(o->dec->sample_fmt), layout);
    if (mkv)
    {
        snprintf(chain, sizeof chain, "aformat=sample_fmts=%s", pcm24 ? "s32" : "s16");
    }
    else
    {
        char layouts[768];
        layouts_of(NULL, c, layouts, sizeof layouts);
        // async: the audio held to the stitched timeline, padded or trimmed where cells join.
        snprintf(chain, sizeof chain, "aresample=async=1:min_hard_comp=0.1:first_pts=0,aformat=sample_fmts=fltp:sample_rates=48000%s%s",
                 layouts[0] ? ":channel_layouts=" : "", layouts);
    }
    e = build_graph(o, args, chain);
    if (e < 0)
        return e;
    o->enc = avcodec_alloc_context3(c);
    if (!o->enc)
        return AVERROR(ENOMEM);
    o->enc->sample_fmt = av_buffersink_get_format(o->sink);
    o->enc->sample_rate = av_buffersink_get_sample_rate(o->sink);
    e = av_buffersink_get_ch_layout(o->sink, &o->enc->ch_layout);
    if (e < 0)
        return e;
    o->enc->time_base = (AVRational){1, o->enc->sample_rate};
    if (!mkv)
    {
        int ch = o->enc->ch_layout.nb_channels;
        o->enc->bit_rate = ch <= 2 ? 192000 : 384000;
    }
    if (r->oc->oformat->flags & AVFMT_GLOBALHEADER)
        o->enc->flags |= AV_CODEC_FLAG_GLOBAL_HEADER;
    e = avcodec_open2(o->enc, c, NULL);
    if (e < 0)
        return e;
    if (o->enc->frame_size > 0 && !(c->capabilities & AV_CODEC_CAP_VARIABLE_FRAME_SIZE))
        av_buffersink_set_frame_size(o->sink, (unsigned)o->enc->frame_size);
    e = avcodec_parameters_from_context(o->st->codecpar, o->enc);
    if (e < 0)
        return e;
    o->st->time_base = o->enc->time_base;
    r->nouts++;
    return 0;
}

// A subpicture stream, set up from the IFO: its packets may not start until well into the film.
static int add_sub(Rip *r, const LvRipStream *s, int w, int h)
{
    Out *o = &r->outs[r->nouts];
    memset(o, 0, sizeof *o);
    o->kind = KIND_SUB;
    o->in_id = s->id;
    o->in_tb = (AVRational){1, 90000};
    o->copy = 1;
    o->st = avformat_new_stream(r->oc, NULL);
    if (!o->st)
        return AVERROR(ENOMEM);
    set_lang(o->st, s->lang);
    AVCodecParameters *p = o->st->codecpar;
    p->codec_type = AVMEDIA_TYPE_SUBTITLE;
    p->codec_id = AV_CODEC_ID_DVD_SUBTITLE;
    p->width = w;
    p->height = h;
    if (r->job->sub_extradata)
    {
        size_t n = strlen(r->job->sub_extradata);
        p->extradata = av_mallocz(n + 1 + AV_INPUT_BUFFER_PADDING_SIZE);
        if (!p->extradata)
            return AVERROR(ENOMEM);
        memcpy(p->extradata, r->job->sub_extradata, n + 1);
        p->extradata_size = (int)n + 1;
    }
    o->st->time_base = o->in_tb;
    r->nouts++;
    return 0;
}

static int add_chapters(Rip *r)
{
    const LvRipJob *j = r->job;
    if (j->chapter_count < 2)
        return 0;
    r->oc->chapters = av_calloc(j->chapter_count, sizeof *r->oc->chapters);
    if (!r->oc->chapters)
        return AVERROR(ENOMEM);
    for (int i = 0; i < j->chapter_count; i++)
    {
        AVChapter *c = av_mallocz(sizeof *c);
        if (!c)
            return AVERROR(ENOMEM);
        c->id = i + 1;
        c->time_base = (AVRational){1, 1000000};
        c->start = j->chapters_us[i];
        c->end = i + 1 < j->chapter_count ? j->chapters_us[i + 1] : FFMAX(j->duration_us, c->start + 1);
        char name[32];
        snprintf(name, sizeof name, "Chapter %d", i + 1);
        av_dict_set(&c->metadata, "title", name, 0);
        r->oc->chapters[r->oc->nb_chapters++] = c;
    }
    return 0;
}

static int open_output(Rip *r)
{
    const LvRipJob *j = r->job;
    int e = avformat_alloc_output_context2(&r->oc, NULL, j->mode == LV_RIP_MKV ? "matroska" : "mp4", NULL);
    if (e < 0)
        return e;
    AVStream *video = input_with_id(r, 0x1e0, AVMEDIA_TYPE_VIDEO);
    if (!video)
        video = input_with_id(r, -1, AVMEDIA_TYPE_VIDEO);
    if (!video)
        return AVERROR_STREAM_NOT_FOUND;
    e = add_video(r, video);
    if (e < 0)
        return e;
    for (int i = 0; i < j->audio_count && r->nouts < 32; i++)
    {
        AVStream *a = input_with_id(r, j->audio[i].id, AVMEDIA_TYPE_AUDIO);
        if (!a)
            continue; // listed in the IFO but never in the stream
        e = add_audio(r, a, j->audio[i].lang);
        if (e < 0)
            return e;
    }
    if (j->mode == LV_RIP_MKV)
        for (int i = 0; i < j->sub_count && r->nouts < 64; i++)
        {
            e = add_sub(r, &j->subs[i], video->codecpar->width, video->codecpar->height);
            if (e < 0)
                return e;
        }
    e = add_chapters(r);
    if (e < 0)
        return e;
    int len = WideCharToMultiByte(CP_UTF8, 0, j->out, -1, NULL, 0, NULL, NULL);
    char *utf8 = len > 0 ? av_malloc(len) : NULL;
    if (!utf8)
        return AVERROR(ENOMEM);
    WideCharToMultiByte(CP_UTF8, 0, j->out, -1, utf8, len, NULL, NULL);
    e = avio_open(&r->oc->pb, utf8, AVIO_FLAG_WRITE);
    av_free(utf8);
    if (e < 0)
        return e;
    return avformat_write_header(r->oc, NULL);
}

// ---- the loop -------------------------------------------------------------------------------------------------

static int write_packet(Rip *r, Out *o, AVPacket *pkt, AVRational from)
{
    av_packet_rescale_ts(pkt, from, o->st->time_base);
    pkt->stream_index = o->st->index;
    pkt->pos = -1;
    if (pkt->dts != AV_NOPTS_VALUE)
    {
        if (o->last_dts != AV_NOPTS_VALUE && pkt->dts <= o->last_dts)
            pkt->dts = o->last_dts + 1;
        if (pkt->pts != AV_NOPTS_VALUE && pkt->pts < pkt->dts)
            pkt->pts = pkt->dts;
        o->last_dts = pkt->dts;
    }
    return av_interleaved_write_frame(r->oc, pkt);
}

static int drain_encoder(Rip *r, Out *o)
{
    for (;;)
    {
        int e = avcodec_receive_packet(o->enc, r->enc_pkt);
        if (e == AVERROR(EAGAIN) || e == AVERROR_EOF)
            return 0;
        if (e < 0)
            return e;
        e = write_packet(r, o, r->enc_pkt, o->enc->time_base);
        av_packet_unref(r->enc_pkt);
        if (e < 0)
            return e;
    }
}

static int drain_filter(Rip *r, Out *o, int flush)
{
    for (;;)
    {
        int e = av_buffersink_get_frame(o->sink, r->filtered);
        if (e == AVERROR(EAGAIN))
            return 0;
        if (e == AVERROR_EOF)
        {
            if (!flush)
                return 0;
            e = avcodec_send_frame(o->enc, NULL);
            if (e < 0 && e != AVERROR_EOF)
                return e;
            return drain_encoder(r, o);
        }
        if (e < 0)
            return e;
        r->filtered->pict_type = AV_PICTURE_TYPE_NONE;
        if (r->filtered->pts != AV_NOPTS_VALUE)
            r->filtered->pts = av_rescale_q(r->filtered->pts, av_buffersink_get_time_base(o->sink), o->enc->time_base);
        e = avcodec_send_frame(o->enc, r->filtered);
        av_frame_unref(r->filtered);
        if (e < 0)
            return e;
        e = drain_encoder(r, o);
        if (e < 0)
            return e;
    }
}

static int drain_decoder(Rip *r, Out *o)
{
    for (;;)
    {
        int e = avcodec_receive_frame(o->dec, r->frame);
        if (e == AVERROR(EAGAIN) || e == AVERROR_EOF)
            return 0;
        if (e < 0)
            return 0; // a broken picture: carry on with the next
        r->frame->pts = r->frame->best_effort_timestamp;
        e = av_buffersrc_add_frame_flags(o->src, r->frame, AV_BUFFERSRC_FLAG_KEEP_REF);
        av_frame_unref(r->frame);
        if (e < 0)
            return e;
        e = drain_filter(r, o, 0);
        if (e < 0)
            return e;
    }
}

// Moves the packet onto the stitched timeline: a stream that jumps back, or ahead by more than ten seconds, has
// crossed into a cell with its own clock (subtitles, which are sparse, only follow).
static void stitch(Rip *r, Out *o, AVPacket *pkt)
{
    long long ts = pkt->dts != AV_NOPTS_VALUE ? pkt->dts : pkt->pts;
    if (ts == AV_NOPTS_VALUE)
        return;
    long long t = av_rescale_q(ts, o->in_tb, AV_TIME_BASE_Q) + r->offset_us;
    if (o->kind != KIND_SUB && o->next_us != AV_NOPTS_VALUE)
    {
        long long delta = t - o->next_us;
        if (delta > 10LL * AV_TIME_BASE || delta < -AV_TIME_BASE / 10)
        {
            r->offset_us -= delta;
            t -= delta;
        }
    }
    long long shift = av_rescale_q(r->offset_us, AV_TIME_BASE_Q, o->in_tb);
    if (pkt->pts != AV_NOPTS_VALUE)
        pkt->pts += shift;
    if (pkt->dts != AV_NOPTS_VALUE)
        pkt->dts += shift;
    if (o->kind != KIND_SUB)
        o->next_us = t + (pkt->duration > 0 ? av_rescale_q(pkt->duration, o->in_tb, AV_TIME_BASE_Q) : 0);
}

static Out *out_for(Rip *r, int index)
{
    if (index < 0 || index >= 256)
        return NULL;
    if (r->map[index] == -1)
    {
        AVStream *st = r->in->streams[index];
        r->map[index] = -2;
        for (int i = 0; i < r->nouts; i++)
            if (r->outs[i].in_id == st->id && r->outs[i].kind == (st->codecpar->codec_type == AVMEDIA_TYPE_VIDEO ? KIND_VIDEO
                                                                    : st->codecpar->codec_type == AVMEDIA_TYPE_AUDIO ? KIND_AUDIO
                                                                                                                    : KIND_SUB))
            {
                r->map[index] = i;
                r->outs[i].in_tb = st->time_base;
                break;
            }
    }
    return r->map[index] >= 0 ? &r->outs[r->map[index]] : NULL;
}

static int run(Rip *r)
{
    AVPacket *pkt = av_packet_alloc();
    if (!pkt)
        return AVERROR(ENOMEM);
    int e = 0;
    while (!cancelled(r))
    {
        e = av_read_frame(r->in, pkt);
        if (e == AVERROR_EOF || e == AVERROR(EIO))
        {
            e = 0;
            break;
        }
        if (e < 0)
            break;
        Out *o = out_for(r, pkt->stream_index);
        if (o)
        {
            stitch(r, o, pkt);
            if (o->copy)
                e = write_packet(r, o, pkt, o->in_tb);
            else
            {
                e = avcodec_send_packet(o->dec, pkt);
                if (e < 0 && e != AVERROR(EAGAIN))
                    e = 0; // a damaged packet: skip it
                else
                    e = drain_decoder(r, o);
            }
        }
        av_packet_unref(pkt);
        if (e < 0)
            break;
    }
    av_packet_free(&pkt);
    if (e >= 0 && cancelled(r))
        e = AVERROR_EXIT;
    if (e < 0)
        return e;
    // The end: everything still in the decoders, filters and encoders.
    for (int i = 0; i < r->nouts; i++)
    {
        Out *o = &r->outs[i];
        if (o->copy)
            continue;
        avcodec_send_packet(o->dec, NULL);
        e = drain_decoder(r, o);
        if (e < 0)
            return e;
        e = av_buffersrc_add_frame_flags(o->src, NULL, 0);
        if (e < 0)
            return e;
        e = drain_filter(r, o, 1);
        if (e < 0)
            return e;
    }
    return av_write_trailer(r->oc);
}

EXPORT int lv_rip(const LvRipJob *job)
{
    Rip *r = av_mallocz(sizeof *r);
    if (!r)
        return AVERROR(ENOMEM);
    r->job = job;
    memset(r->map, -1, sizeof r->map);
    for (int i = 0; i < 64; i++)
    {
        r->outs[i].next_us = AV_NOPTS_VALUE;
        r->outs[i].last_dts = AV_NOPTS_VALUE;
    }
    r->frame = av_frame_alloc();
    r->filtered = av_frame_alloc();
    r->enc_pkt = av_packet_alloc();
    int e = r->frame && r->filtered && r->enc_pkt ? 0 : AVERROR(ENOMEM);
    if (e >= 0)
        e = open_input(r);
    if (e >= 0)
    {
        // The title's first time is 0.
        if (r->in->start_time != AV_NOPTS_VALUE)
            r->offset_us = -r->in->start_time;
        e = open_output(r);
    }
    for (int i = 0; i < 64; i++)
    {
        // memset in add_* cleared these for the outputs made; put the "none yet" marks back.
        r->outs[i].next_us = AV_NOPTS_VALUE;
        r->outs[i].last_dts = AV_NOPTS_VALUE;
    }
    if (e >= 0)
        e = run(r);
    for (int i = 0; i < r->nouts; i++)
    {
        avcodec_free_context(&r->outs[i].dec);
        avcodec_free_context(&r->outs[i].enc);
        avfilter_graph_free(&r->outs[i].graph);
    }
    if (r->oc)
    {
        if (r->oc->pb)
            avio_closep(&r->oc->pb);
        avformat_free_context(r->oc);
    }
    avformat_close_input(&r->in);
    if (r->io)
    {
        av_freep(&r->io->buffer);
        avio_context_free(&r->io);
    }
    av_frame_free(&r->frame);
    av_frame_free(&r->filtered);
    av_packet_free(&r->enc_pkt);
    av_free(r);
    return e;
}
