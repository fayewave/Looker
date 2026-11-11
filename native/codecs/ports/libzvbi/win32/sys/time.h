/* Looker's MSVC build of libzvbi: struct timeval and gettimeofday() (vbi.c stamps events with it). */
#ifndef LOOKER_ZVBI_SYS_TIME_H
#define LOOKER_ZVBI_SYS_TIME_H
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <winsock2.h> /* struct timeval */
#include <windows.h>
#include <time.h>

static __inline int gettimeofday(struct timeval *tv, void *tz)
{
    FILETIME ft;
    unsigned long long t;
    (void) tz;
    GetSystemTimePreciseAsFileTime(&ft);
    t = ((unsigned long long) ft.dwHighDateTime << 32 | ft.dwLowDateTime) / 10 - 11644473600000000ULL;
    tv->tv_sec = (long) (t / 1000000);
    tv->tv_usec = (long) (t % 1000000);
    return 0;
}
#endif
