#include "hooks.h"
#include <mpi.h>
#include <limits.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* No MPI operation is performed by the constructor. */
__attribute__((constructor)) static void rprof_ctor(void) { rprof_mpi_init(); }

static _Thread_local int inside;
static int world_rank = -1, world_size = -1, thread_level = -1;
static int sync_clock;
static MPI_Comm clock_comm = MPI_COMM_NULL;
static uint64_t clock_group;

#define MAX_REQUESTS 8192
#define MAX_SCRATCH 4096
#define MAX_COMMS 2048
struct request_info {
    MPI_Request handle;
    MPI_Request *storage;
    struct rprof_mpi_event begin;
    int used;
};
struct comm_info { MPI_Comm handle; uint64_t id; int rank, size, inter, used; };
static struct request_info requests[MAX_REQUESTS];
static struct comm_info comms[MAX_COMMS];
static pthread_mutex_t request_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_mutex_t comm_lock = PTHREAD_MUTEX_INITIALIZER;

static int start(struct rprof_mpi_event *e, uint16_t kind, const char *name) {
    if (inside) return 0;
    inside = 1;
    if (!rprof_mpi_begin(e, kind, name)) { inside = 0; return 0; }
    e->rank = world_rank;
    e->world_size = world_size;
    e->thread_level = thread_level;
    return 1;
}
static void finish(struct rprof_mpi_event *e, int rc) {
    if (!e->end_ns) e->end_ns = rprof_mpi_now();
    e->result = rc;
    rprof_mpi_emit(e);
    inside = 0;
}
static void emit_aux(struct rprof_mpi_event *e) { rprof_mpi_emit(e); }
static uint64_t comm_id(MPI_Comm c) {
    if (c == MPI_COMM_WORLD) return 1;
    if (c == MPI_COMM_SELF) return 2;
    if (c == MPI_COMM_NULL) return 0;
    uint64_t id = 0;
    pthread_mutex_lock(&comm_lock);
    for (int i = 0; i < MAX_COMMS; ++i)
        if (comms[i].used && comms[i].handle == c) { id = comms[i].id; break; }
    pthread_mutex_unlock(&comm_lock);
    return id;
}
static uint64_t remember_comm(MPI_Comm c) {
    if (c == MPI_COMM_NULL) return 0;
    uint64_t id = rprof_mpi_next_id() | (UINT64_C(1) << 63);
    int saved = 0, rank = -1, size = -1, inter = 0;
    if (PMPI_Comm_rank(c, &rank) != MPI_SUCCESS ||
        PMPI_Comm_size(c, &size) != MPI_SUCCESS || PMPI_Comm_test_inter(c, &inter) != MPI_SUCCESS) return 0;
    pthread_mutex_lock(&comm_lock);
    for (int i = 0; i < MAX_COMMS; ++i)
        if (!comms[i].used) { comms[i] = (struct comm_info){c, id, rank, size, inter, 1}; saved = 1; break; }
    pthread_mutex_unlock(&comm_lock);
    return saved ? id : 0;
}
static void forget_comm(MPI_Comm c) {
    pthread_mutex_lock(&comm_lock);
    for (int i = 0; i < MAX_COMMS; ++i)
        if (comms[i].used && comms[i].handle == c) { comms[i].used = 0; break; }
    pthread_mutex_unlock(&comm_lock);
}
static void comm_fields(struct rprof_mpi_event *e, MPI_Comm c) {
    if (c == MPI_COMM_WORLD) {
        e->comm_id = 1; e->comm_rank = world_rank; e->comm_size = world_size;
    } else if (c == MPI_COMM_SELF) {
        e->comm_id = 2; e->comm_rank = 0; e->comm_size = 1;
    } else if (c != MPI_COMM_NULL) {
        pthread_mutex_lock(&comm_lock);
        for (int i = 0; i < MAX_COMMS; ++i)
            if (comms[i].used && comms[i].handle == c) {
                e->comm_id = comms[i].id;
                e->comm_rank = comms[i].rank;
                e->comm_size = comms[i].size;
                if (comms[i].inter) e->flags |= RPROF_MPI_INTERCOMM;
                break;
            }
        pthread_mutex_unlock(&comm_lock);
    }
}
static void bytes_fields(struct rprof_mpi_event *e, int count, MPI_Datatype type) {
    e->count = count;
    e->flags &= ~RPROF_MPI_BYTES_KNOWN;
    if (e->peer == MPI_PROC_NULL) { e->bytes = 0; e->flags |= RPROF_MPI_BYTES_KNOWN; return; }
    /* Unknown/intercommunicator contexts can have ignored datatype arguments. */
    if (!e->comm_id || (e->flags & RPROF_MPI_INTERCOMM)) return;
    if (count < 0 || type == MPI_DATATYPE_NULL) return;
    MPI_Count size = 0;
    if (PMPI_Type_size_x(type, &size) == MPI_SUCCESS && size >= 0 &&
        (count == 0 || (uint64_t)size <= UINT64_MAX / (uint64_t)count)) {
        e->bytes = (uint64_t)size * (uint64_t)count;
        e->flags |= RPROF_MPI_BYTES_KNOWN;
    }
}
static void received(struct rprof_mpi_event *e, MPI_Status *s, MPI_Datatype type) {
    if (!s || s == MPI_STATUS_IGNORE) return;
    e->peer = s->MPI_SOURCE;
    e->tag = s->MPI_TAG;
    e->flags &= ~RPROF_MPI_BYTES_KNOWN;
    MPI_Count n;
    if (type != MPI_DATATYPE_NULL && PMPI_Get_elements_x(s, MPI_BYTE, &n) == MPI_SUCCESS && n >= 0) {
        e->bytes = (uint64_t)n;
        e->flags |= RPROF_MPI_BYTES_KNOWN;
    }
}
static int track(MPI_Request h, MPI_Request *storage, struct rprof_mpi_event *call) {
    if (h == MPI_REQUEST_NULL) return 1;
    struct rprof_mpi_event b = *call;
    b.kind = RPROF_MPI_REQUEST_BEGIN;
    b.request_id = rprof_mpi_next_id();
    pthread_mutex_lock(&request_lock);
    int slot = -1;
    for (int i = 0; i < MAX_REQUESTS; ++i)
        if (!requests[i].used) { slot = i; requests[i] = (struct request_info){h, storage, b, 1}; break; }
    pthread_mutex_unlock(&request_lock);
    if (slot < 0) return 0;
    /* The initiating CALL and BEGIN share the request id and initiation call id. */
    call->request_id = b.request_id;
    emit_aux(&b);
    return 1;
}
static void end_request(MPI_Request h, MPI_Request *storage, const struct rprof_mpi_event *call,
                        MPI_Status *status, int detached) {
    if (h == MPI_REQUEST_NULL) return;
    struct rprof_mpi_event e;
    int exact = 0, candidates = 0, chosen = -1;
    pthread_mutex_lock(&request_lock);
    for (int i = 0; i < MAX_REQUESTS; ++i)
        if (requests[i].used && requests[i].handle == h && storage && requests[i].storage == storage) {
            if (!exact) chosen = i;
            ++exact;
        }
    if (!exact) {
        for (int i = 0; i < MAX_REQUESTS; ++i)
            if (requests[i].used && requests[i].handle == h) {
                if (!candidates) chosen = i;
                ++candidates;
            }
    }
    if (chosen >= 0) {
        if (exact > 1 || candidates > 1)
            for (int i = 0; i < MAX_REQUESTS; ++i)
                if (requests[i].used && requests[i].handle == h &&
                    (!exact || requests[i].storage == storage))
                    requests[i].begin.flags |= RPROF_MPI_REQUEST_AMBIGUOUS;
        e = requests[chosen].begin;
        requests[chosen].used = 0;
    }
    pthread_mutex_unlock(&request_lock);
    if (chosen < 0) return; /* e.g. persistent requests are intentionally unsupported */
    e.kind = RPROF_MPI_REQUEST_END;
    e.call_id = call->call_id;
    e.end_ns = call->end_ns;
    e.result = call->result;
    if (call->result == MPI_ERR_IN_STATUS && status && status != MPI_STATUS_IGNORE)
        e.result = status->MPI_ERROR;
    if (detached) e.flags |= RPROF_MPI_DETACHED;
    if (!detached && strcmp(e.name, "MPI_Irecv") == 0) {
        e.flags &= ~RPROF_MPI_BYTES_KNOWN;
        e.bytes = 0;
    }
    if (status && status != MPI_STATUS_IGNORE && !detached) {
        int cancelled = 0;
        if (PMPI_Test_cancelled(status, &cancelled) == MPI_SUCCESS && cancelled)
            e.flags |= RPROF_MPI_CANCELLED;
        if (strcmp(e.name, "MPI_Irecv") == 0) {
            if (cancelled) { e.bytes = 0; e.flags |= RPROF_MPI_BYTES_KNOWN; }
            else if (e.result == MPI_SUCCESS) received(&e, status, MPI_BYTE);
            else { e.peer = status->MPI_SOURCE; e.tag = status->MPI_TAG; }
        }
    }
    emit_aux(&e);
}
/* No snapshot means handles could be recycled invisibly by the array operation.
 * Retire the entire bounded map explicitly, rather than misattribute a reuse. */
