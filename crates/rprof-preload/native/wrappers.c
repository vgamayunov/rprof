#define _GNU_SOURCE
#define _LARGEFILE64_SOURCE

#include "hooks.h"

#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdint.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

/* Calls made by the hooks or the dynamic loader must never enter the hooks again. */
static __thread unsigned int rprof_depth;

typedef int (*open_fn)(const char *, int, ...);
typedef int (*openat_fn)(int, const char *, int, ...);
typedef int (*creat_fn)(const char *, mode_t);
typedef int (*close_fn)(int);
typedef ssize_t (*io_fn)(int, void *, size_t);
typedef ssize_t (*write_fn)(int, const void *, size_t);
typedef ssize_t (*pio_fn)(int, void *, size_t, off_t);
typedef ssize_t (*pwrite_fn)(int, const void *, size_t, off_t);
typedef ssize_t (*pio64_fn)(int, void *, size_t, off64_t);
typedef ssize_t (*pwrite64_fn)(int, const void *, size_t, off64_t);
typedef int (*sync_fn)(int);
typedef int (*dup_fn)(int);
typedef int (*dup2_fn)(int, int);
typedef int (*dup3_fn)(int, int, int);
typedef off_t (*seek_fn)(int, off_t, int);
typedef off64_t (*seek64_fn)(int, off64_t, int);
typedef int (*fortify_open_fn)(const char *, int);
typedef int (*fortify_openat_fn)(int, const char *, int);

