// Looker's one entry point into LibRaw (compiled into raw_r.dll by the overlay port): LibRaw's C API has no
// setters for the camera's white balance or half-size decoding, so this sets them on the C++ object directly.
// Returns 0 with straight RGBA (upright: LibRaw applies the file's flip) in a buffer freed by looker_raw_free and the
// capture time in Unix seconds (0 when the file has none), or a LibRaw error code (looker_raw_error names it).
// Never lets an exception out.

#include "libraw/libraw.h"

#include <algorithm>
#include <cstdlib>
#include <new>

extern "C" __declspec(dllexport) int looker_raw_decode(const void *data, size_t size, int box_w, int box_h, int *out_w, int *out_h,
                                                       int *full_w, int *full_h, long long *taken, unsigned char **out_rgba)
{
    *out_rgba = nullptr;
    *taken = 0;
    try
    {
        LibRaw *rp = new (std::nothrow) LibRaw(0);
        if (!rp)
            return LIBRAW_UNSUFFICIENT_MEMORY;
        struct Free
        {
            LibRaw *p;
            ~Free()
            {
                p->recycle();
                delete p;
            }
        } free_rp{rp};

        libraw_output_params_t &P = rp->imgdata.params;
        P.use_camera_wb = 1; // the colours the camera saw, not dcraw's daylight default
        P.output_bps = 8;
        P.output_color = 1; // sRGB

        int r = rp->open_buffer(data, size);
        if (r != LIBRAW_SUCCESS)
            return r;
        int fw = rp->imgdata.sizes.width, fh = rp->imgdata.sizes.height;
        if (rp->imgdata.sizes.flip & 4)
            std::swap(fw, fh);
        *full_w = fw;
        *full_h = fh;
        *taken = (long long)rp->imgdata.other.timestamp;
        // Half size skips demosaicing (about 4x faster) and still covers a box at most half the image.
        if (box_w > 0 && box_h > 0 && fw > 0 && fh > 0 && std::min((double)box_w / fw, (double)box_h / fh) <= 0.5)
            P.half_size = 1;

        if ((r = rp->unpack()) != LIBRAW_SUCCESS)
            return r;
        if ((r = rp->dcraw_process()) != LIBRAW_SUCCESS)
            return r;
        int err = 0;
        libraw_processed_image_t *img = rp->dcraw_make_mem_image(&err);
        if (!img)
            return err ? err : LIBRAW_UNSPECIFIED_ERROR;
        if (img->type != LIBRAW_IMAGE_BITMAP || img->bits != 8 || (img->colors != 3 && img->colors != 1))
        {
            LibRaw::dcraw_clear_mem(img);
            return LIBRAW_UNSPECIFIED_ERROR;
        }
        size_t n = (size_t)img->width * img->height;
        unsigned char *rgba = (unsigned char *)std::malloc(n * 4);
        if (!rgba)
        {
            LibRaw::dcraw_clear_mem(img);
            return LIBRAW_UNSUFFICIENT_MEMORY;
        }
        const unsigned char *s = img->data;
        for (size_t i = 0; i < n; i++)
        {
            unsigned char *o = rgba + i * 4;
            if (img->colors == 3)
            {
                o[0] = s[i * 3];
                o[1] = s[i * 3 + 1];
                o[2] = s[i * 3 + 2];
            }
            else
                o[0] = o[1] = o[2] = s[i];
            o[3] = 255;
        }
        *out_w = img->width;
        *out_h = img->height;
        *out_rgba = rgba;
        LibRaw::dcraw_clear_mem(img);
        return 0;
    }
    catch (...)
    {
        return LIBRAW_UNSPECIFIED_ERROR;
    }
}

extern "C" __declspec(dllexport) void looker_raw_free(unsigned char *p)
{
    std::free(p);
}

extern "C" __declspec(dllexport) const char *looker_raw_error(int code)
{
    return libraw_strerror(code);
}