static void abandon_tracking(const struct rprof_mpi_event *call) {
    for (int i = 0; i < MAX_REQUESTS; ++i) {
        struct rprof_mpi_event e;
        int found = 0;
        pthread_mutex_lock(&request_lock);
        if (requests[i].used) {
            e = requests[i].begin; requests[i].used = 0; found = 1;
        }
        pthread_mutex_unlock(&request_lock);
        if (found) {
            e.kind = RPROF_MPI_REQUEST_END;
            e.call_id = call->call_id;
            e.end_ns = call->start_ns;
            e.flags |= RPROF_MPI_TRACKING_LIMIT;
            snprintf(e.detail, sizeof e.detail, "tracking abandoned: request snapshot unavailable");
            emit_aux(&e);
        }
    }
}
static void metadata(void) {
    struct rprof_mpi_event e;
    if (!rprof_mpi_begin(&e, RPROF_MPI_METADATA, "MPI_Get_library_version")) return;
    e.rank = world_rank; e.world_size = world_size; e.thread_level = thread_level;
    char text[MPI_MAX_LIBRARY_VERSION_STRING];
    int len = 0;
    int rc = PMPI_Get_library_version(text, &len);
    if (rc == MPI_SUCCESS && len > 0) {
        size_t n = (size_t)len;
        if (n >= sizeof e.detail) { n = sizeof e.detail - 1; e.flags |= RPROF_MPI_DETAIL_TRUNCATED; }
        memcpy(e.detail, text, n);
        e.detail[n] = 0;
    }
    e.end_ns = rprof_mpi_now(); e.result = rc; emit_aux(&e);
}
static void clock_sample(int stage) {
    if (clock_comm == MPI_COMM_NULL) return;
    int r = world_rank, n = world_size;
    int64_t offset = 0;
    uint64_t uncertainty = 0, token = clock_group, anchor = 0;
    if (PMPI_Bcast(&token, 1, MPI_UINT64_T, 0, clock_comm) != MPI_SUCCESS) return;
    clock_group = token;
    if (r == 0) {
        for (int p = 1; p < n; ++p) {
            uint64_t best = UINT64_MAX;
            int64_t best_offset = 0;
            uint64_t best_anchor = 0;
            for (int i = 0; i < 8; ++i) {
                uint64_t t0 = rprof_mpi_now(), peer_time = 0;
                if (PMPI_Send(&t0, 1, MPI_UINT64_T, p, 0, clock_comm) != MPI_SUCCESS) return;
                if (PMPI_Recv(&peer_time, 1, MPI_UINT64_T, p, 1, clock_comm, MPI_STATUS_IGNORE) != MPI_SUCCESS) return;
                uint64_t t1 = rprof_mpi_now(), rtt = t1 - t0;
                if (rtt < best) { best = rtt; best_offset = (int64_t)(t0 + rtt / 2) - (int64_t)peer_time; best_anchor = peer_time; }
            }
            int64_t values[3] = {best_offset, (int64_t)(best / 2), (int64_t)best_anchor};
            if (PMPI_Send(values, 3, MPI_INT64_T, p, 2, clock_comm) != MPI_SUCCESS) return;
        }
        anchor = rprof_mpi_now();
    } else {
        for (int i = 0; i < 8; ++i) {
            uint64_t ping = 0, t;
            if (PMPI_Recv(&ping, 1, MPI_UINT64_T, 0, 0, clock_comm, MPI_STATUS_IGNORE) != MPI_SUCCESS) return;
            t = rprof_mpi_now();
            if (PMPI_Send(&t, 1, MPI_UINT64_T, 0, 1, clock_comm) != MPI_SUCCESS) return;
        }
        int64_t values[3] = {0, 0, 0};
        if (PMPI_Recv(values, 3, MPI_INT64_T, 0, 2, clock_comm, MPI_STATUS_IGNORE) != MPI_SUCCESS) return;
        offset = values[0]; uncertainty = (uint64_t)values[1]; anchor = (uint64_t)values[2];
    }
    struct rprof_mpi_event e;
    /* inside is set by the enclosing Init/Finalize wrapper. */
    if (rprof_mpi_begin(&e, RPROF_MPI_CLOCK, "clock_calibration")) {
        e.rank = r; e.world_size = n; e.thread_level = thread_level;
        e.start_ns = e.end_ns = anchor;
        e.aux0 = offset; e.aux1 = uncertainty; e.aux2 = token; e.aux3 = (uint64_t)stage;
        emit_aux(&e);
    }
}
static void initialized(int level) {
    PMPI_Comm_rank(MPI_COMM_WORLD, &world_rank);
    PMPI_Comm_size(MPI_COMM_WORLD, &world_size);
    thread_level = level;
    metadata();
    const char *env = getenv("RPROF_MPI_CLOCK_SYNC");
    sync_clock = env && strcmp(env, "1") == 0;
    if (sync_clock && PMPI_Comm_dup(MPI_COMM_WORLD, &clock_comm) == MPI_SUCCESS) {
        if (world_rank == 0) {
            struct rprof_mpi_event identity;
            if (rprof_mpi_begin(&identity, RPROF_MPI_CLOCK, "clock_group"))
                clock_group = identity.instance ^ rprof_mpi_next_id();
            else clock_group = rprof_mpi_next_id() ^ rprof_mpi_now();
        }
        clock_sample(0);
    }
}

