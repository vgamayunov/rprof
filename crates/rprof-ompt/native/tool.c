#include "hooks.h"
#include "omp-tools.h"

#include <errno.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>

/* A tool library has one socket/ID namespace. A second runtime must not replace
 * its lookup functions or register callbacks against the first runtime's state. */
static atomic_int state; /* 0 = free, 1 = initializing, 2 = active, 3 = finalized */
static ompt_get_thread_data_t get_thread_data;
static _Thread_local uint64_t thread_id, sequence;
static char runtime_name[192];
static atomic_int runtime_name_ready;

static void copy_bounded(char *dst, size_t size, const char *src) {
    size_t i = 0;
    if (src) for (; i + 1 < size && src[i]; ++i) dst[i] = src[i];
    dst[i] = '\0';
}

static uint64_t id(const ompt_data_t *data) { return data ? data->value : 0; }
static uint64_t assign_id(ompt_data_t *data) {
    if (data && !data->value) data->value = rprof_omp_next_id();
    return id(data);
}

static uint64_t current_thread(void) {
    if (!thread_id && get_thread_data) {
        ompt_data_t *data = get_thread_data();
        thread_id = assign_id(data);
    }
    return thread_id;
}

static int begin(struct rprof_omp_event *e, uint16_t kind, const char *name) {
    if (atomic_load_explicit(&state, memory_order_acquire) != 2 ||
        !rprof_omp_begin(e, kind, name)) return 0;
    e->sequence = ++sequence;
    e->thread_id = current_thread();
    return 1;
}

static void metadata(uint16_t kind, const char *name, int64_t value,
                     int64_t aux, const char *detail) {
    struct rprof_omp_event e;
    if (!begin(&e, kind, name)) return;
    e.value = value;
    e.aux = aux;
    if (detail) copy_bounded(e.detail, sizeof e.detail, detail);
    rprof_omp_emit(&e);
}

static void thread_begin(ompt_thread_t type, ompt_data_t *data) {
    int saved = errno;
    thread_id = assign_id(data);
    struct rprof_omp_event e;
    if (begin(&e, RPROF_OMP_THREAD_BEGIN, "thread.begin")) {
        e.thread_id = thread_id;
        e.value = type;
        rprof_omp_emit(&e);
    }
    errno = saved;
}

static void thread_end(ompt_data_t *data) {
    int saved = errno;
    struct rprof_omp_event e;
    if (begin(&e, RPROF_OMP_THREAD_END, "thread.end")) {
        if (id(data)) e.thread_id = id(data);
        rprof_omp_emit(&e);
    }
    thread_id = 0;
    errno = saved;
}

static void parallel_begin(ompt_data_t *encountering, const ompt_frame_t *frame,
                           ompt_data_t *parallel, unsigned int requested,
                           int flags, const void *ra) {
    int saved = errno;
    (void)frame;
    uint64_t parallel_id = assign_id(parallel);
    struct rprof_omp_event e;
    if (begin(&e, RPROF_OMP_PARALLEL_BEGIN, "parallel.begin")) {
        e.parallel_id = parallel_id;
        e.task_id = id(encountering);
        e.count = requested;
        e.flags = (uint32_t)flags;
        e.codeptr = (uintptr_t)ra;
        rprof_omp_emit(&e);
    }
    errno = saved;
}

static void parallel_end(ompt_data_t *parallel, ompt_data_t *encountering,
                         int flags, const void *ra) {
    int saved = errno;
    struct rprof_omp_event e;
    if (begin(&e, RPROF_OMP_PARALLEL_END, "parallel.end")) {
        /* A NULL parallel_data stays unknown; the encountering task and RA
         * remain available to the decoder for matching. */
        e.parallel_id = id(parallel);
        e.task_id = id(encountering);
        e.flags = (uint32_t)flags;
        e.codeptr = (uintptr_t)ra;
        rprof_omp_emit(&e);
    }
    errno = saved;
}

