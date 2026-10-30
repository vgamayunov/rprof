// CUDA 13.0 / CUPTI 2025.3.1 injection adapter. The Rust hooks own the 608-byte
// wire ABI, nonblocking transport, instance/activity IDs, and shared statistics.
#include "hooks.h"
#include <cupti.h>
#include <cupti_activity.h>
#include <cupti_callbacks.h>
#include <cuda.h>
#include <atomic>
#include <cerrno>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <unistd.h>

#if CUDA_VERSION < 13000 || CUDA_VERSION >= 14000
#error "This adapter requires reviewed CUDA 13 activity record layouts"
#endif

namespace {
constexpr size_t kBufferBytes = 1u << 20;
constexpr unsigned kBuffers = 8;
alignas(64) uint8_t buffers[kBuffers][kBufferBytes];
std::atomic<uint32_t> occupied{0};
std::atomic<uint64_t> declines{0}, invalid_buffers{0}, incomplete_buffers{0}, incomplete_records{0};
std::atomic<bool> active{false};
// 0=not called, 1=initializing, 2=active, 3=failed/absent, 4=shut down.
std::atomic<unsigned> init_state{0};
pid_t owner_pid = 0;
CUpti_SubscriberHandle subscriber = nullptr;
CUpti_ActivityKind enabled[10] = {};
unsigned enabled_count = 0;
thread_local bool in_profiler = false;
thread_local uint32_t runtime_stack[32] = {};
thread_local unsigned runtime_depth = 0;
thread_local unsigned runtime_overflow_depth = 0;

struct Guard {
  int saved_errno;
  bool previous;
  Guard() : saved_errno(errno), previous(in_profiler) { in_profiler = true; }
  ~Guard() { in_profiler = previous; errno = saved_errno; }
};

bool own_process() { return owner_pid == getpid(); }

void text(char *dest, size_t capacity, const char *src, rprof_cuda_event &event) {
  if (!src || capacity == 0) return;
  size_t len = strnlen(src, capacity);
  memcpy(dest, src, len);
  if (len == capacity) {
    dest[capacity - 1] = '\0';
    event.flags |= RPROF_CUDA_NAME_TRUNCATED;
  }
}

void diagnostic(const char *name, const char *detail, int64_t value = 0,
                uint32_t flags = RPROF_CUDA_SOURCE_ERROR) {
  rprof_cuda_event e;
  if (!rprof_cuda_begin(&e, RPROF_CUDA_ERROR, name)) return;
  e.flags |= flags;
  e.value = value;
  text(e.detail, sizeof(e.detail), detail, e);
  rprof_cuda_emit(&e);
  if (flags & RPROF_CUDA_SOURCE_ERROR) rprof_cuda_source_error();
}

void cup_error(const char *stage, CUptiResult result, bool fatal = true) {
  const char *message = nullptr;
  (void)cuptiGetResultString(result, &message);
  diagnostic(stage, message ? message : "CUPTI error", result,
             fatal ? RPROF_CUDA_SOURCE_ERROR : RPROF_CUDA_OPTIONAL);
}

void capability(const char *name, CUptiResult result, bool optional) {
  rprof_cuda_event e;
  if (!rprof_cuda_begin(&e, RPROF_CUDA_CAPABILITY, name)) return;
  e.value = result == CUPTI_SUCCESS ? 1 : 0;
  e.result = static_cast<int32_t>(result);
  e.flags |= RPROF_CUDA_RESULT_KNOWN | (optional ? RPROF_CUDA_OPTIONAL : 0);
  if (result != CUPTI_SUCCESS) {
    const char *message = nullptr;
    (void)cuptiGetResultString(result, &message);
    text(e.detail, sizeof(e.detail), message, e);
  }
  rprof_cuda_emit(&e);
  if (result != CUPTI_SUCCESS && !optional) rprof_cuda_source_error();
}

// CUPTI converts device activity timestamps into this CLOCK_MONOTONIC domain.
// The GPU-to-host interpolation uncertainty is not exposed by CUPTI.
uint64_t CUPTIAPI timestamp() {
  const int saved_errno = errno;
  uint64_t now = rprof_cuda_now();
  errno = saved_errno;
  return now;
}

void times(rprof_cuda_event &e, uint64_t start, uint64_t end) {
  e.tid = 0; // CUPTI completion threads are not application threads.
  e.flags |= RPROF_CUDA_UNKNOWN_THREAD | RPROF_CUDA_MONOTONIC_CLOCK;
  if (!start || !end || start == CUPTI_TIMESTAMP_UNKNOWN ||
      end == CUPTI_TIMESTAMP_UNKNOWN || end < start) {
    e.start_ns = e.end_ns = 0;
    e.flags |= RPROF_CUDA_TIMING_UNAVAILABLE;
  } else {
    e.start_ns = start;
    e.end_ns = end;
  }
}

void gpu(rprof_cuda_event &e, uint16_t kind, const char *name,
         uint64_t start, uint64_t end, uint32_t device, uint32_t context,
         uint32_t stream, uint32_t correlation) {
  rprof_cuda_begin(&e, kind, nullptr);
  text(e.name, sizeof(e.name), name, e);
  times(e, start, end);
  e.device = device;
  e.context = context;
  e.stream = stream;
  e.correlation = correlation;
}

const char *copy_name(uint8_t kind) {
  switch (kind) {
    case CUPTI_ACTIVITY_MEMCPY_KIND_HTOD: return "memcpyHtoD";
    case CUPTI_ACTIVITY_MEMCPY_KIND_DTOH: return "memcpyDtoH";
    case CUPTI_ACTIVITY_MEMCPY_KIND_DTOD: return "memcpyDtoD";
    case CUPTI_ACTIVITY_MEMCPY_KIND_HTOH: return "memcpyHtoH";
    case CUPTI_ACTIVITY_MEMCPY_KIND_HTOA: return "memcpyHtoA";
    case CUPTI_ACTIVITY_MEMCPY_KIND_ATOH: return "memcpyAtoH";
    case CUPTI_ACTIVITY_MEMCPY_KIND_ATOA: return "memcpyAtoA";
    case CUPTI_ACTIVITY_MEMCPY_KIND_ATOD: return "memcpyAtoD";
    case CUPTI_ACTIVITY_MEMCPY_KIND_DTOA: return "memcpyDtoA";
    case CUPTI_ACTIVITY_MEMCPY_KIND_PTOP: return "memcpyPtoP";
    default: return "memcpyUnknown";
  }
}

void record(const CUpti_Activity *a) {
  rprof_cuda_event e;
  switch (a->kind) {
    case CUPTI_ACTIVITY_KIND_CONCURRENT_KERNEL: {
      const auto &k = *reinterpret_cast<const CUpti_ActivityKernel10 *>(a);
      gpu(e, RPROF_CUDA_KERNEL, k.name, k.start, k.end, k.deviceId,
          k.contextId, k.streamId, k.correlationId);
      e.flags |= RPROF_CUDA_ASYNC; // kernel launches do not wait for GPU execution.
      e.grid[0] = k.gridX; e.grid[1] = k.gridY; e.grid[2] = k.gridZ;
      e.block[0] = k.blockX; e.block[1] = k.blockY; e.block[2] = k.blockZ;
      e.cluster[0] = k.clusterX; e.cluster[1] = k.clusterY; e.cluster[2] = k.clusterZ;
      e.registers = k.registersPerThread;
      e.static_shared = k.staticSharedMemory > 0 ? k.staticSharedMemory : 0;
      e.dynamic_shared = k.dynamicSharedMemory > 0 ? k.dynamicSharedMemory : 0;
      e.graph_id = k.graphId; e.graph_node_id = k.graphNodeId;
      e.value = k.gridId; // value: CUPTI grid ID (not an elapsed duration).
      rprof_cuda_emit(&e);
      break;
    }
    case CUPTI_ACTIVITY_KIND_MEMCPY: {
      const auto &m = *reinterpret_cast<const CUpti_ActivityMemcpy6 *>(a);
      gpu(e, RPROF_CUDA_MEMCPY, copy_name(m.copyKind), m.start, m.end, m.deviceId,
          m.contextId, m.streamId, m.correlationId);
      if (m.flags & CUPTI_ACTIVITY_FLAG_MEMCPY_ASYNC) e.flags |= RPROF_CUDA_ASYNC;
      e.runtime_correlation = m.runtimeCorrelationId;
      e.bytes = m.bytes; e.operation = m.copyKind;
      e.value = m.copyKind; // value: CUPTI memcpy direction enum.
      e.aux0 = m.srcKind; e.aux1 = m.dstKind; e.aux2 = m.copyCount;
      e.graph_id = m.graphId; e.graph_node_id = m.graphNodeId;
      snprintf(e.detail, sizeof(e.detail), "copyKind=%u srcKind=%u dstKind=%u count=%llu",
               m.copyKind, m.srcKind, m.dstKind, static_cast<unsigned long long>(m.copyCount));
      rprof_cuda_emit(&e);
      break;
    }
    case CUPTI_ACTIVITY_KIND_MEMCPY2: {
      const auto &m = *reinterpret_cast<const CUpti_ActivityMemcpyPtoP4 *>(a);
      gpu(e, RPROF_CUDA_MEMCPY, copy_name(m.copyKind), m.start, m.end, m.deviceId,
          m.contextId, m.streamId, m.correlationId);
      if (m.flags & CUPTI_ACTIVITY_FLAG_MEMCPY_ASYNC) e.flags |= RPROF_CUDA_ASYNC;
      e.bytes = m.bytes; e.operation = m.copyKind; e.value = m.copyKind;
      e.src_device = m.srcDeviceId; e.dst_device = m.dstDeviceId;
      e.aux0 = m.srcKind; e.aux1 = m.dstKind;
      e.graph_id = m.graphId; e.graph_node_id = m.graphNodeId;
      snprintf(e.detail, sizeof(e.detail), "peer copyKind=%u srcKind=%u dstKind=%u",
               m.copyKind, m.srcKind, m.dstKind);
      rprof_cuda_emit(&e);
      break;
    }
    case CUPTI_ACTIVITY_KIND_MEMSET: {
      const auto &m = *reinterpret_cast<const CUpti_ActivityMemset4 *>(a);
      gpu(e, RPROF_CUDA_MEMSET, "memset", m.start, m.end, m.deviceId,
          m.contextId, m.streamId, m.correlationId);
      if (m.flags & CUPTI_ACTIVITY_FLAG_MEMSET_ASYNC) e.flags |= RPROF_CUDA_ASYNC;
      e.bytes = m.bytes; e.value = m.value; // memset fill byte/value.
      e.aux0 = m.memoryKind; e.graph_id = m.graphId; e.graph_node_id = m.graphNodeId;
      rprof_cuda_emit(&e);
      break;
    }
    case CUPTI_ACTIVITY_KIND_DEVICE: {
      const auto &d = *reinterpret_cast<const CUpti_ActivityDevice5 *>(a);
      rprof_cuda_begin(&e, RPROF_CUDA_DEVICE, nullptr);
      times(e, 0, 0);
      e.device = d.id; e.bytes = d.globalMemorySize;
      e.aux0 = d.computeCapabilityMajor; e.aux1 = d.computeCapabilityMinor;
      e.aux2 = d.numMultiprocessors;
      if (d.isCudaVisible) e.flags |= RPROF_CUDA_DEVICE_VISIBLE;
      text(e.name, sizeof(e.name), d.name, e);
      const auto *uuid = reinterpret_cast<const unsigned char *>(&d.uuid);
      snprintf(e.detail, sizeof(e.detail),
               "uuid=%02x%02x%02x%02x-%02x%02x-%02x%02x-%02x%02x-%02x%02x%02x%02x%02x%02x mig=%u",
               uuid[0], uuid[1], uuid[2], uuid[3], uuid[4], uuid[5], uuid[6], uuid[7],
               uuid[8], uuid[9], uuid[10], uuid[11], uuid[12], uuid[13], uuid[14], uuid[15],
               d.isMigEnabled);
      rprof_cuda_emit(&e);
      // Device5 explicitly transfers ownership of its name to the client.
      free(const_cast<char *>(d.name));
      break;
    }
    case CUPTI_ACTIVITY_KIND_CONTEXT: {
      const auto &c = *reinterpret_cast<const CUpti_ActivityContext3 *>(a);
      rprof_cuda_begin(&e, RPROF_CUDA_CONTEXT, "context");
      times(e, 0, 0); e.device = c.deviceId; e.context = c.contextId;
      e.stream = c.nullStreamId; e.value = c.computeApiKind;
      e.aux0 = c.parentContextId; e.aux1 = c.isGreenContext;
      e.aux2 = c.numMultiprocessors;
      rprof_cuda_emit(&e);
      break;
    }
    case CUPTI_ACTIVITY_KIND_STREAM: {
      const auto &s = *reinterpret_cast<const CUpti_ActivityStream *>(a);
      rprof_cuda_begin(&e, RPROF_CUDA_STREAM, "stream");
      times(e, 0, 0); e.context = s.contextId; e.stream = s.streamId;
      e.correlation = s.correlationId; e.value = static_cast<int64_t>(s.priority);
      e.aux0 = s.flag;
      rprof_cuda_emit(&e);
      break;
    }
    case CUPTI_ACTIVITY_KIND_SYNCHRONIZATION: {
      const auto &s = *reinterpret_cast<const CUpti_ActivitySynchronization2 *>(a);
      gpu(e, RPROF_CUDA_SYNC, "host synchronization", s.start, s.end,
          UINT32_MAX, s.contextId, s.streamId, s.correlationId);
      if (e.stream == CUPTI_SYNCHRONIZATION_INVALID_VALUE) e.stream = UINT32_MAX;
      e.value = s.type; e.aux0 = s.cudaEventId; e.aux1 = s.cudaEventSyncId;
      e.result = static_cast<int32_t>(s.returnValue); e.flags |= RPROF_CUDA_RESULT_KNOWN;
      text(e.detail, sizeof(e.detail), "Host API synchronization duration, not GPU execution", e);
      rprof_cuda_emit(&e);
      break;
    }
    case CUPTI_ACTIVITY_KIND_EXTERNAL_CORRELATION: {
      const auto &x = *reinterpret_cast<const CUpti_ActivityExternalCorrelation *>(a);
      rprof_cuda_begin(&e, RPROF_CUDA_EXTERNAL, "external correlation");
      times(e, 0, 0); e.correlation = x.correlationId;
      e.value = x.externalKind; e.aux0 = x.externalId;
      rprof_cuda_emit(&e);
      break;
    }
    default: break;
  }
}

void CUPTIAPI request_buffer(uint8_t **buffer, size_t *size, size_t *max_records) {
  Guard guard;
  *buffer = nullptr; *size = 0; *max_records = 0;
  if (!own_process()) return;
  for (unsigned i = 0; i < kBuffers; ++i) {
    uint32_t bit = 1u << i;
    uint32_t old = occupied.load(std::memory_order_relaxed);
    while (!(old & bit)) {
      if (occupied.compare_exchange_weak(old, old | bit, std::memory_order_acq_rel)) {
        *buffer = buffers[i]; *size = kBufferBytes;
        return;
      }
    }
  }
  declines.fetch_add(1, std::memory_order_relaxed);
}

void dropped() {
  size_t count = 0;
  CUptiResult status = cuptiActivityGetNumDroppedRecords(nullptr, 0, &count);
  if (status == CUPTI_SUCCESS) {
    if (count) rprof_cuda_source_drop(count); // CUPTI resets the count on each query.
  } else cup_error("dropped-record query", status);
}

void CUPTIAPI complete_buffer(CUcontext, uint32_t, uint8_t *buffer, size_t, size_t valid) {
  if (!own_process()) return; // never touch inherited CUPTI state in a fork child.
  Guard guard;
  uint32_t bit = 0;
  for (unsigned i = 0; i < kBuffers; ++i)
    if (buffer == buffers[i]) { bit = 1u << i; break; }
  if (!bit || valid > kBufferBytes) {
    invalid_buffers.fetch_add(1, std::memory_order_relaxed);
    diagnostic("activity buffer", "invalid pointer or size");
    if (bit) occupied.fetch_and(~bit, std::memory_order_release);
    return;
  }
  CUpti_Activity *item = nullptr;
  for (;;) {
    CUptiResult result = cuptiActivityGetNextRecord(buffer, valid, &item);
    if (result == CUPTI_ERROR_MAX_LIMIT_REACHED) break;
    if (result == CUPTI_ERROR_INVALID_KIND) {
      // Forced flush can contain incomplete activities. Do not invent spans.
      incomplete_buffers.fetch_add(1, std::memory_order_relaxed);
      incomplete_records.fetch_add(1, std::memory_order_relaxed);
      diagnostic("incomplete activity", "CUPTI INVALID_KIND: incomplete/invalid record on flush",
                 result, RPROF_CUDA_SOURCE_INCOMPLETE);
      break;
    }
    if (result != CUPTI_SUCCESS || !item) {
      incomplete_buffers.fetch_add(1, std::memory_order_relaxed);
      cup_error("activity decode", result);
      break;
    }
    record(item);
  }
  dropped();
  occupied.fetch_and(~bit, std::memory_order_release);
}

bool sync_function(const char *name) {
  if (!name) return false;
  return strstr(name, "Synchronize") != nullptr ||
         strstr(name, "cudaEventQuery") != nullptr ||
         strstr(name, "cuEventQuery") != nullptr;
}

void CUPTIAPI api_callback(void *, CUpti_CallbackDomain domain, CUpti_CallbackId cbid,
                           const void *data) {
  if (!active.load(std::memory_order_acquire) || !own_process() || in_profiler || !data) return;
  if (domain != CUPTI_CB_DOMAIN_RUNTIME_API && domain != CUPTI_CB_DOMAIN_DRIVER_API) return;
  Guard guard;
  const auto &c = *static_cast<const CUpti_CallbackData *>(data);
  const bool runtime = domain == CUPTI_CB_DOMAIN_RUNTIME_API;
  const uint64_t now = rprof_cuda_now();
  if (c.callbackSite == CUPTI_API_ENTER) {
    if (c.correlationData) *c.correlationData = now;
    if (runtime) {
      if (runtime_overflow_depth) ++runtime_overflow_depth;
      else if (runtime_depth < 32) runtime_stack[runtime_depth++] = c.correlationId;
      else {
        runtime_overflow_depth = 1;
        diagnostic("runtime stack overflow", "driver parent correlation unavailable during overflow");
      }
    }
  }
  rprof_cuda_event e;
  rprof_cuda_begin(&e, c.callbackSite == CUPTI_API_ENTER ? RPROF_CUDA_API_BEGIN : RPROF_CUDA_API_END,
                   nullptr);
  text(e.name, sizeof(e.name), c.functionName, e);
  e.api_domain = runtime ? RPROF_CUDA_DOMAIN_RUNTIME : RPROF_CUDA_DOMAIN_DRIVER;
  e.operation = cbid; e.correlation = c.correlationId;
  e.context = c.contextUid;
  if (!runtime && !runtime_overflow_depth && runtime_depth)
    e.parent_correlation = runtime_stack[runtime_depth - 1];
  if (c.callbackSite == CUPTI_API_EXIT &&
      (!c.correlationData || !*c.correlationData || *c.correlationData > now)) {
    e.start_ns = 0;
    e.flags |= RPROF_CUDA_TIMING_UNAVAILABLE;
  } else {
    e.start_ns = c.callbackSite == CUPTI_API_EXIT ? *c.correlationData : now;
  }
  e.end_ns = now;
  e.flags |= RPROF_CUDA_MONOTONIC_CLOCK;
  if (c.callbackSite == CUPTI_API_EXIT && c.functionReturnValue) {
    // Both cudaError_t and CUresult are 32-bit enum return values.
    memcpy(&e.result, c.functionReturnValue, sizeof(e.result));
    e.flags |= RPROF_CUDA_RESULT_KNOWN;
  }
  rprof_cuda_emit(&e);
  if (runtime && c.callbackSite == CUPTI_API_EXIT && runtime_overflow_depth) {
    --runtime_overflow_depth;
  } else if (runtime && c.callbackSite == CUPTI_API_EXIT && runtime_depth) {
    if (runtime_stack[runtime_depth - 1] == c.correlationId) --runtime_depth;
    else {
      diagnostic("runtime stack mismatch", "nested callback correlation did not match");
      for (unsigned i = runtime_depth; i > 0; --i) {
        if (runtime_stack[i - 1] == c.correlationId) { runtime_depth = i - 1; break; }
      }
    }
  }
  // No CUDA API is called here. Normal flush after a completed synchronization
  // lets CUPTI report completed GPU work without introducing synchronization.
  if (c.callbackSite == CUPTI_API_EXIT && sync_function(c.functionName)) {
    CUptiResult result = cuptiActivityFlushAll(0);
    if (result != CUPTI_SUCCESS) cup_error("syncpoint flush", result);
  }
}

void shutdown_adapter() {
  if (!own_process() || !subscriber) return;
  Guard guard;
  active.store(false, std::memory_order_release);
  CUptiResult result = cuptiActivityFlushAll(CUPTI_ACTIVITY_FLAG_FLUSH_FORCED);
  const bool flush_ok = result == CUPTI_SUCCESS;
  if (result != CUPTI_SUCCESS) cup_error("final flush", result);
  dropped();
  for (unsigned i = enabled_count; i; --i) {
    result = cuptiActivityDisable(enabled[i - 1]);
    if (result != CUPTI_SUCCESS) cup_error("activity disable", result);
  }
  result = cuptiUnsubscribe(subscriber);
  if (result != CUPTI_SUCCESS) cup_error("unsubscribe", result);
  subscriber = nullptr;
  rprof_cuda_event e;
  if (rprof_cuda_begin(&e, RPROF_CUDA_FINALIZE, "CUPTI collector")) {
    e.flags |= RPROF_CUDA_FORCED_FLUSH;
    e.value = static_cast<int64_t>(declines.load()); // declined pool allocations
    e.aux0 = invalid_buffers.load(); e.aux1 = incomplete_buffers.load();
    e.aux2 = occupied.load();
    snprintf(e.detail, sizeof(e.detail), "pool_declines=%llu invalid_buffers=%llu incomplete_buffers=%llu incomplete_records=%llu busy_mask=%u",
             static_cast<unsigned long long>(declines.load()),
             static_cast<unsigned long long>(invalid_buffers.load()),
             static_cast<unsigned long long>(incomplete_buffers.load()),
             static_cast<unsigned long long>(incomplete_records.load()), occupied.load());
    rprof_cuda_emit(&e);
  }
  // Only a successful forced flush constitutes normal finalization.
  // Disable/unsubscribe failures are reported independently above.
  if (flush_ok) rprof_cuda_finalized();
  init_state.store(4, std::memory_order_release);
}

bool enable(CUpti_ActivityKind kind, const char *name, bool optional) {
  CUptiResult result = cuptiActivityEnable(kind);
  capability(name, result, optional);
  if (result == CUPTI_SUCCESS) enabled[enabled_count++] = kind;
  return result == CUPTI_SUCCESS || optional;
}
} // namespace