int MPI_Init(int *argc, char ***argv) {
    struct rprof_mpi_event e; int active = start(&e, RPROF_MPI_CALL, "MPI_Init");
    int rc = PMPI_Init(argc, argv);
    if (active) { e.end_ns = rprof_mpi_now(); e.result = rc; if (rc == MPI_SUCCESS) initialized(MPI_THREAD_SINGLE); e.rank = world_rank; e.world_size = world_size; e.thread_level = thread_level; emit_aux(&e); inside = 0; }
    return rc;
}
int MPI_Init_thread(int *argc, char ***argv, int required, int *provided) {
    struct rprof_mpi_event e; int active = start(&e, RPROF_MPI_CALL, "MPI_Init_thread");
    int rc = PMPI_Init_thread(argc, argv, required, provided);
    if (active) { e.end_ns = rprof_mpi_now(); e.result = rc; if (rc == MPI_SUCCESS && provided) initialized(*provided); e.rank = world_rank; e.world_size = world_size; e.thread_level = thread_level; emit_aux(&e); inside = 0; }
    return rc;
}
int MPI_Finalize(void) {
    struct rprof_mpi_event e; int active = start(&e, RPROF_MPI_CALL, "MPI_Finalize");
    if (active && clock_comm != MPI_COMM_NULL) { clock_sample(1); PMPI_Comm_free(&clock_comm); }
    if (active) e.start_ns = rprof_mpi_now();
    int rc = PMPI_Finalize();
    if (active) finish(&e, rc);
    return rc;
}
int MPI_Abort(MPI_Comm c, int errorcode) {
    struct rprof_mpi_event e; int active = start(&e, RPROF_MPI_CALL, "MPI_Abort");
    if (active) { comm_fields(&e, c); e.flags |= RPROF_MPI_ABORT_ENTRY; e.result = errorcode; e.end_ns = rprof_mpi_now(); emit_aux(&e); }
    int rc = PMPI_Abort(c, errorcode);
    if (active) inside = 0;
    return rc;
}

