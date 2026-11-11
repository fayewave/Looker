/* Looker's MSVC build of libzvbi: a small iconv() (looker_iconv.c) in place of GNU libiconv, enough for what zvbi
   converts: UCS-2 (host order, little-endian here) to and from UTF-8, Latin-1, ASCII and Windows code pages. */
#ifndef LOOKER_ZVBI_ICONV_H
#define LOOKER_ZVBI_ICONV_H
#include <stddef.h>

typedef struct looker_iconv *iconv_t;

iconv_t iconv_open(const char *tocode, const char *fromcode);
size_t iconv(iconv_t cd, char **inbuf, size_t *inbytesleft, char **outbuf, size_t *outbytesleft);
int iconv_close(iconv_t cd);

#endif