/* Only the first use of a symbol enters dlsym. Simultaneous first uses are safe. */
#define REAL(type, name)                                                     \
    static type real_##name;                                                  \
    static type get_##name(void) {                                            \
        type fn = __atomic_load_n(&real_##name, __ATOMIC_ACQUIRE);            \
        if (fn == NULL) {                                                     \
            fn = (type)dlsym(RTLD_NEXT, #name);                                \
            if (fn != NULL) {                                                 \
                type expected = NULL;                                         \
                if (!__atomic_compare_exchange_n(&real_##name, &expected, fn, \
                                                  0, __ATOMIC_RELEASE,        \
                                                  __ATOMIC_ACQUIRE))          \
                    fn = expected;                                            \
            }                                                                 \
        }                                                                     \
        return fn;                                                            \
    }

REAL(open_fn, open)
REAL(open_fn, open64)
REAL(openat_fn, openat)
REAL(openat_fn, openat64)
REAL(creat_fn, creat)
REAL(creat_fn, creat64)
REAL(close_fn, close)
REAL(io_fn, read)
REAL(write_fn, write)
REAL(pio_fn, pread)
REAL(pio64_fn, pread64)
REAL(pwrite_fn, pwrite)
REAL(pwrite64_fn, pwrite64)
REAL(sync_fn, fsync)
REAL(sync_fn, fdatasync)
REAL(dup_fn, dup)
REAL(dup2_fn, dup2)
REAL(dup3_fn, dup3)
REAL(seek_fn, lseek)
REAL(seek64_fn, lseek64)
REAL(fortify_open_fn, __open_2)
REAL(fortify_open_fn, __open64_2)
REAL(fortify_openat_fn, __openat_2)
REAL(fortify_openat_fn, __openat64_2)

static int needs_mode(int flags) {
    return (flags & O_CREAT) != 0 || (flags & O_TMPFILE) == O_TMPFILE;
}

/* The glibc fortified entry points must reject missing modes, including in
 * the recursive syscall path (where resolving another symbol is unsafe). */
extern void __fortify_fail(const char *) __attribute__((noreturn));
static void check_fortified_flags(int flags) {
    if (needs_mode(flags))
        __fortify_fail("invalid open call: O_CREAT or O_TMPFILE without mode");
}

/* Resolve before begin so loader activity is not counted in the IO duration.
 * Restore the caller's errno both before libc and after the end hook. */
#define START(name, op, fd_arg, other_arg, path_arg, flags_arg, offset_arg, count_arg) \
    int incoming_errno = errno;                                               \
    ++rprof_depth;                                                             \
    __typeof__(get_##name()) fn = get_##name();                                \
    errno = incoming_errno;                                                    \
    struct rprof_io_event event;                                               \
    int collect = rprof_io_begin(&event, op, fd_arg, other_arg, path_arg,      \
                                 flags_arg, offset_arg, count_arg);           \
    errno = incoming_errno

#define FINISH(value) do {                                                    \
    int call_errno = errno;                                                    \
    if (collect)                                                               \
        rprof_io_end(&event, (int64_t)(value), call_errno);                    \
    if (rprof_depth)                                                            \
        --rprof_depth;                                                         \
    errno = call_errno;                                                        \
} while (0)

#define OPEN_WRAPPER(name, operation, dirarg, syscall_dir, other_arg)         \
    int name(dirarg const char *path, int flags, ...) {                        \
        mode_t mode = 0;                                                       \
        int has_mode = needs_mode(flags);                                      \
        if (has_mode) {                                                        \
            va_list ap;                                                        \
            va_start(ap, flags);                                               \
            mode = va_arg(ap, mode_t);                                         \
            va_end(ap);                                                        \
        }                                                                     \
        if (rprof_depth)                                                       \
            return (int)syscall(SYS_openat, syscall_dir, path, flags, mode);   \
        START(name, operation, -1, other_arg, path, flags, -1, 0);            \
        int result = fn ? (has_mode ? fn(dircall path, flags, mode)            \
                                    : fn(dircall path, flags))                 \
                        : (int)syscall(SYS_openat, syscall_dir, path, flags, mode); \
        FINISH(result);                                                        \
        return result;                                                         \
    }

/* dircall is empty for open and supplies dirfd for openat. */
#define dircall
OPEN_WRAPPER(open, RPROF_OPEN, , AT_FDCWD, -1)
OPEN_WRAPPER(open64, RPROF_OPEN, , AT_FDCWD, -1)
#undef dircall
#define dircall dirfd,
#define DIRFD_PARAM int dirfd,
OPEN_WRAPPER(openat, RPROF_OPENAT, DIRFD_PARAM, dirfd, dirfd)
OPEN_WRAPPER(openat64, RPROF_OPENAT, DIRFD_PARAM, dirfd, dirfd)
#undef DIRFD_PARAM
#undef dircall
#undef OPEN_WRAPPER

#define CREAT_WRAPPER(name)                                                    \
    int name(const char *path, mode_t mode) {                                  \
        if (rprof_depth)                                                       \
            return (int)syscall(SYS_openat, AT_FDCWD, path,                    \
                                O_WRONLY | O_CREAT | O_TRUNC, mode);          \
        START(name, RPROF_CREAT, -1, -1, path,                          \
              O_WRONLY | O_CREAT | O_TRUNC, -1, 0);                             \
        int result = fn ? fn(path, mode)                                       \
                        : (int)syscall(SYS_openat, AT_FDCWD, path,             \
                                       O_WRONLY | O_CREAT | O_TRUNC, mode);     \
        FINISH(result);                                                        \
        return result;                                                         \
    }
CREAT_WRAPPER(creat)
CREAT_WRAPPER(creat64)
#undef CREAT_WRAPPER

#define SIMPLE_WRAPPER(name, operation, nr)                                   \
    int name(int fd) {                                                         \
        if (rprof_depth)                                                       \
            return (int)syscall(nr, fd);                                       \
        START(name, operation, fd, -1, NULL, 0, -1, 0);                       \
        int result = fn ? fn(fd) : (int)syscall(nr, fd);                       \
        FINISH(result);                                                        \
        return result;                                                         \
    }
SIMPLE_WRAPPER(close, RPROF_CLOSE, SYS_close)
SIMPLE_WRAPPER(fsync, RPROF_FSYNC, SYS_fsync)
SIMPLE_WRAPPER(fdatasync, RPROF_FDATASYNC, SYS_fdatasync)
SIMPLE_WRAPPER(dup, RPROF_DUP, SYS_dup)
#undef SIMPLE_WRAPPER

#define IO_WRAPPER(name, operation, nr, buffer_type)                          \
    ssize_t name(int fd, buffer_type buffer, size_t count) {                  \
        if (rprof_depth)                                                       \
            return (ssize_t)syscall(nr, fd, buffer, count);                   \
        START(name, operation, fd, -1, NULL, 0, -1, (uint64_t)count);         \
        ssize_t result = fn ? fn(fd, buffer, count)                            \
                            : (ssize_t)syscall(nr, fd, buffer, count);        \
        FINISH(result);                                                        \
        return result;                                                         \
    }
IO_WRAPPER(read, RPROF_READ, SYS_read, void *)
IO_WRAPPER(write, RPROF_WRITE, SYS_write, const void *)
#undef IO_WRAPPER

#define POSITIONAL_WRAPPER(name, operation, nr, buffer_type, offset_type)     \
    ssize_t name(int fd, buffer_type buffer, size_t count, offset_type offset) { \
        if (rprof_depth)                                                       \
            return (ssize_t)syscall(nr, fd, buffer, count, offset);           \
        START(name, operation, fd, -1, NULL, 0, (int64_t)offset,              \
              (uint64_t)count);                                                \
        ssize_t result = fn ? fn(fd, buffer, count, offset)                    \
                            : (ssize_t)syscall(nr, fd, buffer, count, offset); \
        FINISH(result);                                                        \
        return result;                                                         \
    }
POSITIONAL_WRAPPER(pread, RPROF_PREAD, SYS_pread64, void *, off_t)
POSITIONAL_WRAPPER(pread64, RPROF_PREAD, SYS_pread64, void *, off64_t)
POSITIONAL_WRAPPER(pwrite, RPROF_PWRITE, SYS_pwrite64, const void *, off_t)
POSITIONAL_WRAPPER(pwrite64, RPROF_PWRITE, SYS_pwrite64, const void *, off64_t)
#undef POSITIONAL_WRAPPER

static int raw_dup2(int oldfd, int newfd) {
#ifdef SYS_dup2
    return (int)syscall(SYS_dup2, oldfd, newfd);
#else
    if (oldfd == newfd)
        return syscall(SYS_fcntl, oldfd, F_GETFD) < 0 ? -1 : oldfd;
    return (int)syscall(SYS_dup3, oldfd, newfd, 0);
#endif
}

int dup2(int oldfd, int newfd) {
    if (rprof_depth)
        return raw_dup2(oldfd, newfd);
    START(dup2, RPROF_DUP2, oldfd, newfd, NULL, 0, -1, 0);
    int result = fn ? fn(oldfd, newfd) : raw_dup2(oldfd, newfd);
    FINISH(result);
    return result;
}

int dup3(int oldfd, int newfd, int flags) {
    if (rprof_depth)
        return (int)syscall(SYS_dup3, oldfd, newfd, flags);
    START(dup3, RPROF_DUP3, oldfd, newfd, NULL, flags, -1, 0);
    int result = fn ? fn(oldfd, newfd, flags)
                    : (int)syscall(SYS_dup3, oldfd, newfd, flags);
    FINISH(result);
    return result;
}

#define SEEK_WRAPPER(name, offset_type)                                       \
    offset_type name(int fd, offset_type offset, int whence) {                 \
        if (rprof_depth)                                                       \
            return (offset_type)syscall(SYS_lseek, fd, offset, whence);        \
        START(name, RPROF_LSEEK, fd, -1, NULL, whence, (int64_t)offset, 0);   \
        offset_type result = fn ? fn(fd, offset, whence)                       \
                                : (offset_type)syscall(SYS_lseek, fd, offset, whence); \
        FINISH(result);                                                        \
        return result;                                                         \
    }
SEEK_WRAPPER(lseek, off_t)
SEEK_WRAPPER(lseek64, off64_t)
#undef SEEK_WRAPPER

#define FORTIFIED_OPEN_WRAPPER(name, operation, dirarg, syscall_dir, other_arg) \
    int name(dirarg const char *path, int flags) {                             \
        check_fortified_flags(flags);                                          \
        if (rprof_depth)                                                       \
            return (int)syscall(SYS_openat, syscall_dir, path, flags, 0);      \
        START(name, operation, -1, other_arg, path, flags, -1, 0);            \
        int result = fn ? fn(dircall path, flags)                              \
                        : (int)syscall(SYS_openat, syscall_dir, path, flags, 0); \
        FINISH(result);                                                        \
        return result;                                                         \
    }
#define dircall
FORTIFIED_OPEN_WRAPPER(__open_2, RPROF_OPEN, , AT_FDCWD, -1)
FORTIFIED_OPEN_WRAPPER(__open64_2, RPROF_OPEN, , AT_FDCWD, -1)
#undef dircall
#define dircall dirfd,
#define DIRFD_PARAM int dirfd,
FORTIFIED_OPEN_WRAPPER(__openat_2, RPROF_OPENAT, DIRFD_PARAM, dirfd, dirfd)
FORTIFIED_OPEN_WRAPPER(__openat64_2, RPROF_OPENAT, DIRFD_PARAM, dirfd, dirfd)
#undef DIRFD_PARAM
#undef dircall
#undef FORTIFIED_OPEN_WRAPPER

static void rprof_child(void) {
    int saved_errno = errno;
    /* A child may inherit a nonzero depth if fork occurred inside a hook. */
    rprof_depth = 1;
    rprof_io_after_fork();
    rprof_depth = 0;
    errno = saved_errno;
}

__attribute__((constructor)) static void rprof_construct(void) {
    int saved_errno = errno;
    ++rprof_depth;
    /* Resolve supported libc entry points before application threads/forks start.
     * Inherited child hooks can then use read/write without entering dlsym. */
    (void)get_open(); (void)get_open64(); (void)get_openat(); (void)get_openat64();
    (void)get_creat(); (void)get_creat64(); (void)get_close();
    (void)get_read(); (void)get_write();
    (void)get_pread(); (void)get_pread64(); (void)get_pwrite(); (void)get_pwrite64();
    (void)get_fsync(); (void)get_fdatasync();
    (void)get_dup(); (void)get_dup2(); (void)get_dup3();
    (void)get_lseek(); (void)get_lseek64();
    (void)get___open_2(); (void)get___open64_2();
    (void)get___openat_2(); (void)get___openat64_2();
    rprof_io_init();
    (void)pthread_atfork(NULL, NULL, rprof_child);
    if (rprof_depth)
        --rprof_depth;
    errno = saved_errno;
}

__attribute__((destructor)) static void rprof_destruct(void) {
    int saved_errno = errno;
    ++rprof_depth;
    rprof_io_fini();
    if (rprof_depth)
        --rprof_depth;
    errno = saved_errno;
}