#define SEND_WRAPPER(NAME) \
int MPI_##NAME(const void *buf, int count, MPI_Datatype type, int dest, int tag, MPI_Comm comm) { \
    struct rprof_mpi_event e; int a = start(&e, RPROF_MPI_CALL, "MPI_" #NAME); \
    if (a) { comm_fields(&e, comm); e.peer = dest; e.tag = tag; e.count = count; e.start_ns = rprof_mpi_now(); } \
    int rc = PMPI_##NAME(buf, count, type, dest, tag, comm); \
    if (a) { e.end_ns = rprof_mpi_now(); if (rc == MPI_SUCCESS) bytes_fields(&e, count, type); finish(&e, rc); } \
    return rc; \
}
SEND_WRAPPER(Send) SEND_WRAPPER(Ssend) SEND_WRAPPER(Bsend) SEND_WRAPPER(Rsend)

int MPI_Recv(void *buf, int count, MPI_Datatype type, int source, int tag, MPI_Comm comm, MPI_Status *status) {
    struct rprof_mpi_event e; int a = start(&e, RPROF_MPI_CALL, "MPI_Recv");
    if (a) { comm_fields(&e, comm); e.peer = source; e.tag = tag; e.count = count; e.start_ns = rprof_mpi_now(); }
    MPI_Status private_status;
    MPI_Status *s = a && status == MPI_STATUS_IGNORE ? &private_status : status;
    int rc = PMPI_Recv(buf, count, type, source, tag, comm, s);
    if (a) { e.end_ns = rprof_mpi_now(); if (rc == MPI_SUCCESS) received(&e, s, type); finish(&e, rc); } return rc;
}
int MPI_Sendrecv(const void *sendbuf, int sendcount, MPI_Datatype sendtype, int dest, int sendtag,
                 void *recvbuf, int recvcount, MPI_Datatype recvtype, int source, int recvtag,
                 MPI_Comm comm, MPI_Status *status) {
    struct rprof_mpi_event e; int a = start(&e, RPROF_MPI_CALL, "MPI_Sendrecv");
    if (a) { comm_fields(&e, comm); e.peer = dest; e.tag = sendtag; e.count = sendcount; e.start_ns = rprof_mpi_now(); }
    MPI_Status private_status;
    MPI_Status *s = a && status == MPI_STATUS_IGNORE ? &private_status : status;
    int rc = PMPI_Sendrecv(sendbuf, sendcount, sendtype, dest, sendtag, recvbuf, recvcount, recvtype, source, recvtag, comm, s);
    if (a) { e.end_ns = rprof_mpi_now(); if (rc == MPI_SUCCESS) {
        bytes_fields(&e, sendcount, sendtype);
        MPI_Count n = MPI_UNDEFINED;
        int known = PMPI_Get_elements_x(s, MPI_BYTE, &n) == MPI_SUCCESS && n >= 0;
        e.aux0 = s->MPI_SOURCE; e.aux1 = (uint64_t)(int64_t)s->MPI_TAG;
        e.aux2 = known ? (uint64_t)n : 0;
        e.aux3 = (uint64_t)(int64_t)recvcount;
        snprintf(e.detail, sizeof e.detail, "send and receive local logical bytes (not network volume); recv_source=%d recv_tag=%d recv_bytes=%s%llu recv_capacity_count=%d",
                 s->MPI_SOURCE, s->MPI_TAG, known ? "" : "unknown:",
                 (unsigned long long)e.aux2, recvcount);
    } finish(&e, rc); } return rc;
}
#define ISEND_WRAPPER(NAME) \
int MPI_##NAME(const void *buf, int count, MPI_Datatype type, int dest, int tag, MPI_Comm comm, MPI_Request *request) { \
    struct rprof_mpi_event e; int a = start(&e, RPROF_MPI_CALL, "MPI_" #NAME); \
    if (a) { comm_fields(&e, comm); e.peer = dest; e.tag = tag; e.count = count; e.start_ns = rprof_mpi_now(); } \
    int rc = PMPI_##NAME(buf, count, type, dest, tag, comm, request); \
    if (a) { e.end_ns = rprof_mpi_now(); e.result = rc; if (rc == MPI_SUCCESS) { bytes_fields(&e, count, type); if (request && *request != MPI_REQUEST_NULL && !track(*request, request, &e)) e.flags |= RPROF_MPI_TRACKING_LIMIT; } emit_aux(&e); inside = 0; } return rc; \
}
ISEND_WRAPPER(Isend) ISEND_WRAPPER(Issend) ISEND_WRAPPER(Ibsend) ISEND_WRAPPER(Irsend)
int MPI_Irecv(void *buf, int count, MPI_Datatype type, int source, int tag, MPI_Comm comm, MPI_Request *request) {
    struct rprof_mpi_event e; int a = start(&e, RPROF_MPI_CALL, "MPI_Irecv");
    if (a) { comm_fields(&e, comm); e.peer = source; e.tag = tag; e.count = count; e.start_ns = rprof_mpi_now(); }
    int rc = PMPI_Irecv(buf, count, type, source, tag, comm, request);
    if (a) { e.end_ns = rprof_mpi_now(); e.result = rc; if (rc == MPI_SUCCESS) { bytes_fields(&e, count, type); if (request && *request != MPI_REQUEST_NULL && !track(*request, request, &e)) e.flags |= RPROF_MPI_TRACKING_LIMIT; } emit_aux(&e); inside = 0; } return rc;
}

