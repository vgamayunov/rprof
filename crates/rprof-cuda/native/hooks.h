#ifndef RPROF_CUDA_HOOKS_H
#define RPROF_CUDA_HOOKS_H
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
enum { RPROF_CUDA_RUNTIME=1,RPROF_CUDA_CAPABILITY=2,RPROF_CUDA_DEVICE=3,
 RPROF_CUDA_CONTEXT=4,RPROF_CUDA_STREAM=5,RPROF_CUDA_API_BEGIN=6,RPROF_CUDA_API_END=7,
 RPROF_CUDA_KERNEL=8,RPROF_CUDA_MEMCPY=9,RPROF_CUDA_MEMSET=10,RPROF_CUDA_SYNC=11,
 RPROF_CUDA_FINALIZE=12,RPROF_CUDA_ERROR=13,RPROF_CUDA_DROPPED=14,RPROF_CUDA_EXTERNAL=15 };
enum { RPROF_CUDA_TIMING_UNAVAILABLE=1,RPROF_CUDA_NAME_TRUNCATED=2,
 RPROF_CUDA_RESULT_KNOWN=4,RPROF_CUDA_DEVICE_VISIBLE=8,RPROF_CUDA_OPTIONAL=16,
 RPROF_CUDA_SOURCE_ERROR=32,RPROF_CUDA_FORCED_FLUSH=64,RPROF_CUDA_ASYNC=128,
 RPROF_CUDA_UNKNOWN_THREAD=256,RPROF_CUDA_MONOTONIC_CLOCK=1024,RPROF_CUDA_SOURCE_INCOMPLETE=2048 };
enum { RPROF_CUDA_DOMAIN_RUNTIME=1,RPROF_CUDA_DOMAIN_DRIVER=2 };
struct rprof_cuda_event {
 uint32_t magic; uint16_t version,kind; uint32_t pid,tid;
 uint64_t instance,activity_id,start_ns,end_ns,correlation,runtime_correlation,parent_correlation;
 uint64_t bytes,graph_id,graph_node_id;
 int64_t value; uint64_t aux0,aux1,aux2;
 uint32_t device,context,stream,src_device,dst_device,flags;
 int32_t result; uint32_t api_domain,operation;
 uint32_t grid[3],block[3],cluster[3],registers,reserved;
 uint64_t static_shared,dynamic_shared;
 char name[192],detail[192];
};
#ifdef __cplusplus
static_assert(sizeof(rprof_cuda_event)==608,"CUDA wire ABI mismatch");
#endif
/* init maps RPROF_CUDA_FD/RPROF_CUDA_STATS_FD, returns0 if absent. */
int rprof_cuda_init(void);
void rprof_cuda_activate(void);
uint64_t rprof_cuda_now(void);
int rprof_cuda_begin(struct rprof_cuda_event *,uint16_t kind,const char *name);
void rprof_cuda_emit(struct rprof_cuda_event *);
void rprof_cuda_source_drop(uint64_t records);
void rprof_cuda_source_error(void);
void rprof_cuda_finalized(void);
#ifdef __cplusplus
}
#endif
#endif
