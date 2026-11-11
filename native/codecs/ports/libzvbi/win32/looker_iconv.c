/* Looker's MSVC build of libzvbi: the iconv() zvbi needs, one UCS-2 character at a time.
 *
 * Sources: UCS-2 (= UCS-2LE), UCS-2BE, UTF-16LE/BE (BMP only), UTF-8, ISO-8859-1, ASCII.
 * Targets: the same, plus any Windows code page by name ("CP1252", "WINDOWS-1252", "ISO-8859-n").
 * POSIX semantics: E2BIG when the output is full, EILSEQ for a character the target can't hold (zvbi substitutes
 * its replacement and carries on), EINVAL for a sequence cut off at the end of the input.
 *
 * Written for Looker's port; under zvbi's own licence (LGPL-2.0+) like the library it is built into. */
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>
#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include <ctype.h>

#include "iconv.h"

enum kind { K_UCS2LE, K_UCS2BE, K_UTF8, K_LATIN1, K_ASCII, K_CODEPAGE };

struct looker_iconv {
    enum kind from, to;
    UINT to_cp;
};

static int parse(const char *name, enum kind *kind, UINT *cp)
{
    char n[64];
    size_t i, j = 0;

    if (!name)
        return 0;
    for (i = 0; name[i] && j + 1 < sizeof n; i++) {
        if (name[i] == '-' || name[i] == '_')
            continue;
        if (name[i] == '/')
            break; /* "//TRANSLIT" and the like */
        n[j++] = (char) toupper((unsigned char) name[i]);
    }
    n[j] = 0;
    *cp = 0;

    if (!strcmp(n, "UCS2") || !strcmp(n, "UCS2LE") || !strcmp(n, "UTF16LE") || !strcmp(n, "UNICODELITTLE"))
        *kind = K_UCS2LE;
    else if (!strcmp(n, "UCS2BE") || !strcmp(n, "UTF16BE") || !strcmp(n, "UNICODEBIG"))
        *kind = K_UCS2BE;
    else if (!strcmp(n, "UTF8"))
        *kind = K_UTF8;
    else if (!strcmp(n, "ISO88591") || !strcmp(n, "LATIN1") || !strcmp(n, "L1"))
        *kind = K_LATIN1;
    else if (!strcmp(n, "ASCII") || !strcmp(n, "USASCII") || !strcmp(n, "ANSIX3.41968"))
        *kind = K_ASCII;
    else {
        if (!strncmp(n, "ISO8859", 7) && n[7] >= '1' && n[7] <= '9')
            *cp = 28590 + (UINT) atoi(n + 7);
        else if (!strncmp(n, "WINDOWS", 7) && isdigit((unsigned char) n[7]))
            *cp = (UINT) atoi(n + 7);
        else if (!strncmp(n, "CP", 2) && isdigit((unsigned char) n[2]))
            *cp = (UINT) atoi(n + 2);
        if (!*cp || !IsValidCodePage(*cp))
            return 0;
        *kind = K_CODEPAGE;
    }
    return 1;
}

iconv_t iconv_open(const char *tocode, const char *fromcode)
{
    struct looker_iconv cd, *p;
    UINT from_cp;

    if (!parse(tocode, &cd.to, &cd.to_cp) || !parse(fromcode, &cd.from, &from_cp) || cd.from == K_CODEPAGE) {
        errno = EINVAL;
        return (iconv_t) -1;
    }
    p = malloc(sizeof *p);
    if (!p) {
        errno = ENOMEM;
        return (iconv_t) -1;
    }
    *p = cd;
    return p;
}

int iconv_close(iconv_t cd)
{
    if (cd == (iconv_t) -1 || !cd) {
        errno = EBADF;
        return -1;
    }
    free(cd);
    return 0;
}