int MPI_Wait(MPI_Request *request, MPI_Status *status) {
    struct rprof_mpi_event e; int a = start(&e, RPROF_MPI_CALL, "MPI_Wait");
    MPI_Request old = a && request ? *request : MPI_REQUEST_NULL;
    MPI_Status private_status;
    MPI_Status *s = a && status == MPI_STATUS_IGNORE ? &private_status : status;
    int rc = PMPI_Wait(request, s);
    if (a) { e.end_ns = rprof_mpi_now(); e.result = rc; emit_aux(&e); if (request && *request == MPI_REQUEST_NULL) end_request(old, request, &e, rc == MPI_SUCCESS ? s : NULL, 0); inside = 0; } return rc;
}
int MPI_Test(MPI_Request *request, int *flag, MPI_Status *status) {
    struct rprof_mpi_event e; int a = start(&e, RPROF_MPI_CALL, "MPI_Test");
    MPI_Request old = a && request ? *request : MPI_REQUEST_NULL;
    MPI_Status private_status;
    MPI_Status *s = a && status == MPI_STATUS_IGNORE ? &private_status : status;
    int rc = PMPI_Test(request, flag, s);
    if (a) { e.end_ns = rprof_mpi_now(); e.result = rc; emit_aux(&e); if (request && *request == MPI_REQUEST_NULL && (rc != MPI_SUCCESS || (flag && *flag))) end_request(old, request, &e, rc == MPI_SUCCESS ? s : NULL, 0); inside = 0; } return rc;
}

