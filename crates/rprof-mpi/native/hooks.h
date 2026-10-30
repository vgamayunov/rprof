#ifndef RPROF_MPI_HOOKS_H
#define RPROF_MPI_HOOKS_H
#include <stdint.h>
#include <stddef.h>

enum { RPROF_MPI_CALL=1, RPROF_MPI_REQUEST_BEGIN=2, RPROF_MPI_REQUEST_END=3,
       RPROF_MPI_COMM=4, RPROF_MPI_CLOCK=5, RPROF_MPI_METADATA=6 };
enum { RPROF_MPI_BYTES_KNOWN=1, RPROF_MPI_CANCELLED=2, RPROF_MPI_DETACHED=4,
       RPROF_MPI_TRACKING_LIMIT=8, RPROF_MPI_DETAIL_TRUNCATED=16,
       RPROF_MPI_ABORT_ENTRY=32, RPROF_MPI_REQUEST_AMBIGUOUS=64, RPROF_MPI_INTERCOMM=128 };

struct rprof_mpi_event {
    uint32_t magic;
    uint16_t version, kind;
    uint32_t pid, tid;
    uint64_t instance, start_ns, end_ns, call_id, request_id, comm_id, bytes;
    int64_t count;
    int32_t peer, tag, rank, world_size, comm_rank, comm_size, result;
    uint32_t flags;
    int32_t thread_level;
    uint32_t reserved;
    int64_t aux0;
    uint64_t aux1, aux2, aux3;
    char name[64], detail[256];
};
_Static_assert(sizeof(struct rprof_mpi_event) == 472, "MPI Rust/C ABI mismatch");

/* Constructor init maps RPROF_MPI_FD / RPROF_MPI_STATS_FD. No MPI calls here. */
void rprof_mpi_init(void);
/* Returns 0 if disabled; otherwise zeroes event and fills identity/time/name,
 * assigns a unique call_id, and sets rank/peer/etc to -1. Does not call MPI. */
int rprof_mpi_begin(struct rprof_mpi_event *event, uint16_t kind, const char *name);
/* Fills wire magic/version if needed, keeps supplied times/IDs, sends once
 * nonblocking and updates shared attempt/loss counters. */
void rprof_mpi_emit(struct rprof_mpi_event *event);
uint64_t rprof_mpi_now(void);
uint64_t rprof_mpi_next_id(void);
#endif