static void implicit_task(ompt_scope_endpoint_t endpoint, ompt_data_t *parallel,
                          ompt_data_t *task, unsigned int actual,
                          unsigned int index, int flags) {
    int saved = errno;
    uint64_t task_id = endpoint == ompt_scope_end ? id(task) : assign_id(task);
    if (endpoint == ompt_scope_begin || endpoint == ompt_scope_end ||
        endpoint == ompt_scope_beginend) {
        for (int n = 0; n < (endpoint == ompt_scope_beginend ? 2 : 1); ++n) {
            int is_begin = endpoint == ompt_scope_begin || (endpoint == ompt_scope_beginend && n == 0);
            struct rprof_omp_event e;
            if (begin(&e, is_begin ? RPROF_OMP_IMPLICIT_BEGIN : RPROF_OMP_IMPLICIT_END,
                      is_begin ? "implicit.begin" : "implicit.end")) {
                e.parallel_id = id(parallel);
                e.task_id = task_id;
                e.count = actual;
                e.aux = index;
                e.flags = (uint32_t)flags;
                rprof_omp_emit(&e);
            }
        }
    }
    errno = saved;
}

static const char *work_name(ompt_work_t kind) {
    switch (kind) {
    case ompt_work_loop: return "work.loop";
    case ompt_work_sections: return "work.sections";
    case ompt_work_single_executor: return "work.single_executor";
    case ompt_work_single_other: return "work.single_other";
    case ompt_work_workshare: return "work.workshare";
    case ompt_work_distribute: return "work.distribute";
    case ompt_work_taskloop: return "work.taskloop";
    case ompt_work_scope: return "work.scope";
    case ompt_work_loop_static: return "work.loop_static";
    case ompt_work_loop_dynamic: return "work.loop_dynamic";
    case ompt_work_loop_guided: return "work.loop_guided";
    case ompt_work_loop_other: return "work.loop_other";
    default: return "work.unknown";
    }
}

static const char *sync_name(ompt_sync_region_t kind, int wait) {
    switch (kind) {
    case ompt_sync_region_barrier: return wait ? "wait.barrier" : "sync.barrier";
    case ompt_sync_region_barrier_implicit: return wait ? "wait.barrier_implicit" : "sync.barrier_implicit";
    case ompt_sync_region_barrier_explicit: return wait ? "wait.barrier_explicit" : "sync.barrier_explicit";
    case ompt_sync_region_barrier_implementation: return wait ? "wait.barrier_implementation" : "sync.barrier_implementation";
    case ompt_sync_region_taskwait: return wait ? "wait.taskwait" : "sync.taskwait";
    case ompt_sync_region_taskgroup: return wait ? "wait.taskgroup" : "sync.taskgroup";
    case ompt_sync_region_reduction: return wait ? "wait.reduction" : "sync.reduction";
    case ompt_sync_region_barrier_implicit_workshare: return wait ? "wait.barrier_implicit_workshare" : "sync.barrier_implicit_workshare";
    case ompt_sync_region_barrier_implicit_parallel: return wait ? "wait.barrier_implicit_parallel" : "sync.barrier_implicit_parallel";
    case ompt_sync_region_barrier_teams: return wait ? "wait.barrier_teams" : "sync.barrier_teams";
    default: return wait ? "wait.unknown" : "sync.unknown";
    }
}

static void work(ompt_work_t kind, ompt_scope_endpoint_t endpoint,
                 ompt_data_t *parallel, ompt_data_t *task, uint64_t count,
                 const void *ra) {
    int saved = errno;
    if (endpoint == ompt_scope_begin || endpoint == ompt_scope_end || endpoint == ompt_scope_beginend) {
        for (int n = 0; n < (endpoint == ompt_scope_beginend ? 2 : 1); ++n) {
            int is_begin = endpoint == ompt_scope_begin || (endpoint == ompt_scope_beginend && n == 0);
            struct rprof_omp_event e;
            if (begin(&e, is_begin ? RPROF_OMP_WORK_BEGIN : RPROF_OMP_WORK_END, work_name(kind))) {
                e.parallel_id = id(parallel);
                e.task_id = id(task);
                e.value = kind;
                e.count = count;
                e.codeptr = (uintptr_t)ra;
                rprof_omp_emit(&e);
            }
        }
    }
    errno = saved;
}