struct scratch { MPI_Request *old; MPI_Status *statuses; };
static struct scratch snapshot(int count, MPI_Request *array, int need_status) {
    struct scratch s = {0};
    if (count < 0 || count > MAX_SCRATCH || !array) return s;
    s.old = malloc((size_t)(count ? count : 1) * sizeof *s.old);
    if (s.old) memcpy(s.old, array, (size_t)count * sizeof *s.old);
    if (need_status) s.statuses = malloc((size_t)(count ? count : 1) * sizeof *s.statuses);
    return s;
}
static void release_scratch(struct scratch *s) { free(s->old); free(s->statuses); }
#define ARRAY_SETUP(NAME, STATUSES) \
    struct rprof_mpi_event e; int a = start(&e, RPROF_MPI_CALL, "MPI_" #NAME); \
    struct scratch sc = {0}; \
    if (a) { e.count = count; sc = snapshot(count, requests_array, STATUSES); \
        if (count > 0 && (!sc.old || ((STATUSES) && !sc.statuses))) { e.flags |= RPROF_MPI_TRACKING_LIMIT; abandon_tracking(&e); } }
#define ARRAY_DONE(RC) \
    if (a) { e.end_ns = rprof_mpi_now(); e.result = (RC); emit_aux(&e); release_scratch(&sc); inside = 0; }
int MPI_Waitall(int count, MPI_Request requests_array[], MPI_Status statuses[]) {
    ARRAY_SETUP(Waitall, statuses == MPI_STATUSES_IGNORE);
    MPI_Status *s = a && statuses == MPI_STATUSES_IGNORE && sc.statuses ? sc.statuses : statuses;
    int rc = PMPI_Waitall(count, requests_array, s);
    if (a) { e.end_ns = rprof_mpi_now(); e.result = rc; emit_aux(&e);
        if (sc.old && requests_array)
            for (int i = 0; i < count; ++i)
                if (requests_array[i] == MPI_REQUEST_NULL &&
                    (rc != MPI_ERR_IN_STATUS || s == MPI_STATUSES_IGNORE || s[i].MPI_ERROR != MPI_ERR_PENDING))
                    end_request(sc.old[i], &requests_array[i], &e, (rc == MPI_SUCCESS || rc == MPI_ERR_IN_STATUS) && s != MPI_STATUSES_IGNORE ? &s[i] : NULL, 0);
        release_scratch(&sc); inside = 0; } return rc;
}
int MPI_Testall(int count, MPI_Request requests_array[], int *flag, MPI_Status statuses[]) {
    ARRAY_SETUP(Testall, statuses == MPI_STATUSES_IGNORE);
    MPI_Status *s = a && statuses == MPI_STATUSES_IGNORE && sc.statuses ? sc.statuses : statuses;
    int rc = PMPI_Testall(count, requests_array, flag, s);
    if (a) { e.end_ns = rprof_mpi_now(); e.result = rc; emit_aux(&e);
        if (sc.old && requests_array && (rc != MPI_SUCCESS || (flag && *flag)))
            for (int i = 0; i < count; ++i)
                if (requests_array[i] == MPI_REQUEST_NULL &&
                    (rc != MPI_ERR_IN_STATUS || s == MPI_STATUSES_IGNORE || s[i].MPI_ERROR != MPI_ERR_PENDING))
                    end_request(sc.old[i], &requests_array[i], &e, (rc == MPI_SUCCESS || rc == MPI_ERR_IN_STATUS) && s != MPI_STATUSES_IGNORE ? &s[i] : NULL, 0);
        release_scratch(&sc); inside = 0; } return rc;
}
int MPI_Waitany(int count, MPI_Request requests_array[], int *index, MPI_Status *status) {
    ARRAY_SETUP(Waitany, 0); MPI_Status private_status;
    MPI_Status *s = a && status == MPI_STATUS_IGNORE ? &private_status : status;
    int rc = PMPI_Waitany(count, requests_array, index, s);
    if (a) { e.end_ns = rprof_mpi_now(); e.result = rc; emit_aux(&e);
        if (sc.old && requests_array && index && rc == MPI_SUCCESS && *index >= 0 && *index < count && requests_array[*index] == MPI_REQUEST_NULL)
            end_request(sc.old[*index], &requests_array[*index], &e, s, 0);
        if (sc.old && requests_array && rc != MPI_SUCCESS && rc != MPI_ERR_IN_STATUS)
            for (int i = 0; i < count; ++i)
                if (requests_array[i] == MPI_REQUEST_NULL) end_request(sc.old[i], &requests_array[i], &e, NULL, 0);
        release_scratch(&sc); inside = 0; } return rc;
}
int MPI_Testany(int count, MPI_Request requests_array[], int *index, int *flag, MPI_Status *status) {
    ARRAY_SETUP(Testany, 0); MPI_Status private_status;
    MPI_Status *s = a && status == MPI_STATUS_IGNORE ? &private_status : status;
    int rc = PMPI_Testany(count, requests_array, index, flag, s);
    if (a) { e.end_ns = rprof_mpi_now(); e.result = rc; emit_aux(&e);
        if (rc == MPI_SUCCESS && sc.old && flag && *flag && index && *index >= 0 && *index < count && requests_array[*index] == MPI_REQUEST_NULL)
            end_request(sc.old[*index], &requests_array[*index], &e, s, 0);
        if (sc.old && requests_array && rc != MPI_SUCCESS && rc != MPI_ERR_IN_STATUS)
            for (int i = 0; i < count; ++i)
                if (requests_array[i] == MPI_REQUEST_NULL) end_request(sc.old[i], &requests_array[i], &e, NULL, 0);
        release_scratch(&sc); inside = 0; } return rc;
}
int MPI_Waitsome(int count, MPI_Request requests_array[], int *outcount, int indices[], MPI_Status statuses[]) {
    ARRAY_SETUP(Waitsome, statuses == MPI_STATUSES_IGNORE);
    MPI_Status *s = a && statuses == MPI_STATUSES_IGNORE && sc.statuses ? sc.statuses : statuses;
    int rc = PMPI_Waitsome(count, requests_array, outcount, indices, s);
    if (a) { e.end_ns = rprof_mpi_now(); e.result = rc; emit_aux(&e);
        if (sc.old && (rc == MPI_SUCCESS || (rc == MPI_ERR_IN_STATUS && s != MPI_STATUSES_IGNORE)) && outcount && *outcount > 0 && *outcount <= count && indices)
            for (int j = 0; j < *outcount; ++j) { int i = indices[j]; if (i >= 0 && i < count && requests_array[i] == MPI_REQUEST_NULL && (rc == MPI_SUCCESS || s[j].MPI_ERROR != MPI_ERR_PENDING)) end_request(sc.old[i], &requests_array[i], &e, s == MPI_STATUSES_IGNORE ? NULL : &s[j], 0); }
        if (sc.old && requests_array && rc != MPI_SUCCESS && rc != MPI_ERR_IN_STATUS)
            for (int i = 0; i < count; ++i)
                if (requests_array[i] == MPI_REQUEST_NULL) end_request(sc.old[i], &requests_array[i], &e, NULL, 0);
        release_scratch(&sc); inside = 0; } return rc;
}
int MPI_Testsome(int count, MPI_Request requests_array[], int *outcount, int indices[], MPI_Status statuses[]) {
    ARRAY_SETUP(Testsome, statuses == MPI_STATUSES_IGNORE);
    MPI_Status *s = a && statuses == MPI_STATUSES_IGNORE && sc.statuses ? sc.statuses : statuses;
    int rc = PMPI_Testsome(count, requests_array, outcount, indices, s);
    if (a) { e.end_ns = rprof_mpi_now(); e.result = rc; emit_aux(&e);
        if (sc.old && (rc == MPI_SUCCESS || (rc == MPI_ERR_IN_STATUS && s != MPI_STATUSES_IGNORE)) && outcount && *outcount > 0 && *outcount <= count && indices)
            for (int j = 0; j < *outcount; ++j) { int i = indices[j]; if (i >= 0 && i < count && requests_array[i] == MPI_REQUEST_NULL && (rc == MPI_SUCCESS || s[j].MPI_ERROR != MPI_ERR_PENDING)) end_request(sc.old[i], &requests_array[i], &e, s == MPI_STATUSES_IGNORE ? NULL : &s[j], 0); }
        if (sc.old && requests_array && rc != MPI_SUCCESS && rc != MPI_ERR_IN_STATUS)
            for (int i = 0; i < count; ++i)
                if (requests_array[i] == MPI_REQUEST_NULL) end_request(sc.old[i], &requests_array[i], &e, NULL, 0);
        release_scratch(&sc); inside = 0; } return rc;
}
int MPI_Request_free(MPI_Request *request) {
    struct rprof_mpi_event e; int a = start(&e, RPROF_MPI_CALL, "MPI_Request_free");
    MPI_Request old = a && request ? *request : MPI_REQUEST_NULL;
    int rc = PMPI_Request_free(request);
    if (a) { e.end_ns = rprof_mpi_now(); e.result = rc; emit_aux(&e); if (rc == MPI_SUCCESS && request && *request == MPI_REQUEST_NULL) end_request(old, request, &e, NULL, 1); inside = 0; } return rc;
}
int MPI_Cancel(MPI_Request *request) {
    struct rprof_mpi_event e; int a = start(&e, RPROF_MPI_CALL, "MPI_Cancel");
    int rc = PMPI_Cancel(request); if (a) finish(&e, rc); return rc;
}

#define SIMPLE_COMM(NAME, DECL, PRE, POST, ARGS) \
int MPI_##NAME DECL { struct rprof_mpi_event e; int a = start(&e, RPROF_MPI_CALL, "MPI_" #NAME); \
    if (a) { comm_fields(&e, comm); PRE; e.start_ns = rprof_mpi_now(); } \
    int rc = PMPI_##NAME ARGS; if (a) { e.end_ns = rprof_mpi_now(); \
        snprintf(e.detail, sizeof e.detail, "local logical extent, not network volume"); \
        if (rc == MPI_SUCCESS) { POST; } finish(&e, rc); } return rc; }
