#define _GNU_SOURCE
#include "../../rprof-ompt/native/hooks.h"
#include <dlfcn.h>
#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#if !defined(__linux__) || !defined(__x86_64__) || !defined(__GNUC__) || __GNUC__ != 13 || defined(__clang__)
#error "rprof-gomp requires GCC 13 on Linux x86_64"
#endif
#ifndef RPROF_GOMP_BUILD_VERSION
#error "Build with -DRPROF_GOMP_BUILD_VERSION=\"13.2.0\""
#endif
#ifndef RPROF_GOMP_BUILD_SHA
#error "Build with -DRPROF_GOMP_BUILD_SHA=\"<target libgomp SHA256>\""
#endif
_Static_assert(sizeof(RPROF_GOMP_BUILD_SHA) == 65, "target SHA256 must be 64 hex digits");

enum {
#define X(name, wrapped) SLOT_##name,
#include "symbols.def"
#undef X
  SLOT_COUNT
};
static const char *const names[SLOT_COUNT] = {
#define X(name, wrapped) [SLOT_##name] = #name,
#include "symbols.def"
#undef X
};
static _Atomic(void *) targets[SLOT_COUNT];
static _Atomic signed char target_checked[SLOT_COUNT]; /* 1 verified base, -1 foreign */
extern void *const rprof_gomp_wrappers[SLOT_COUNT] __attribute__((visibility("hidden")));
static pthread_mutex_t init_lock = PTHREAD_MUTEX_INITIALIZER;
static _Atomic int state; /* 0 uninitialized; 1 active; -1 mismatch/no sink; -2 forked child */
enum detail_level { DETAIL_REGIONS, DETAIL_BODIES, DETAIL_FULL };
static enum detail_level detail = DETAIL_REGIONS; /* immutable after state becomes 1 */
static _Atomic unsigned init_in_progress;
static _Atomic(void *) verified_base;
static _Atomic bool foreign_reported, child_reported;
static _Atomic bool task_declined;
static _Thread_local int emitting, resolving;
static int (*team_size_query)(void), (*thread_num_query)(void), (*cancellation_query)(void);
static _Thread_local struct {
  uint64_t seq, thread, task, parallel;
  unsigned depth, base, overflow;
  struct { uint64_t task, parallel; int64_t kind; bool open; } work[128];
} tls;

static void child_after_fork(void) {
  if (atomic_load_explicit(&state, memory_order_relaxed) != 0 ||
      atomic_load_explicit(&init_in_progress, memory_order_relaxed) != 0 || resolving)
    atomic_store_explicit(&state, -2, memory_order_relaxed);
  tls.seq=0; tls.thread=0; tls.task=0; tls.parallel=0;
  tls.depth=0; tls.base=0; tls.overflow=0;
  emitting=0; resolving=0;
}
/* Register before any application GOMP call; the constructor never loads,
 * resolves or initializes libgomp or the profiler wire transport. */
__attribute__((constructor)) static void register_fork(void) {
  int saved=errno;
  (void)pthread_atfork(NULL,NULL,child_after_fork);
  errno=saved;
}
static void child_diagnostic(void) {
  if (atomic_exchange_explicit(&child_reported,true,memory_order_relaxed)) return;
  int saved=errno;
  resolving=1;
  if (rprof_omp_init()) {
    struct rprof_omp_event e;
    if (rprof_omp_begin(&e,RPROF_OMP_RUNTIME,"gcc libgomp")) {
      e.flags=RPROF_GOMP_BACKEND|RPROF_OMP_UNAVAILABLE;
      e.sequence=++tls.seq;
      snprintf(e.detail,sizeof e.detail,"fork after observed or initializing libgomp: child forwards without instrumentation");
      rprof_omp_emit(&e);
    }
  }
  resolving=0;
  errno=saved;
}
static void foreign_diagnostic(unsigned slot) {
  if (atomic_exchange_explicit(&foreign_reported,true,memory_order_relaxed)) return;
  int saved=errno;
  resolving=1;
  struct rprof_omp_event e;
  const char *cap=detail==DETAIL_FULL ? "gomp.api" :
                  slot==SLOT_GOMP_task ? "gomp.task_body" : "gomp.parallel_body";
  if (rprof_omp_begin(&e,RPROF_OMP_CAPABILITY,cap)) {
    e.flags=RPROF_GOMP_BACKEND;
    e.value=3;
    e.thread_id=tls.thread;
    e.sequence=++tls.seq;
    snprintf(e.detail,sizeof e.detail,"%s resolves outside verified libgomp DSO; ABI-only forwarding",names[slot]);
    rprof_omp_emit(&e);
  }
  resolving=0;
  errno=saved;
}
static int (*verified_query(const char *name,void *base))(void) {
  void *p=dlsym(RTLD_NEXT,name);
  Dl_info info;
  if (!p || !dladdr(p,&info) || info.dli_fbase!=base) return NULL;
  return (int (*)(void))p;
}
static void finish(void) {
  if (atomic_load_explicit(&state, memory_order_relaxed) != 1) return;
  int saved = errno;
  struct rprof_omp_event e;
  if (rprof_omp_begin(&e, RPROF_OMP_FINALIZE, "finalize")) {
    e.flags = RPROF_GOMP_BACKEND;
    e.thread_id = tls.thread;
    e.sequence = ++tls.seq;
    rprof_omp_emit(&e);
  }
  errno = saved;
}