/* One character from the input: its UCS-2 value and length in bytes, 0 when cut off, -1 when invalid. */
static int decode(enum kind k, const unsigned char *s, size_t n, unsigned *c)
{
    switch (k) {
    case K_UCS2LE:
        if (n < 2)
            return 0;
        *c = s[0] | s[1] << 8;
        return 2;
    case K_UCS2BE:
        if (n < 2)
            return 0;
        *c = s[0] << 8 | s[1];
        return 2;
    case K_LATIN1:
        *c = s[0];
        return 1;
    case K_ASCII:
        if (s[0] >= 0x80)
            return -1;
        *c = s[0];
        return 1;
    case K_UTF8:
        if (s[0] < 0x80) {
            *c = s[0];
            return 1;
        }
        if (s[0] >= 0xC2 && s[0] < 0xE0) {
            if (n < 2)
                return 0;
            if ((s[1] & 0xC0) != 0x80)
                return -1;
            *c = (s[0] & 0x1F) << 6 | (s[1] & 0x3F);
            return 2;
        }
        if (s[0] >= 0xE0 && s[0] < 0xF0) {
            if (n < 3)
                return n == 2 && (s[1] & 0xC0) != 0x80 ? -1 : 0;
            if ((s[1] & 0xC0) != 0x80 || (s[2] & 0xC0) != 0x80)
                return -1;
            *c = (s[0] & 0x0F) << 12 | (s[1] & 0x3F) << 6 | (s[2] & 0x3F);
            if (*c < 0x800 || (*c >= 0xD800 && *c < 0xE000))
                return -1;
            return 3;
        }
        return -1; /* beyond the BMP, or not UTF-8: UCS-2 can't hold it */
    default:
        return -1;
    }
}

/* One character to the output: bytes written, 0 when it doesn't fit, -1 when the target can't represent it. */
static int encode(const struct looker_iconv *cd, unsigned c, unsigned char *d, size_t n)
{
    switch (cd->to) {
    case K_UCS2LE:
    case K_UCS2BE:
        if (n < 2)
            return 0;
        d[cd->to == K_UCS2BE] = (unsigned char) c;
        d[cd->to != K_UCS2BE] = (unsigned char) (c >> 8);
        return 2;
    case K_LATIN1:
    case K_ASCII:
        if (c >= (cd->to == K_ASCII ? 0x80u : 0x100u))
            return -1;
        if (n < 1)
            return 0;
        d[0] = (unsigned char) c;
        return 1;
    case K_UTF8:
        if (c < 0x80) {
            if (n < 1)
                return 0;
            d[0] = (unsigned char) c;
            return 1;
        }
        if (c < 0x800) {
            if (n < 2)
                return 0;
            d[0] = (unsigned char) (0xC0 | c >> 6);
            d[1] = (unsigned char) (0x80 | (c & 0x3F));
            return 2;
        }
        if (n < 3)
            return 0;
        d[0] = (unsigned char) (0xE0 | c >> 12);
        d[1] = (unsigned char) (0x80 | (c >> 6 & 0x3F));
        d[2] = (unsigned char) (0x80 | (c & 0x3F));
        return 3;
    case K_CODEPAGE: {
        WCHAR w = (WCHAR) c;
        char buf[8];
        BOOL used_default = FALSE;
        int r = WideCharToMultiByte(cd->to_cp, WC_NO_BEST_FIT_CHARS, &w, 1, buf, sizeof buf, NULL, &used_default);
        if (r <= 0) /* some code pages refuse the flags */
            r = WideCharToMultiByte(cd->to_cp, 0, &w, 1, buf, sizeof buf, NULL, NULL);
        if (r <= 0 || used_default)
            return -1;
        if ((size_t) r > n)
            return 0;
        memcpy(d, buf, (size_t) r);
        return r;
    }
    }
    return -1;
}

size_t iconv(iconv_t cd, char **inbuf, size_t *inbytesleft, char **outbuf, size_t *outbytesleft)
{
    if (cd == (iconv_t) -1 || !cd) {
        errno = EBADF;
        return (size_t) -1;
    }
    if (!inbuf || !*inbuf)
        return 0; /* stateless: nothing to reset or flush */

    while (*inbytesleft > 0) {
        unsigned c;
        int in = decode(cd->from, (const unsigned char *) *inbuf, *inbytesleft, &c);
        int out;

        if (in == 0) {
            errno = EINVAL;
            return (size_t) -1;
        }
        if (in < 0) {
            errno = EILSEQ;
            return (size_t) -1;
        }
        out = encode(cd, c, (unsigned char *) *outbuf, *outbytesleft);
        if (out == 0) {
            errno = E2BIG;
            return (size_t) -1;
        }
        if (out < 0) {
            errno = EILSEQ;
            return (size_t) -1;
        }
        *inbuf += in;
        *inbytesleft -= (size_t) in;
        *outbuf += out;
        *outbytesleft -= (size_t) out;
    }
    return 0;
}