SIMPLE_COMM(Barrier, (MPI_Comm comm), (void)0, (void)0, (comm))
SIMPLE_COMM(Bcast, (void *buf, int count, MPI_Datatype type, int root, MPI_Comm comm),
            e.peer = root; e.count = count, bytes_fields(&e, count, type), (buf,count,type,root,comm))
SIMPLE_COMM(Reduce, (const void *sendbuf, void *recvbuf, int count, MPI_Datatype type, MPI_Op op, int root, MPI_Comm comm),
            e.peer = root; e.count = count, bytes_fields(&e, count, type), (sendbuf,recvbuf,count,type,op,root,comm))
SIMPLE_COMM(Allreduce, (const void *sendbuf, void *recvbuf, int count, MPI_Datatype type, MPI_Op op, MPI_Comm comm),
            e.count = count, bytes_fields(&e, count, type), (sendbuf,recvbuf,count,type,op,comm))
SIMPLE_COMM(Gather, (const void *sendbuf,int sendcount,MPI_Datatype sendtype,void *recvbuf,int recvcount,MPI_Datatype recvtype,int root,MPI_Comm comm),
            e.peer=root; e.count=sendbuf == MPI_IN_PLACE ? recvcount : sendcount,
            (sendbuf == MPI_IN_PLACE && e.comm_rank == root) ? bytes_fields(&e,recvcount,recvtype) : bytes_fields(&e,sendcount,sendtype),
            (sendbuf,sendcount,sendtype,recvbuf,recvcount,recvtype,root,comm))
SIMPLE_COMM(Scatter, (const void *sendbuf,int sendcount,MPI_Datatype sendtype,void *recvbuf,int recvcount,MPI_Datatype recvtype,int root,MPI_Comm comm),
            e.peer=root; e.count=recvbuf == MPI_IN_PLACE ? sendcount : recvcount,
            (recvbuf == MPI_IN_PLACE && e.comm_rank == root) ? bytes_fields(&e,sendcount,sendtype) : bytes_fields(&e,recvcount,recvtype),
            (sendbuf,sendcount,sendtype,recvbuf,recvcount,recvtype,root,comm))
SIMPLE_COMM(Allgather, (const void *sendbuf,int sendcount,MPI_Datatype sendtype,void *recvbuf,int recvcount,MPI_Datatype recvtype,MPI_Comm comm),
            e.count=sendbuf == MPI_IN_PLACE ? recvcount : sendcount,
            sendbuf == MPI_IN_PLACE ? bytes_fields(&e,recvcount,recvtype) : bytes_fields(&e,sendcount,sendtype),
            (sendbuf,sendcount,sendtype,recvbuf,recvcount,recvtype,comm))
SIMPLE_COMM(Alltoall, (const void *sendbuf,int sendcount,MPI_Datatype sendtype,void *recvbuf,int recvcount,MPI_Datatype recvtype,MPI_Comm comm),
            e.count=sendbuf == MPI_IN_PLACE ? recvcount : sendcount,
            sendbuf == MPI_IN_PLACE ? bytes_fields(&e,recvcount,recvtype) : bytes_fields(&e,sendcount,sendtype),
            (sendbuf,sendcount,sendtype,recvbuf,recvcount,recvtype,comm))

#define ICOLL(NAME, DECL, PRE, POST, ARGS) \
int MPI_##NAME DECL { struct rprof_mpi_event e; int a = start(&e, RPROF_MPI_CALL, "MPI_" #NAME); \
    if (a) { PRE; e.start_ns = rprof_mpi_now(); } int rc = PMPI_##NAME ARGS; \
    if (a) { e.end_ns = rprof_mpi_now(); e.result = rc; \
        snprintf(e.detail, sizeof e.detail, "local logical extent, not network volume"); \
        if (rc == MPI_SUCCESS) { POST; if (request && *request != MPI_REQUEST_NULL && !track(*request, request, &e)) e.flags |= RPROF_MPI_TRACKING_LIMIT; } \
        emit_aux(&e); inside = 0; } return rc; }
ICOLL(Ibarrier, (MPI_Comm comm, MPI_Request *request), comm_fields(&e, comm), (void)0, (comm,request))
ICOLL(Ibcast, (void *buf,int count,MPI_Datatype type,int root,MPI_Comm comm,MPI_Request *request),
      comm_fields(&e,comm); e.peer=root; e.count=count, bytes_fields(&e,count,type), (buf,count,type,root,comm,request))