extern "C" int InitializeInjection() {
  const int saved_errno = errno;
  unsigned expected = 0;
  if (!init_state.compare_exchange_strong(expected, 1, std::memory_order_acq_rel)) {
    errno = saved_errno;
    return 1;
  }
  if (!rprof_cuda_init()) {
    init_state.store(3, std::memory_order_release);
    errno = saved_errno;
    return 1;
  }
  owner_pid = getpid();
  char previous[128] = {};
  CUpti_SubscriberParams params = {};
  params.structSize = CUpti_SubscriberParams_STRUCT_SIZE;
  params.subscriberName = "rprof-cuda";
  params.oldSubscriberName = previous;
  params.oldSubscriberSize = sizeof(previous);
  CUptiResult result = cuptiSubscribe_v2(&subscriber, api_callback, nullptr, &params);
  if (result != CUPTI_SUCCESS) {
    if (result == CUPTI_ERROR_MULTIPLE_SUBSCRIBERS_NOT_SUPPORTED)
      diagnostic("subscriber conflict", previous[0] ? previous : "another CUPTI subscriber owns callbacks", result);
    else cup_error("subscribe", result);
    subscriber = nullptr;
    init_state.store(3, std::memory_order_release);
    errno = saved_errno;
    return 1; // profiler failure must never replace application CUDA results.
  }
  result = cuptiActivityRegisterCallbacks(request_buffer, complete_buffer);
  if (result != CUPTI_SUCCESS) { cup_error("activity callbacks", result); goto failed; }
  result = cuptiActivityRegisterTimestampCallback(timestamp);
  if (result != CUPTI_SUCCESS) { cup_error("monotonic timestamp", result); goto failed; }
  if (!enable(CUPTI_ACTIVITY_KIND_CONCURRENT_KERNEL, "concurrent_kernel", false) ||
      !enable(CUPTI_ACTIVITY_KIND_MEMCPY, "memcpy", false) ||
      !enable(CUPTI_ACTIVITY_KIND_MEMSET, "memset", false)) goto failed;
  enable(CUPTI_ACTIVITY_KIND_MEMCPY2, "memcpy_peer", true);
  enable(CUPTI_ACTIVITY_KIND_DEVICE, "device", true);
  enable(CUPTI_ACTIVITY_KIND_CONTEXT, "context", true);
  enable(CUPTI_ACTIVITY_KIND_STREAM, "stream", true);
  enable(CUPTI_ACTIVITY_KIND_SYNCHRONIZATION, "synchronization", true);
  enable(CUPTI_ACTIVITY_KIND_EXTERNAL_CORRELATION, "external_correlation", true);
  result = cuptiEnableDomain(1, subscriber, CUPTI_CB_DOMAIN_RUNTIME_API);
  capability("runtime_callbacks", result, false);
  if (result != CUPTI_SUCCESS) goto failed;
  result = cuptiEnableDomain(1, subscriber, CUPTI_CB_DOMAIN_DRIVER_API);
  capability("driver_callbacks", result, false);
  if (result != CUPTI_SUCCESS) goto failed;
  {
    uint32_t version = 0;
    result = cuptiGetVersion(&version);
    if (result != CUPTI_SUCCESS) { cup_error("CUPTI version", result); goto failed; }
    if (atexit(shutdown_adapter) != 0) {
      diagnostic("atexit registration", "normal exit flush unavailable");
      goto failed;
    }
    rprof_cuda_activate();
    active.store(true, std::memory_order_release);
    init_state.store(2, std::memory_order_release);
    rprof_cuda_event info;
    if (rprof_cuda_begin(&info, RPROF_CUDA_RUNTIME, "CUDA 13 CUPTI activity")) {
      info.flags |= RPROF_CUDA_MONOTONIC_CLOCK;
      info.value = version; // numeric CUPTI version from cuptiGetVersion.
      info.aux0 = CUDA_VERSION; info.aux1 = 0; // no CUDA driver API during injection.
      info.aux2 = kBufferBytes; info.bytes = kBuffers * kBufferBytes;
      text(info.detail, sizeof(info.detail),
           "CLOCK_MONOTONIC; GPU timestamps interpolated by CUPTI; uncertainty unavailable", info);
      rprof_cuda_emit(&info);
    }
  }
  errno = saved_errno;
  return 1;
failed:
  active.store(false, std::memory_order_release);
  for (unsigned i = enabled_count; i; --i) (void)cuptiActivityDisable(enabled[i - 1]);
  (void)cuptiUnsubscribe(subscriber);
  subscriber = nullptr;
  enabled_count = 0;
  init_state.store(3, std::memory_order_release);
  errno = saved_errno;
  return 1;
}
