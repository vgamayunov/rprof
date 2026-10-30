#ifndef RPROF_IO_HOOKS_H
#define RPROF_IO_HOOKS_H

#include <stdint.h>

enum rprof_io_op {
    RPROF_OPEN = 1, RPROF_OPENAT = 2, RPROF_CLOSE = 3, RPROF_READ = 4,
    RPROF_WRITE = 5, RPROF_PREAD = 6, RPROF_PWRITE = 7, RPROF_FSYNC = 8,
    RPROF_FDATASYNC = 9, RPROF_DUP = 10, RPROF_DUP2 = 11, RPROF_DUP3 = 12,
    RPROF_LSEEK = 13, RPROF_CREAT = 14
};

struct rprof_io_event {
    uint32_t magic;
    uint16_t version, operation;
    uint32_t pid, tid, ppid, flags;
    uint64_t instance, resource, start_ns, end_ns, requested;
    int64_t result, offset;
    int32_t fd, other_fd, error;
    uint16_t path_len, path_flags;
    uint8_t path[256];
};
_Static_assert(sizeof(struct rprof_io_event) == 352, "Rust/C event ABI mismatch");

void rprof_io_init(void);
void rprof_io_after_fork(void);
void rprof_io_fini(void);
int rprof_io_begin(struct rprof_io_event *event, uint16_t operation, int fd,
                   int other_fd, const char *path, int flags, int64_t offset,
                   uint64_t requested);
void rprof_io_end(struct rprof_io_event *event, int64_t result, int error);

#endif