static void sync_event(ompt_sync_region_t kind, ompt_scope_endpoint_t endpoint,
                       ompt_data_t *parallel, ompt_data_t *task, const void *ra,
                       int wait) {
    if (endpoint != ompt_scope_begin && endpoint != ompt_scope_end && endpoint != ompt_scope_beginend) return;
    for (int n = 0; n < (endpoint == ompt_scope_beginend ? 2 : 1); ++n) {
        int is_begin = endpoint == ompt_scope_begin || (endpoint == ompt_scope_beginend && n == 0);
        struct rprof_omp_event e;
        uint16_t event = wait ? (is_begin ? RPROF_OMP_WAIT_BEGIN : RPROF_OMP_WAIT_END)
                              : (is_begin ? RPROF_OMP_SYNC_BEGIN : RPROF_OMP_SYNC_END);
        if (begin(&e, event, sync_name(kind, wait))) {
            e.parallel_id = id(parallel);
            e.task_id = id(task);
            e.value = kind;
            e.codeptr = (uintptr_t)ra;
            rprof_omp_emit(&e);
        }
    }
}

static void sync_region(ompt_sync_region_t kind, ompt_scope_endpoint_t endpoint,
                        ompt_data_t *parallel, ompt_data_t *task, const void *ra) {
    int saved = errno;
    sync_event(kind, endpoint, parallel, task, ra, 0);
    errno = saved;
}
static void sync_region_wait(ompt_sync_region_t kind, ompt_scope_endpoint_t endpoint,
                             ompt_data_t *parallel, ompt_data_t *task, const void *ra) {
    int saved = errno;
    sync_event(kind, endpoint, parallel, task, ra, 1);
    errno = saved;
}

static void task_create(ompt_data_t *encountering, const ompt_frame_t *frame,
                        ompt_data_t *new_task, int flags, int has_dependences,
                        const void *ra) {
    int saved = errno;
    (void)frame;
    uint64_t task_id = assign_id(new_task);
    struct rprof_omp_event e;
    if (begin(&e, RPROF_OMP_TASK_CREATE, "task.create")) {
        e.task_id = task_id;
        e.parent_task_id = id(encountering);
        e.flags = (uint32_t)flags;
        e.aux = has_dependences;
        e.codeptr = (uintptr_t)ra;
        rprof_omp_emit(&e);
    }
    errno = saved;
}

static void task_schedule(ompt_data_t *prior, ompt_task_status_t status,
                          ompt_data_t *next) {
    int saved = errno;
    struct rprof_omp_event e;
    if (begin(&e, RPROF_OMP_TASK_SCHEDULE, "task.schedule")) {
        e.task_id = id(prior);
        e.next_task_id = id(next);
        /* Preserve the raw status: detach/early_fulfill/late_fulfill are not
         * interchangeable with task_complete, nor do they close intervals. */
        e.value = status;
        rprof_omp_emit(&e);
    }
    errno = saved;
}

static void task_dependence(ompt_data_t *source, ompt_data_t *destination) {
    int saved = errno;
    struct rprof_omp_event e;
    if (begin(&e, RPROF_OMP_DEPENDENCE, "task.dependence")) {
        e.parent_task_id = id(source);
        e.task_id = id(destination);
        rprof_omp_emit(&e);
    }
    errno = saved;
}

#define REGISTER(event, callback) do { \
    ompt_set_result_t result = set_callback(event, (ompt_callback_t)(callback)); \
    registrations[registration_count++] = (struct registration){#event, result, event}; \
} while (0)

