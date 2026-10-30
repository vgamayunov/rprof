// CUDA 13 fixture: bounded, two nonblocking streams and optional second device.
// cuda events give a GPU-side elapsed reference (not a CUPTI/host clock match).
#include <cuda_runtime.h>
#include <cstdio>
#include <cstring>

namespace {
constexpr int kCount = 4096;
constexpr int kLaunches = 3;
constexpr int kStreams = 2;
// Per device: 4 H2D + 2 D2H copies of 16 KiB = 96 KiB memcpy,
// plus 2 x 16 KiB memset, and 6 named kernel launches.

bool ok(cudaError_t result, const char *what) {
  if (result == cudaSuccess) return true;
  fprintf(stderr, "%s: %s\n", what, cudaGetErrorString(result));
  return false;
}
} // namespace

// Extern C ensures a stable name in CUPTI kernel activities.
extern "C" __global__ void timed_add(int *out, const int *rhs, int n,
                                      unsigned long long spin_cycles) {
  unsigned long long start = clock64();
  while (clock64() - start < spin_cycles) { /* small GPU clock-based delay */ }
  int i = blockIdx.x * blockDim.x + threadIdx.x;
  if (i < n) out[i] += rhs[i];
}

static bool run_device(int device) {
  if (!ok(cudaSetDevice(device), "set device")) return false;
  int clock_khz = 0;
  if (!ok(cudaDeviceGetAttribute(&clock_khz, cudaDevAttrClockRate, device), "device clock rate")) return false;
  cudaStream_t streams[kStreams]{};
  cudaEvent_t starts[kStreams]{}, ends[kStreams]{};
  int *host_a[kStreams]{}, *host_b[kStreams]{}, *host_out[kStreams]{};
  int *dev_a[kStreams]{}, *dev_b[kStreams]{};
  bool success = true;
  const size_t bytes = kCount * sizeof(int);
  // clockRate is in kHz: clockRate * 6 is approximately six milliseconds.
  const auto cycles = static_cast<unsigned long long>(clock_khz) * 6;
  for (int s = 0; s < kStreams; ++s) {
    if (!ok(cudaStreamCreateWithFlags(&streams[s], cudaStreamNonBlocking), "stream create") ||
        !ok(cudaEventCreate(&starts[s]), "event start") ||
        !ok(cudaEventCreate(&ends[s]), "event end") ||
        !ok(cudaMallocHost(&host_a[s], bytes), "pinned input") ||
        !ok(cudaMallocHost(&host_b[s], bytes), "pinned rhs") ||
        !ok(cudaMallocHost(&host_out[s], bytes), "pinned output") ||
        !ok(cudaMalloc(&dev_a[s], bytes), "device input") ||
        !ok(cudaMalloc(&dev_b[s], bytes), "device rhs")) { success = false; goto cleanup; }
    for (int i = 0; i < kCount; ++i) {
      host_a[s][i] = i + s;
      host_b[s][i] = 2 + s;
      host_out[s][i] = 0;
    }
  }
  for (int s = 0; s < kStreams; ++s) {
    if (!ok(cudaEventRecord(starts[s], streams[s]), "event record start") ||
        !ok(cudaMemsetAsync(dev_a[s], 0, bytes, streams[s]), "memset") ||
        !ok(cudaMemcpyAsync(dev_a[s], host_a[s], bytes, cudaMemcpyHostToDevice, streams[s]), "H2D input") ||
        !ok(cudaMemcpyAsync(dev_b[s], host_b[s], bytes, cudaMemcpyHostToDevice, streams[s]), "H2D rhs")) {
      success = false; goto cleanup;
    }
    for (int launch = 0; launch < kLaunches; ++launch) {
      // Eight blocks cover all 4096 values while leaving most SMs free for
      // the other stream; each block spins for roughly six milliseconds.
      timed_add<<<8, 512, 0, streams[s]>>>(dev_a[s], dev_b[s], kCount, cycles);
      if (!ok(cudaGetLastError(), "kernel launch")) { success = false; goto cleanup; }
    }
    if (!ok(cudaMemcpyAsync(host_out[s], dev_a[s], bytes, cudaMemcpyDeviceToHost, streams[s]), "D2H") ||
        !ok(cudaEventRecord(ends[s], streams[s]), "event record end")) { success = false; goto cleanup; }
  }
  for (int s = 0; s < kStreams; ++s) {
    if (!ok(cudaStreamSynchronize(streams[s]), "stream synchronize")) { success = false; goto cleanup; }
    float elapsed_ms = 0;
    if (!ok(cudaEventElapsedTime(&elapsed_ms, starts[s], ends[s]), "CUDA event elapsed")) {
      success = false; goto cleanup;
    }
    printf("device=%d stream=%d CUDA-event-elapsed-ms=%.3f\n", device, s, elapsed_ms);
    for (int i = 0; i < kCount; ++i) {
      if (host_out[s][i] != i + s + kLaunches * (2 + s)) {
        fprintf(stderr, "mismatch device=%d stream=%d index=%d\n", device, s, i);
        success = false; goto cleanup;
      }
    }
    if (!ok(cudaEventSynchronize(ends[s]), "event synchronize")) { success = false; goto cleanup; }
  }
  if (!ok(cudaDeviceSynchronize(), "device synchronize")) success = false;
cleanup:
  for (int s = 0; s < kStreams; ++s) {
    if (streams[s]) (void)cudaStreamSynchronize(streams[s]);
    if (dev_a[s]) (void)cudaFree(dev_a[s]);
    if (dev_b[s]) (void)cudaFree(dev_b[s]);
    if (host_a[s]) (void)cudaFreeHost(host_a[s]);
    if (host_b[s]) (void)cudaFreeHost(host_b[s]);
    if (host_out[s]) (void)cudaFreeHost(host_out[s]);
    if (starts[s]) (void)cudaEventDestroy(starts[s]);
    if (ends[s]) (void)cudaEventDestroy(ends[s]);
    if (streams[s]) (void)cudaStreamDestroy(streams[s]);
  }
  return success;
}

int main(int argc, char **argv) {
  if (argc > 2 || (argc == 2 && strcmp(argv[1], "--two-devices") != 0)) {
    fprintf(stderr, "usage: %s [--two-devices]\n", argv[0]);
    return 2;
  }
  int count = 0;
  if (!ok(cudaGetDeviceCount(&count), "device count") || count < 1) return 1;
  if (!run_device(0)) return 1;
  if (argc == 2) {
    if (count < 2) puts("second CUDA device not visible; skipping");
    else if (!run_device(1)) return 1;
  }
  puts("verified");
  return 0;
}