ICOLL(Iallreduce, (const void *sendbuf,void *recvbuf,int count,MPI_Datatype type,MPI_Op op,MPI_Comm comm,MPI_Request *request),
      comm_fields(&e,comm); e.count=count, bytes_fields(&e,count,type), (sendbuf,recvbuf,count,type,op,comm,request))

static void describe_comm(struct rprof_mpi_event *e, MPI_Comm c) {
    if (c == MPI_COMM_NULL) return;
    comm_fields(e, c);
    if (c == MPI_COMM_WORLD) { snprintf(e->detail, sizeof e->detail, "world ranks 0..%d", world_size - 1); return; }
    MPI_Group group, world;
    if (PMPI_Comm_group(c, &group) != MPI_SUCCESS) return;
    if (PMPI_Comm_group(MPI_COMM_WORLD, &world) != MPI_SUCCESS) { PMPI_Group_free(&group); return; }
    int off = snprintf(e->detail, sizeof e->detail, "world ranks:");
    for (int i = 0; i < e->comm_size; ++i) {
        int translated = MPI_UNDEFINED;
        PMPI_Group_translate_ranks(group, 1, &i, world, &translated);
        int n = snprintf(e->detail + off, sizeof e->detail - (size_t)off, " %d", translated);
        if (n < 0 || n >= (int)sizeof e->detail - off) { e->flags |= RPROF_MPI_DETAIL_TRUNCATED; break; }
        off += n;
    }
    PMPI_Group_free(&group); PMPI_Group_free(&world);
}
int MPI_Comm_rank(MPI_Comm comm, int *rank) {
    struct rprof_mpi_event e; int a = start(&e, RPROF_MPI_CALL, "MPI_Comm_rank");
    if (a) { e.comm_id = comm_id(comm); e.start_ns = rprof_mpi_now(); }
    int rc = PMPI_Comm_rank(comm, rank);
    if (a) e.end_ns = rprof_mpi_now();
    if (a) { if (rc == MPI_SUCCESS && rank) e.comm_rank = *rank; finish(&e, rc); } return rc;
}
int MPI_Comm_size(MPI_Comm comm, int *size) {
    struct rprof_mpi_event e; int a = start(&e, RPROF_MPI_CALL, "MPI_Comm_size");
    if (a) { e.comm_id = comm_id(comm); e.start_ns = rprof_mpi_now(); }
    int rc = PMPI_Comm_size(comm, size);
    if (a) e.end_ns = rprof_mpi_now();
    if (a) { if (rc == MPI_SUCCESS && size) e.comm_size = *size; finish(&e, rc); } return rc;
}
int MPI_Comm_dup(MPI_Comm comm, MPI_Comm *newcomm) {
    struct rprof_mpi_event e; int a = start(&e, RPROF_MPI_CALL, "MPI_Comm_dup");
    if (a) { comm_fields(&e,comm); e.start_ns = rprof_mpi_now(); }
    int rc = PMPI_Comm_dup(comm,newcomm);
    if (a) { e.end_ns = rprof_mpi_now(); e.result = rc; emit_aux(&e);
        if (rc == MPI_SUCCESS && newcomm && *newcomm != MPI_COMM_NULL) {
            struct rprof_mpi_event c = e; c.kind = RPROF_MPI_COMM; c.comm_id = remember_comm(*newcomm);
            if (!c.comm_id) c.flags |= RPROF_MPI_TRACKING_LIMIT;
            describe_comm(&c,*newcomm); emit_aux(&c);
        } inside = 0; } return rc;
}
int MPI_Comm_split(MPI_Comm comm, int color, int key, MPI_Comm *newcomm) {
    struct rprof_mpi_event e; int a = start(&e, RPROF_MPI_CALL, "MPI_Comm_split");
    if (a) { comm_fields(&e,comm); e.aux0 = color; e.aux1 = (uint64_t)key; e.start_ns = rprof_mpi_now(); }
    int rc = PMPI_Comm_split(comm,color,key,newcomm);
    if (a) { e.end_ns = rprof_mpi_now(); e.result = rc; emit_aux(&e);
        if (rc == MPI_SUCCESS && newcomm && *newcomm != MPI_COMM_NULL) {
            struct rprof_mpi_event c = e; c.kind = RPROF_MPI_COMM; c.comm_id = remember_comm(*newcomm);
            if (!c.comm_id) c.flags |= RPROF_MPI_TRACKING_LIMIT;
            describe_comm(&c,*newcomm); emit_aux(&c);
        } inside = 0; } return rc;
}
int MPI_Comm_free(MPI_Comm *comm) {
    struct rprof_mpi_event e; int a = start(&e, RPROF_MPI_CALL, "MPI_Comm_free");
    MPI_Comm old = a && comm ? *comm : MPI_COMM_NULL;
    if (a) { if (old != MPI_COMM_NULL) comm_fields(&e,old); e.start_ns = rprof_mpi_now(); }
    int rc = PMPI_Comm_free(comm);
    if (a) { e.end_ns = rprof_mpi_now(); e.result = rc; emit_aux(&e);
        if (rc == MPI_SUCCESS && old != MPI_COMM_NULL) { struct rprof_mpi_event c = e; c.kind = RPROF_MPI_COMM; snprintf(c.detail,sizeof c.detail,"freed"); emit_aux(&c); forget_comm(old); }
        inside = 0; } return rc;
}