/* No OpenMP runtime is linked into this DSO. Only a genuine RTLD_NEXT
 * libgomp target may turn on instrumentation. No ctor touches libgomp. */
__attribute__((visibility("hidden"))) void *rprof_gomp_target(unsigned slot) {
  int saved = errno;
  if (slot >= SLOT_COUNT) _exit(127);
  void *p = atomic_load_explicit(&targets[slot], memory_order_acquire);
  if (!p) {
    p = dlsym(RTLD_NEXT, names[slot]);
    if (!p) {
      static const char message[] = "rprof-gomp: missing GCC libgomp entry point\n";
      (void)write(STDERR_FILENO, message, sizeof(message) - 1);
      _exit(127); /* Calling a nonexistent real function has no valid ABI. */
    }
    atomic_store_explicit(&targets[slot], p, memory_order_release);
  }
  int s = atomic_load_explicit(&state, memory_order_acquire);
  if (resolving || emitting) { errno=saved; return p; }
  if (s == -2) { child_diagnostic(); errno=saved; return p; }
  if (s == 0) {
    atomic_fetch_add_explicit(&init_in_progress,1,memory_order_acq_rel);
    resolving=1;
    pthread_mutex_lock(&init_lock);
    if (atomic_load_explicit(&state, memory_order_relaxed) == 0) {
      Dl_info info;
      bool identity = dladdr(p, &info) && info.dli_fname &&
                      strstr(info.dli_fname, "libgomp.so") != NULL;
      bool matched = identity && rprof_omp_runtime_matches(info.dli_fname, RPROF_GOMP_BUILD_SHA);
      int sink = rprof_omp_init();
      const char *selected=getenv("RPROF_GOMP_DETAIL");
      bool valid_detail=true;
      if (selected) {
        if (strcmp(selected,"regions")==0) detail=DETAIL_REGIONS;
        else if (strcmp(selected,"bodies")==0) detail=DETAIL_BODIES;
        else if (strcmp(selected,"full")==0) detail=DETAIL_FULL;
        else valid_detail=false;
      }
      if (!matched || !valid_detail) {
        if (sink) {
          struct rprof_omp_event e;
          if (rprof_omp_begin(&e, RPROF_OMP_RUNTIME, "gcc libgomp")) {
            e.flags = RPROF_GOMP_BACKEND | RPROF_OMP_UNAVAILABLE;
            if (!valid_detail)
              snprintf(e.detail,sizeof e.detail,"invalid RPROF_GOMP_DETAIL (expected regions, bodies or full); libgomp forwarded unchanged");
            else
              snprintf(e.detail, sizeof e.detail, "GCC %s target libgomp fingerprint mismatch or non-libgomp target", RPROF_GOMP_BUILD_VERSION);
            e.sequence = ++tls.seq;
            rprof_omp_emit(&e);
          }
        }
        atomic_store_explicit(&state, -1, memory_order_release);
      } else if (!sink) {
        atomic_store_explicit(&state, -1, memory_order_release);
      } else {
        atomic_store_explicit(&verified_base,info.dli_fbase,memory_order_release);
        atomic_store_explicit(&target_checked[slot],1,memory_order_release);
        team_size_query=verified_query("omp_get_num_threads",info.dli_fbase);
        thread_num_query=verified_query("omp_get_thread_num",info.dli_fbase);
        cancellation_query=verified_query("omp_get_cancellation",info.dli_fbase);
        rprof_omp_activate();
        struct rprof_omp_event e;
        if (rprof_omp_begin(&e, RPROF_OMP_RUNTIME, "gcc libgomp")) {
          e.flags = RPROF_GOMP_BACKEND;
          e.value = 201511; /* compiler _OPENMP date, not runtime's advertised version */
          snprintf(e.detail, sizeof e.detail, "GCC %s / libgomp SHA256 %.64s; GCC 13 x86_64 ABI; detail=%s",
                   RPROF_GOMP_BUILD_VERSION,RPROF_GOMP_BUILD_SHA,
                   detail==DETAIL_FULL ? "full" : detail==DETAIL_BODIES ? "bodies" : "regions");
          e.sequence = ++tls.seq;
          rprof_omp_emit(&e);
        }
        static const struct { const char *name; int value; const char *detail; } caps[] = {
          {"gomp.api", 3, "Reviewed GCC 13 GOMP/lock APIs only; other calls forwarded unchanged"},
          {"gomp.parallel_body", 5, "Joined GOMP_parallel and combined parallel constructs only"},
          {"gomp.loop_chunks", 3, "Signed loop start/next/end and sections; not inline static/ULL/ordered"},
          {"gomp.task_body", 3, "Tied ordinary tasks with bounded payload/alignment; no cancellation"},
          {"gomp.task_to_body_return", 3, "Creation to body return, not runtime task completion"},
          {"gomp.inline_static_work", 2, "Compiler-inlined static chunks cannot be observed"},
          {"gomp.task_dependence_edges", 2, "Depend array forwarded without interpreting edges"},
          {"gomp.untied_tasks", 2, "Forwarded without body replacement"},
          {"gomp.detached_tasks", 2, "Forwarded without body replacement"},
          {"gomp.taskloop_body", 2, "Runtime writes first two data words; API passthrough"},
          {"gomp.legacy_parallel_body", 2, "Start/end and reductions have opaque data; passthrough"},
          {"gomp.thread_lifecycle", 3, "ThreadBegin on first observed call; no ThreadEnd for runtime pool threads"},
        };
        for (size_t i = 0; i < sizeof caps / sizeof caps[0]; ++i)
          if (rprof_omp_begin(&e, RPROF_OMP_CAPABILITY, caps[i].name)) {
            e.flags = RPROF_GOMP_BACKEND;
            e.value = caps[i].value;
            snprintf(e.detail, sizeof e.detail, "%s", caps[i].detail);
            if (detail!=DETAIL_FULL &&
                (!strcmp(caps[i].name,"gomp.api") || !strcmp(caps[i].name,"gomp.loop_chunks"))) {
              e.value=1;
              snprintf(e.detail,sizeof e.detail,"disabled by detail selection (%s)",detail==DETAIL_BODIES ? "bodies" : "regions");
            }
            if (detail==DETAIL_REGIONS &&
                (!strcmp(caps[i].name,"gomp.task_body") || !strcmp(caps[i].name,"gomp.task_to_body_return"))) {
              e.value=1;
              snprintf(e.detail,sizeof e.detail,"disabled by detail selection (regions)");
            }
            e.sequence = ++tls.seq;
            rprof_omp_emit(&e);
          }
        atexit(finish);
        atomic_store_explicit(&state, 1, memory_order_release);
      }
    }
    pthread_mutex_unlock(&init_lock);
    resolving=0;
    atomic_fetch_sub_explicit(&init_in_progress,1,memory_order_acq_rel);
    s = atomic_load_explicit(&state, memory_order_acquire);
  }
  if (s == 1 && detail != DETAIL_FULL &&
      slot > SLOT_GOMP_parallel_sections &&
      !(detail == DETAIL_BODIES && slot == SLOT_GOMP_task)) {
    errno=saved;
    return p; /* original ABI and no adapter event work in coarse modes */
  }
  if (s == 1) {
    signed char checked=atomic_load_explicit(&target_checked[slot],memory_order_acquire);
    if (checked == 0) {
      Dl_info info;
      checked=dladdr(p,&info) && info.dli_fbase==atomic_load_explicit(&verified_base,memory_order_acquire) ? 1 : -1;
      atomic_store_explicit(&target_checked[slot],checked,memory_order_release);
    }
    if (checked < 0) { foreign_diagnostic(slot); errno=saved; return p; }
  }
  errno = saved;
  return s == 1 && rprof_gomp_wrappers[slot] ? rprof_gomp_wrappers[slot] : p;
}

