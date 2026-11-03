#ifndef RPROF_OMPT_HOOKS_H
#define RPROF_OMPT_HOOKS_H
#include <stdint.h>
enum { RPROF_OMP_RUNTIME=1,RPROF_OMP_CAPABILITY=2,RPROF_OMP_THREAD_BEGIN=3,
 RPROF_OMP_THREAD_END=4,RPROF_OMP_PARALLEL_BEGIN=5,RPROF_OMP_PARALLEL_END=6,
 RPROF_OMP_IMPLICIT_BEGIN=7,RPROF_OMP_IMPLICIT_END=8,RPROF_OMP_WORK_BEGIN=9,
 RPROF_OMP_WORK_END=10,RPROF_OMP_SYNC_BEGIN=11,RPROF_OMP_SYNC_END=12,
 RPROF_OMP_WAIT_BEGIN=13,RPROF_OMP_WAIT_END=14,RPROF_OMP_TASK_CREATE=15,
 RPROF_OMP_TASK_SCHEDULE=16,RPROF_OMP_DEPENDENCE=17,RPROF_OMP_FINALIZE=18,
 RPROF_GOMP_API_BEGIN=19,RPROF_GOMP_API_END=20,RPROF_GOMP_BODY_BEGIN=21,
 RPROF_GOMP_BODY_END=22,RPROF_GOMP_TASK_CREATE=23 };
#define RPROF_GOMP_BACKEND (UINT64_C(1)<<63)
#define RPROF_OMP_UNAVAILABLE (UINT64_C(1)<<62)
struct rprof_omp_event {
 uint32_t magic; uint16_t version,kind; uint32_t pid,tid;
 uint64_t instance,timestamp_ns,sequence,thread_id,parallel_id,task_id,
 parent_task_id,next_task_id,codeptr,count,flags;
 int64_t value,aux;
 char name[64],detail[192];
};
_Static_assert(sizeof(struct rprof_omp_event)==376,"OMPT wire ABI mismatch");
/* init maps RPROF_OMP_FD/RPROF_OMP_STATS_FD; no OMPT calls and no activation yet. */
int rprof_omp_init(void);
void rprof_omp_activate(void);
uint64_t rprof_omp_next_id(void);
/* begin zeroes storage, stamps identity/time/name; emit preserves those fields.
 * C sets a monotonically increasing per-thread sequence for EVERY emitted packet. */
int rprof_omp_begin(struct rprof_omp_event *,uint16_t kind,const char *name);
void rprof_omp_emit(struct rprof_omp_event *);
/* Cold-path check of the target-built runtime fingerprint; preserves errno. */
int rprof_omp_runtime_matches(const char *path, const char *sha256);
#endif