static int initialize(ompt_function_lookup_t lookup, int device, ompt_data_t *tool_data) {
    int saved = errno;
    int expected = 0;
    (void)device;
    if (!atomic_compare_exchange_strong_explicit(&state, &expected, 1,
                                                  memory_order_acq_rel, memory_order_acquire)) {
        /* A second runtime cannot share these process-global lookup functions. */
        metadata(RPROF_OMP_CAPABILITY, "runtime.rejected", ompt_set_error, 0,
                 "second OpenMP runtime rejected: tool already initialized");
        errno = saved;
        return 0;
    }
    if (!rprof_omp_init()) {
        atomic_store_explicit(&state, 0, memory_order_release);
        errno = saved;
        return 0;
    }
    ompt_set_callback_t set_callback = (ompt_set_callback_t)lookup("ompt_set_callback");
    if (!set_callback) {
        atomic_store_explicit(&state, 0, memory_order_release);
        errno = saved;
        return 0;
    }
    get_thread_data = (ompt_get_thread_data_t)lookup("ompt_get_thread_data");
    struct registration { const char *name; int64_t result, callback; };
    struct registration registrations[11];
    size_t registration_count = 0;
    REGISTER(ompt_callback_thread_begin, thread_begin);
    REGISTER(ompt_callback_thread_end, thread_end);
    REGISTER(ompt_callback_parallel_begin, parallel_begin);
    REGISTER(ompt_callback_parallel_end, parallel_end);
    REGISTER(ompt_callback_implicit_task, implicit_task);
    REGISTER(ompt_callback_work, work);
    REGISTER(ompt_callback_sync_region, sync_region);
    REGISTER(ompt_callback_sync_region_wait, sync_region_wait);
    REGISTER(ompt_callback_task_create, task_create);
    REGISTER(ompt_callback_task_schedule, task_schedule);
    REGISTER(ompt_callback_task_dependence, task_dependence);
    /* No dispatch wire kind exists; do not mislabel dispatch as work. */
    rprof_omp_activate();
    atomic_store_explicit(&state, 2, memory_order_release);
    metadata(RPROF_OMP_RUNTIME, "runtime", tool_data ? (int64_t)tool_data->value : 0,
             0, runtime_name);
    for (size_t i = 0; i < registration_count; ++i)
        metadata(RPROF_OMP_CAPABILITY, registrations[i].name,
                 registrations[i].result, registrations[i].callback, NULL);
    errno = saved;
    return 1;
}

static void finalize(ompt_data_t *tool_data) {
    int saved = errno;
    (void)tool_data;
    struct rprof_omp_event e;
    if (begin(&e, RPROF_OMP_FINALIZE, "finalize")) rprof_omp_emit(&e);
    atomic_store_explicit(&state, 3, memory_order_release);
    errno = saved;
}

static ompt_start_tool_result_t tool = {initialize, finalize, {0}};

__attribute__((visibility("default"))) ompt_start_tool_result_t *
ompt_start_tool(unsigned int omp_version, const char *runtime_version) {
    int saved = errno;
    /* Version is conveyed by tool_data, which is copied into initialize by
     * the runtime. Only the first runtime may claim this tool. */
    int expected = 0;
    if (atomic_compare_exchange_strong_explicit(&runtime_name_ready, &expected, 1,
                                                 memory_order_acq_rel, memory_order_acquire)) {
        copy_bounded(runtime_name, sizeof runtime_name, runtime_version);
        tool.tool_data.value = omp_version;
        atomic_store_explicit(&runtime_name_ready, 2, memory_order_release);
    } else {
        /* Do not hand out the shared tool_data while the first caller writes it. */
        while (atomic_load_explicit(&runtime_name_ready, memory_order_acquire) != 2) { }
    }
    errno = saved;
    return &tool;
}
