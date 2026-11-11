/* Looker's MSVC build of libzvbi: the little of pthreads the Teletext decoder uses (mutexes, pthread_once), on
   SRW locks and one-time initialisation. Build-only: the installed libzvbi.h doesn't include it. */
#ifndef LOOKER_ZVBI_PTHREAD_H
#define LOOKER_ZVBI_PTHREAD_H

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#include <errno.h>

typedef SRWLOCK pthread_mutex_t;
typedef void pthread_mutexattr_t;
#define PTHREAD_MUTEX_INITIALIZER SRWLOCK_INIT

static __inline int pthread_mutex_init(pthread_mutex_t *m, const pthread_mutexattr_t *a)
{ (void) a; InitializeSRWLock(m); return 0; }
static __inline int pthread_mutex_destroy(pthread_mutex_t *m) { (void) m; return 0; }
static __inline int pthread_mutex_lock(pthread_mutex_t *m) { AcquireSRWLockExclusive(m); return 0; }
static __inline int pthread_mutex_trylock(pthread_mutex_t *m)
{ return TryAcquireSRWLockExclusive(m) ? 0 : EBUSY; }
static __inline int pthread_mutex_unlock(pthread_mutex_t *m) { ReleaseSRWLockExclusive(m); return 0; }

typedef INIT_ONCE pthread_once_t;
#define PTHREAD_ONCE_INIT INIT_ONCE_STATIC_INIT

static BOOL CALLBACK looker_once_thunk(PINIT_ONCE once, PVOID fn, PVOID *ctx)
{ (void) once; (void) ctx; ((void (*)(void)) fn)(); return TRUE; }
static __inline int pthread_once(pthread_once_t *once, void (*fn)(void))
{ return InitOnceExecuteOnce(once, looker_once_thunk, (PVOID) fn, NULL) ? 0 : EINVAL; }

/* Only the capture threads (not built) are ever cancelled. */
#define pthread_testcancel() ((void) 0)

#endif
