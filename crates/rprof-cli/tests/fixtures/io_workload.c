#define _GNU_SOURCE
#define _LARGEFILE64_SOURCE
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

extern int __open_2(const char *, int);
extern int __openat_2(int, const char *, int);

struct thread_args { const char *path; unsigned index; };
struct cancel_args { int fd; pthread_barrier_t *barrier; };
static void *cancel_run(void *opaque) {
    struct cancel_args *args = opaque;
    char byte;
    pthread_barrier_wait(args->barrier);
    (void)read(args->fd, &byte, 1);
    abort();
}
static void *thread_run(void *opaque) {
    struct thread_args *args = opaque;
    int fd = open(args->path, O_WRONLY | O_CREAT, 0600);
    assert(fd >= 0);
    for (unsigned i = 0; i < 10; i++) {
        char value = (char)('a' + args->index);
        assert(pwrite(fd, &value, 1, args->index * 10 + i) == 1);
    }
    assert(close(fd) == 0);
    return NULL;
}

int main(int argc, char **argv) {
    assert(argc >= 3);
    const char *mode = argv[1], *path = argv[2];
    char buffer[16384] = {0};
    umask(0);
    if (!strcmp(mode, "basic")) {
        int fd = open(path, O_CREAT | O_TRUNC | O_RDWR, 0601);
        assert(fd >= 0);
        errno = EDOM;
        assert(write(fd, "abcdef", 6) == 6 && errno == EDOM);
        assert(pread(fd, buffer, 16, 0) == 6 && !memcmp(buffer, "abcdef", 6));
        assert(pwrite(fd, "XY", 2, 2) == 2);
        assert(lseek(fd, 0, SEEK_END) == 6);
        assert(read(fd, buffer, 1) == 0);
        assert(fsync(fd) == 0);
        assert(fdatasync(fd) == 0);
        assert(close(fd) == 0);
    } else if (!strcmp(mode, "errors")) {
        errno = 0;
        assert(open(path, O_RDONLY) == -1 && errno == ENOENT);
        assert(read(-1, buffer, 1) == -1 && errno == EBADF);
        assert(write(-1, buffer, 1) == -1 && errno == EBADF);
        volatile uintptr_t invalid = 1;
        assert(open((const char *)invalid, O_RDONLY) == -1 && errno == EFAULT);
        int fd = open("/dev/zero", O_RDONLY);
        assert(fd >= 0);
        assert(read(fd, (void *)invalid, 1) == -1 && errno == EFAULT);
        assert(close(fd) == 0);
    } else if (!strcmp(mode, "reuse")) {
        char second[4096];
        assert(snprintf(second, sizeof(second), "%s.second", path) > 0);
        int first = open(path, O_CREAT | O_TRUNC | O_WRONLY, 0600);
        assert(first >= 0 && write(first, "A", 1) == 1 && close(first) == 0);
        int fd = open(second, O_CREAT | O_TRUNC | O_WRONLY, 0600);
        assert(fd == first && write(fd, "B", 1) == 1);
        int alias = dup(fd);
        assert(alias >= 0 && write(alias, "C", 1) == 1);
        assert(dup2(fd, 20) == 20 && write(20, "D", 1) == 1);
        assert(dup3(fd, 21, O_CLOEXEC) == 21 && write(21, "E", 1) == 1);
        assert(close(alias) == 0 && close(20) == 0 && close(21) == 0 && close(fd) == 0);
    } else if (!strcmp(mode, "aliases")) {
        int fd = open64(path, O_CREAT | O_TRUNC | O_RDWR, 0640);
        assert(fd >= 0);
        off64_t offset = (off64_t)3 * 1024 * 1024 * 1024;
        assert(pwrite64(fd, "x", 1, offset) == 1);
        assert(pread64(fd, buffer, 1, offset) == 1 && buffer[0] == 'x');
        assert(lseek64(fd, offset, SEEK_SET) == offset);
        assert(close(fd) == 0);
        fd = __open_2(path, O_RDONLY); assert(fd >= 0 && close(fd) == 0);
        fd = __openat_2(AT_FDCWD, path, O_RDONLY); assert(fd >= 0 && close(fd) == 0);
        fd = openat64(AT_FDCWD, path, O_RDONLY); assert(fd >= 0 && close(fd) == 0);
        fd = creat64(path, 0640); assert(fd >= 0 && close(fd) == 0);
        fd = open("/tmp", O_TMPFILE | O_RDWR, 0600);
        if (fd >= 0) { struct stat stat; assert(fstat(fd, &stat) == 0 && (stat.st_mode & 0777) == 0600); assert(close(fd) == 0); }
    } else if (!strcmp(mode, "partial")) {
        int pipes[2]; assert(pipe2(pipes, O_NONBLOCK) == 0);
        while (write(pipes[1], buffer, 4096) == 4096) {}
        assert(errno == EAGAIN);
        assert(read(pipes[0], buffer, 4096) == 4096);
        ssize_t count = write(pipes[1], buffer, 8192);
        assert(count > 0 && count < 8192);
        assert(close(pipes[0]) == 0 && close(pipes[1]) == 0);
    } else if (!strcmp(mode, "untracked")) {
        /* Simulates an inherited descriptor: no interposed open established a cache. */
        int fd = (int)syscall(SYS_openat, AT_FDCWD, path, O_CREAT | O_TRUNC | O_WRONLY, 0600);
        assert(fd >= 0 && write(fd, "x", 1) == 1 && close(fd) == 0);
    } else if (!strcmp(mode, "threads")) {
        pthread_t threads[4]; struct thread_args args[4];
        for (unsigned i = 0; i < 4; i++) { args[i] = (struct thread_args){path, i}; assert(pthread_create(&threads[i], NULL, thread_run, &args[i]) == 0); }
        for (unsigned i = 0; i < 4; i++) assert(pthread_join(threads[i], NULL) == 0);
    } else if (!strcmp(mode, "cancel")) {
        int pipes[2]; assert(pipe(pipes) == 0);
        pthread_barrier_t barrier; assert(pthread_barrier_init(&barrier, NULL, 2) == 0);
        struct cancel_args args = {pipes[0], &barrier};
        pthread_t thread; assert(pthread_create(&thread, NULL, cancel_run, &args) == 0);
        pthread_barrier_wait(&barrier);
        assert(pthread_cancel(thread) == 0);
        void *result; assert(pthread_join(thread, &result) == 0 && result == PTHREAD_CANCELED);
        assert(pthread_barrier_destroy(&barrier) == 0);
        assert(close(pipes[0]) == 0 && close(pipes[1]) == 0);
    } else if (!strcmp(mode, "fork")) {
        int fd = open(path, O_CREAT | O_TRUNC | O_WRONLY, 0600); assert(fd >= 0);
        pid_t pid = fork(); assert(pid >= 0);
        if (!pid) { assert(write(fd, "child", 5) == 5 && close(fd) == 0); _exit(0); }
        int status; assert(waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0);
        assert(write(fd, "parent", 6) == 6 && close(fd) == 0);
    } else if (!strcmp(mode, "exec")) {
        int fd = open(path, O_CREAT | O_TRUNC | O_WRONLY, 0600); assert(fd >= 0);
        assert(write(fd, "parent", 6) == 6);
        char *child[] = {argv[0], "exec-child", argv[2], NULL};
        execv(argv[0], child); abort();
    } else if (!strcmp(mode, "exec-child")) {
        int fd = open(path, O_WRONLY | O_APPEND); assert(fd >= 0);
        assert(write(fd, "exec", 4) == 4 && close(fd) == 0);
    } else if (!strcmp(mode, "signal")) {
        int fd = open(path, O_CREAT | O_TRUNC | O_WRONLY, 0600); assert(fd >= 0);
        assert(write(fd, "ready", 5) == 5 && close(fd) == 0);
        for (;;) pause();
    } else if (!strcmp(mode, "bench")) {
        assert(argc == 4); unsigned long count = strtoul(argv[3], NULL, 10);
        int fd = open(path, O_CREAT | O_TRUNC | O_RDWR, 0600); assert(fd >= 0);
        for (unsigned long i = 0; i < count; i++) assert(pwrite(fd, buffer, 4096, 0) == 4096);
        assert(close(fd) == 0);
    } else { abort(); }
    return 0;
}