static void record(uint16_t kind, const char *name, uint64_t parallel, uint64_t task,
                   uint64_t parent, uint64_t code, uint64_t count, int64_t value, int64_t aux) {
  int saved = errno;
  if (detail!=DETAIL_FULL &&
      (kind==RPROF_GOMP_API_BEGIN || kind==RPROF_GOMP_API_END ||
       kind==RPROF_OMP_WORK_BEGIN || kind==RPROF_OMP_WORK_END)) { errno=saved; return; }
  if (emitting || atomic_load_explicit(&state, memory_order_relaxed) != 1) { errno = saved; return; }
  emitting = 1;
  struct rprof_omp_event e;
  if (!tls.thread) {
    tls.thread = rprof_omp_next_id();
    ++tls.seq;
    if (rprof_omp_begin(&e, RPROF_OMP_THREAD_BEGIN, "thread")) {
      e.flags = RPROF_GOMP_BACKEND;
      e.sequence = tls.seq;
      e.thread_id = tls.thread;
      rprof_omp_emit(&e);
    }
  }
  ++tls.seq; /* even unsuccessful record attempts advance the sequence */
  if (rprof_omp_begin(&e, kind, name)) {
    e.flags = RPROF_GOMP_BACKEND;
    e.sequence = tls.seq;
    e.thread_id = tls.thread;
    e.parallel_id = parallel;
    e.task_id = task;
    e.parent_task_id = parent;
    e.codeptr = code;
    e.count = count;
    e.value = value;
    e.aux = aux;
    rprof_omp_emit(&e);
  }
  emitting = 0;
  errno = saved;
}
#define CODE ((uint64_t)(uintptr_t)__builtin_return_address(0))
static void api(int slot, bool begin, uint64_t code, int64_t result) {
  if (detail!=DETAIL_FULL) return;
  record(begin ? RPROF_GOMP_API_BEGIN : RPROF_GOMP_API_END, names[slot],
         tls.parallel, tls.task, 0, code, begin ? 0 : (uint64_t)result, slot + 1, 0);
}
#define REAL(sym, type) ((type)atomic_load_explicit(&targets[SLOT_##sym], memory_order_acquire))
#define API_VOID(sym, decl, args) \
  static void wrap_##sym decl { \
    int saved = errno; uint64_t code = CODE; api(SLOT_##sym, true, code, 0); errno = saved; \
    REAL(sym, void (*) decl) args; saved = errno; \
    api(SLOT_##sym, false, code, 0); errno = saved; \
  }
#define API_RET(type, sym, decl, args) \
  static type wrap_##sym decl { \
    int saved = errno; uint64_t code = CODE; api(SLOT_##sym, true, code, 0); errno = saved; \
    type result = REAL(sym, type (*) decl) args; saved = errno; \
    api(SLOT_##sym, false, code, (int64_t)result); errno = saved; return result; \
  }
API_VOID(GOMP_barrier, (void), ())
API_RET(bool, GOMP_barrier_cancel, (void), ())
API_VOID(GOMP_taskwait, (void), ())
API_VOID(GOMP_taskwait_depend, (void **d), (d))
API_VOID(GOMP_taskwait_depend_nowait, (void **d), (d))
API_VOID(GOMP_taskyield, (void), ())
API_VOID(GOMP_taskgroup_start, (void), ())
API_VOID(GOMP_taskgroup_end, (void), ())
API_VOID(GOMP_critical_start, (void), ())
API_VOID(GOMP_critical_end, (void), ())
API_VOID(GOMP_critical_name_start, (void **p), (p))
API_VOID(GOMP_critical_name_end, (void **p), (p))
API_VOID(GOMP_atomic_start, (void), ())
API_VOID(GOMP_atomic_end, (void), ())
API_VOID(GOMP_ordered_start, (void), ())
API_VOID(GOMP_ordered_end, (void), ())
API_RET(bool, GOMP_single_start, (void), ())
API_RET(void *, GOMP_single_copy_start, (void), ())
API_VOID(GOMP_single_copy_end, (void *p), (p))
API_VOID(omp_set_lock, (void *p), (p))
API_VOID(omp_unset_lock, (void *p), (p))
API_RET(int, omp_test_lock, (void *p), (p))
API_VOID(omp_set_nest_lock, (void *p), (p))
API_VOID(omp_unset_nest_lock, (void *p), (p))
API_RET(int, omp_test_nest_lock, (void *p), (p))

/* A work frame is pushed for each start, including zero-chunk starts.
 * No closing event is synthesized by a barrier or a body returning. */
static void chunk_end(void) {
  if (tls.overflow) return;
  if (tls.depth <= tls.base) return;
  unsigned i = tls.depth - 1;
  if (!tls.work[i].open) return;
  tls.work[i].open = false;
  record(RPROF_OMP_WORK_END, tls.work[i].kind == 101 ? "section" : "loop chunk",
         tls.work[i].parallel, tls.work[i].task, 0, 0, 0, tls.work[i].kind, 0);
}
static void chunk_begin(bool exists, int64_t kind) {
  if (tls.overflow) return;
  if (!exists || tls.depth <= tls.base) return;
  unsigned i = tls.depth - 1;
  tls.work[i].kind = kind;
  tls.work[i].open = true;
  record(RPROF_OMP_WORK_BEGIN, kind == 101 ? "section" : "loop chunk",
         tls.parallel, tls.task, 0, 0, 0, kind, 0);
}
static void push_work(int64_t kind) {
  if (tls.depth == 128 || tls.overflow) { ++tls.overflow; return; }
  tls.work[tls.depth].task = tls.task;
  tls.work[tls.depth].parallel = tls.parallel;
  tls.work[tls.depth].kind = kind;
  tls.work[tls.depth++].open = false;
}
#define START5(sym) \
 static bool wrap_##sym(long a,long b,long c,long d,long *lo,long *hi) { \
  int saved=errno; uint64_t code=CODE; api(SLOT_##sym,true,code,0); errno=saved; \
  bool result=REAL(sym,bool (*)(long,long,long,long,long *,long *))(a,b,c,d,lo,hi); saved=errno; \
  push_work(100); chunk_begin(result,100); api(SLOT_##sym,false,code,result); errno=saved; return result; \
 }
#define START4(sym) \
 static bool wrap_##sym(long a,long b,long c,long *lo,long *hi) { \
  int saved=errno; uint64_t code=CODE; api(SLOT_##sym,true,code,0); errno=saved; \
  bool result=REAL(sym,bool (*)(long,long,long,long *,long *))(a,b,c,lo,hi); saved=errno; \
  push_work(100); chunk_begin(result,100); api(SLOT_##sym,false,code,result); errno=saved; return result; \
 }
#define NEXT(sym) \
 static bool wrap_##sym(long *lo,long *hi) { \
  int saved=errno; uint64_t code=CODE; chunk_end(); api(SLOT_##sym,true,code,0); errno=saved; \
  bool result=REAL(sym,bool (*)(long *,long *))(lo,hi); saved=errno; \
  chunk_begin(result,100); api(SLOT_##sym,false,code,result); errno=saved; return result; \
 }
START5(GOMP_loop_static_start)
START5(GOMP_loop_dynamic_start)
START5(GOMP_loop_guided_start)
START5(GOMP_loop_nonmonotonic_dynamic_start)
START5(GOMP_loop_nonmonotonic_guided_start)
START4(GOMP_loop_runtime_start)
START4(GOMP_loop_nonmonotonic_runtime_start)
START4(GOMP_loop_maybe_nonmonotonic_runtime_start)
NEXT(GOMP_loop_static_next)
NEXT(GOMP_loop_dynamic_next)
NEXT(GOMP_loop_guided_next)
NEXT(GOMP_loop_runtime_next)
NEXT(GOMP_loop_nonmonotonic_dynamic_next)
NEXT(GOMP_loop_nonmonotonic_guided_next)
NEXT(GOMP_loop_nonmonotonic_runtime_next)
NEXT(GOMP_loop_maybe_nonmonotonic_runtime_next)
#define END_WORK(sym) \
 static void wrap_##sym(void) { \
  int saved=errno; uint64_t code=CODE; chunk_end(); if(tls.overflow) --tls.overflow; else if(tls.depth>tls.base) --tls.depth; \
  api(SLOT_##sym,true,code,0); errno=saved; REAL(sym,void (*)(void))(); saved=errno; \
  api(SLOT_##sym,false,code,0); errno=saved; \
 }
#define END_WORK_BOOL(sym) \
 static bool wrap_##sym(void) { \
  int saved=errno; uint64_t code=CODE; chunk_end(); if(tls.overflow) --tls.overflow; else if(tls.depth>tls.base) --tls.depth; \
  api(SLOT_##sym,true,code,0); errno=saved; bool r=REAL(sym,bool (*)(void))(); saved=errno; \
  api(SLOT_##sym,false,code,r); errno=saved; return r; \
 }
END_WORK(GOMP_loop_end)
END_WORK(GOMP_loop_end_nowait)
END_WORK_BOOL(GOMP_loop_end_cancel)
static unsigned wrap_GOMP_sections_start(unsigned n) {
  int saved=errno; uint64_t code=CODE; api(SLOT_GOMP_sections_start,true,code,0); errno=saved;
  unsigned r=REAL(GOMP_sections_start,unsigned (*)(unsigned))(n); saved=errno;
  push_work(101); chunk_begin(r != 0,101); api(SLOT_GOMP_sections_start,false,code,r); errno=saved; return r;
}
static unsigned wrap_GOMP_sections_next(void) {
  int saved=errno; uint64_t code=CODE; chunk_end(); api(SLOT_GOMP_sections_next,true,code,0); errno=saved;
  unsigned r=REAL(GOMP_sections_next,unsigned (*)(void))(); saved=errno;
  chunk_begin(r != 0,101); api(SLOT_GOMP_sections_next,false,code,r); errno=saved; return r;
}
END_WORK(GOMP_sections_end)
END_WORK(GOMP_sections_end_nowait)
END_WORK_BOOL(GOMP_sections_end_cancel)

typedef void (*body_fn)(void *);
struct parallel_ctx { body_fn fn; void *data; uint64_t id, parent; int64_t work_kind; };
static void parallel_body(void *p) {
  struct parallel_ctx *c = p;
  int saved = errno;
  uint64_t old_task=tls.task, old_parallel=tls.parallel;
  unsigned old_depth=tls.depth, old_base=tls.base, old_overflow=tls.overflow;
  tls.base=tls.depth; tls.overflow=0; tls.parallel=c->id; tls.task=rprof_omp_next_id();
  if (detail==DETAIL_FULL && c->work_kind) push_work(c->work_kind);
  int count=team_size_query ? team_size_query() : 0;
  int index=thread_num_query ? thread_num_query() : -1;
  record(RPROF_GOMP_BODY_BEGIN,"parallel outlined body",c->id,tls.task,c->parent,
         (uint64_t)(uintptr_t)c->fn,(uint64_t)count,2,index);
  errno=saved; c->fn(c->data); saved=errno;
  record(RPROF_GOMP_BODY_END,"parallel outlined body",c->id,tls.task,c->parent,
         (uint64_t)(uintptr_t)c->fn,(uint64_t)count,2,index);
  tls.task=old_task; tls.parallel=old_parallel; tls.depth=old_depth;
  tls.base=old_base; tls.overflow=old_overflow; errno=saved;
}
#define PAR_BEGIN(sym,fn,threads,flags,work_kind_arg) \
 int saved=errno; uint64_t code=CODE; \
 struct parallel_ctx c={fn,data,rprof_omp_next_id(),tls.task,work_kind_arg}; \
 record(RPROF_OMP_PARALLEL_BEGIN,#sym,c.id,tls.task,c.parent,(uint64_t)(uintptr_t)fn,threads,0,flags); \
 api(SLOT_##sym,true,code,0); errno=saved
#define PAR_END(sym,fn,threads,flags) \
 saved=errno; api(SLOT_##sym,false,code,0); \
 record(RPROF_OMP_PARALLEL_END,#sym,c.id,tls.task,c.parent,(uint64_t)(uintptr_t)fn,threads,0,flags); errno=saved
static void wrap_GOMP_parallel(body_fn fn,void *data,unsigned threads,unsigned flags) {
  PAR_BEGIN(GOMP_parallel,fn,threads,flags,0);
  REAL(GOMP_parallel,void (*)(body_fn,void *,unsigned,unsigned))(parallel_body,&c,threads,flags);
  PAR_END(GOMP_parallel,fn,threads,flags);
}
#define PAR_LOOP4(sym) \
 static void wrap_##sym(body_fn fn,void *data,unsigned threads,long start,long end,long incr,long chunk,unsigned flags) { \
  PAR_BEGIN(sym,fn,threads,flags,100); \
  REAL(sym,void (*)(body_fn,void *,unsigned,long,long,long,long,unsigned))(parallel_body,&c,threads,start,end,incr,chunk,flags); \
  PAR_END(sym,fn,threads,flags); \
 }
#define PAR_LOOP3(sym) \
 static void wrap_##sym(body_fn fn,void *data,unsigned threads,long start,long end,long incr,unsigned flags) { \
  PAR_BEGIN(sym,fn,threads,flags,100); \
  REAL(sym,void (*)(body_fn,void *,unsigned,long,long,long,unsigned))(parallel_body,&c,threads,start,end,incr,flags); \
  PAR_END(sym,fn,threads,flags); \
 }
PAR_LOOP4(GOMP_parallel_loop_static)
PAR_LOOP4(GOMP_parallel_loop_dynamic)
PAR_LOOP4(GOMP_parallel_loop_guided)
PAR_LOOP4(GOMP_parallel_loop_nonmonotonic_dynamic)
PAR_LOOP4(GOMP_parallel_loop_nonmonotonic_guided)
PAR_LOOP3(GOMP_parallel_loop_runtime)
PAR_LOOP3(GOMP_parallel_loop_nonmonotonic_runtime)
PAR_LOOP3(GOMP_parallel_loop_maybe_nonmonotonic_runtime)
static void wrap_GOMP_parallel_sections(body_fn fn,void *data,unsigned threads,unsigned sections,unsigned flags) {
  PAR_BEGIN(GOMP_parallel_sections,fn,threads,flags,101);
  REAL(GOMP_parallel_sections,void (*)(body_fn,void *,unsigned,unsigned,unsigned))(parallel_body,&c,threads,sections,flags);
  PAR_END(GOMP_parallel_sections,fn,threads,flags);
}

/* GCC 13 task.c calls cpyfn(arg,data) synchronously for both deferred and
 * undeferred tasks; without cpyfn the undeferred path calls fn(data) instead.
 * Supply our own copy callback, preserving the original firstprivate copy
 * operation and its alignment, with metadata in the same runtime allocation. */
struct task_header { body_fn fn; uint64_t id,parent,parallel; size_t offset; };
struct task_source { struct task_header h; void *data; void (*copy)(void *,void *); size_t size; };
static void task_copy(void *dst,void *src) {
  struct task_source *s=src;
  memcpy(dst,&s->h,sizeof s->h);
  void *payload=(char *)dst+s->h.offset;
  if (s->copy) s->copy(payload,s->data);
  else if (s->size) memcpy(payload,s->data,s->size);
}
static void task_body(void *p) {
  struct task_header *h=p;
  int saved=errno;
  uint64_t old_task=tls.task, old_parallel=tls.parallel;
  unsigned old_depth=tls.depth, old_base=tls.base, old_overflow=tls.overflow;
  tls.base=tls.depth; tls.overflow=0; tls.task=h->id; tls.parallel=h->parallel;
  record(RPROF_GOMP_BODY_BEGIN,"task outlined body",h->parallel,h->id,h->parent,
         (uint64_t)(uintptr_t)h->fn,0,4,0);
  errno=saved; h->fn((char *)p+h->offset); saved=errno;
  record(RPROF_GOMP_BODY_END,"task outlined body",h->parallel,h->id,h->parent,
         (uint64_t)(uintptr_t)h->fn,0,4,0);
  tls.task=old_task; tls.parallel=old_parallel; tls.depth=old_depth;
  tls.base=old_base; tls.overflow=old_overflow; errno=saved;
}
static void report_task_decline(void) {
  int saved=errno;
  if (!atomic_exchange_explicit(&task_declined,true,memory_order_relaxed) && !emitting) {
    emitting=1;
    struct rprof_omp_event e;
    ++tls.seq;
    if (rprof_omp_begin(&e,RPROF_OMP_CAPABILITY,"gomp.task_body")) {
      e.flags=RPROF_GOMP_BACKEND;
      e.value=3;
      e.sequence=tls.seq;
      e.thread_id=tls.thread;
      snprintf(e.detail,sizeof e.detail,"Task with unsupported flags, alignment, payload or cancellation forwarded unchanged");
      rprof_omp_emit(&e);
    }
    emitting=0;
  }
  errno=saved;
}
static void wrap_GOMP_task(body_fn fn,void *data,void (*copy)(void *,void *),
                           long size,long align,bool if_clause,unsigned flags,
                           void **depend,int priority,void *detach) {
  int saved=errno; uint64_t code=CODE;
  api(SLOT_GOMP_task,true,code,0);
  /* FINAL/MERGEABLE/DEPEND/PRIORITY only. Runtime may write data[0] for
   * detached/reduction tasks, which therefore cannot be transformed. */
  bool valid=fn && size>=0 && size<=1048576 && align>0 && align<=4096 &&
    !(align & (align-1)) && !(flags & ~(2u|4u|8u|16u)) &&
    cancellation_query && !cancellation_query();
  if (valid) {
    size_t a=(size_t)align;
    if (a<_Alignof(struct task_header)) a=_Alignof(struct task_header);
    size_t offset=(sizeof(struct task_header)+a-1)&~(a-1);
    struct task_source source={{fn,rprof_omp_next_id(),tls.task,tls.parallel,offset},data,copy,(size_t)size};
    record(RPROF_GOMP_TASK_CREATE,"GOMP_task",tls.parallel,source.h.id,tls.task,
           (uint64_t)(uintptr_t)fn,(uint64_t)size,0,flags);
    errno=saved;
    REAL(GOMP_task,void (*)(body_fn,void *,void (*)(void *,void *),long,long,bool,unsigned,void **,int,void *))
      (task_body,&source,task_copy,(long)(offset+(size_t)size),(long)a,if_clause,flags,depend,priority,detach);
  } else {
    report_task_decline();
    errno=saved;
    REAL(GOMP_task,void (*)(body_fn,void *,void (*)(void *,void *),long,long,bool,unsigned,void **,int,void *))
      (fn,data,copy,size,align,if_clause,flags,depend,priority,detach);
  }
  saved=errno; api(SLOT_GOMP_task,false,code,0); errno=saved;
}

/* GCC owns these data layouts or the outlined function outlives the call.
 * Observe only the API duration and forward every original argument. */
API_VOID(GOMP_parallel_start, (body_fn fn,void *data,unsigned threads), (fn,data,threads))
API_VOID(GOMP_parallel_end, (void), ())
API_RET(unsigned, GOMP_parallel_reductions,
        (body_fn fn,void *data,unsigned threads,unsigned flags), (fn,data,threads,flags))
#define LEGACY_LOOP4(sym) \
 API_VOID(sym,(body_fn fn,void *data,unsigned threads,long start,long end,long incr,long chunk), \
         (fn,data,threads,start,end,incr,chunk))
#define LEGACY_LOOP3(sym) \
 API_VOID(sym,(body_fn fn,void *data,unsigned threads,long start,long end,long incr), \
         (fn,data,threads,start,end,incr))
LEGACY_LOOP4(GOMP_parallel_loop_static_start)
LEGACY_LOOP4(GOMP_parallel_loop_dynamic_start)
LEGACY_LOOP4(GOMP_parallel_loop_guided_start)
LEGACY_LOOP3(GOMP_parallel_loop_runtime_start)
API_VOID(GOMP_parallel_sections_start,
         (body_fn fn,void *data,unsigned threads,unsigned sections), (fn,data,threads,sections))
API_VOID(GOMP_taskloop,
         (body_fn fn,void *data,void (*copy)(void *,void *),long size,long align,
          unsigned flags,unsigned long num_tasks,int priority,long start,long end,long step),
         (fn,data,copy,size,align,flags,num_tasks,priority,start,end,step))
API_VOID(GOMP_taskloop_ull,
         (body_fn fn,void *data,void (*copy)(void *,void *),long size,long align,
          unsigned flags,unsigned long num_tasks,int priority,
          unsigned long long start,unsigned long long end,unsigned long long step),
         (fn,data,copy,size,align,flags,num_tasks,priority,start,end,step))
API_RET(bool,GOMP_cancel,(int which,bool enable),(which,enable))
API_RET(bool,GOMP_cancellation_point,(int which),(which))

#define WRAP_0(name) NULL
#define WRAP_1(name) (void *)wrap_##name
#define WRAP_CHOOSE(n,name) WRAP_##n(name)
void *const rprof_gomp_wrappers[SLOT_COUNT] = {
#define X(name, wrapped) [SLOT_##name] = WRAP_CHOOSE(wrapped,name),
#include "symbols.def"
#undef X
};
