# CUDA profiling

The optional target-built `librprof_cuda.so` collects CUDA 13 CUPTI activities.
The base Rust workspace does not require the CUDA SDK or a GPU.

```sh
make build cuda PROFILE=release CUDA_HOME=/usr/local/cuda
CUDA_VISIBLE_DEVICES=0 target/release/rprof record --collect cuda \
  -o gpu.rprof.jsonl -- ./application
target/release/rprof report gpu.rprof.jsonl
target/release/rprof report gpu.rprof.jsonl --format html -o gpu.html
target/release/rprof report gpu.rprof.jsonl --format trace -o gpu.trace.json
```

Use `--cuda-library /absolute/path/librprof_cuda.so` when the library is not beside
the CLI. Build on a system compatible with the target glibc and toolkit. The C++
adapter version-gates CUDA 13 activity layouts; earlier SDKs need a reviewed
adapter. `CUDA_INJECTION64_PATH` invokes `InitializeInjection` on CUDA startup.
An existing injection path is rejected before launch. The adapter claims CUPTI's
subscriber slot before activity setup; an existing subscriber produces a saved
diagnostic without replacing the other tool or changing CUDA API results. This
also applies to future Kineto/PyTorch integration: two independent subscribers
cannot collect together.

## Collected observations

- Runtime and driver API entry/exit, application TID, return code, and nesting.
- Concurrent GPU kernels, bounded names, grid/block/cluster geometry, registers,
  static/dynamic shared-memory bytes, and available graph/node identifiers.
- Memory copies, direction, byte count and memory kinds; peer records additionally
  preserve source/destination devices. Memsets retain fill value and byte count.
- Device name, UUID/MIG identity, CUDA visibility, memory, compute capability and
  multiprocessor count; context and stream metadata.
- Host synchronization activities, attached only when an API thread can be
  resolved. They are host intervals, not GPU work or inferred stream dependencies.
- Optional CUPTI external-correlation records, retained as metadata for later
  higher-level framework adapters.

GPU tracks are keyed by process image, device, context, and stream. They never use
the CUPTI completion-worker TID. Driver/runtime API correlations occupy separate
namespaces; GPU execution has its own identity and explicit links to host calls.
The bounded cache retains 65,536 recent API identities; missing or ambiguous
matches are counted rather than assigned to an unrelated thread. Kernel and API
names exceeding 191 bytes carry a truncation flag.

## Timing and interpretation

Before enabling activities, the adapter registers a `CLOCK_MONOTONIC` timestamp
callback. CUPTI normalizes device timestamps to that clock. The capture then
subtracts its host monotonic origin. CUPTI does not expose the interpolation
uncertainty; reports display it as unknown, never zero. Cross-host clocks still
need explicit alignment as described in [reporting](reporting.md).

Unavailable, reversed, pre-origin, or future intervals become diagnostics, not
fabricated execution spans. An API entry without a usable exit remains incomplete.
Summed kernel durations may exceed elapsed time when streams overlap; they are
not GPU utilization. Runtime and driver API times can also overlap through nesting.
Activity timing does not measure FLOPs, achieved bandwidth, occupancy, or PMU
metrics; those require explicit additional profiling support/passes.

## Buffering, lifecycle, and losses

Eight aligned 1 MiB host activity buffers bound the native pool. Records cross a
fixed 608-byte pointer-free ABI into Rust and an inherited nonblocking Unix-datagram
transport. External conversion uses the existing bounded native/writer queues.
`--io-buffer-events` currently sets the shared native queue capacity for CUDA too.
Normal synchronization API exits request completed activities; normal process exit
forces a final flush. The collector does not add CUDA synchronization calls.

Reports distinguish CUPTI source drops, native ingress drops, writer loss, source
errors, unusable timing, and unresolved/ambiguous attribution. Missing finalization
is explicit, including `_Exit`, fatal signals, and incomplete process images.
A successful flush does not guarantee every in-flight activity had timing; incomplete
buffer records are separately reported. Devices with metadata but no work are
labeled accordingly; only actual timed activity creates GPU timeline tracks.

A workload that never initializes CUDA retains its exit code in the capture and
causes a collector-failed result. Forking after CUDA initialization does not use
inherited CUPTI state in the child; exec starts a new process image. Use separate
per-rank collection for MPI+CUDA and merge normally. Broader mixed-runtime,
CUDA Graph, peer-copy, and fork/exec matrices remain follow-up validation.

## Validation

On an allocated CUDA 13 GPU host, run:

```sh
CUDA_VISIBLE_DEVICES=0 PROFILE=release bash scripts/test-cuda.sh
```

The script builds a two-nonblocking-stream fixture with six named kernels, six
copies totaling 98,304 bytes, and two memsets totaling 32,768 bytes. It checks all
output elements and prints CUDA-event elapsed-time references. Tests check actual
cross-stream overlap, correlations, exact aggregate counts, timing coverage,
subscriber conflict, and missing final flush. The fixture accepts `--two-devices`
for additional device-identity validation. `RPROF_CUDA_ARCH` overrides the default
`-arch=sm_90` compiler option for other validation hardware.

SDK-free tests exercise decoder timing failures, synchronization attribution,
ambiguous correlations, bounded cache eviction, precise values above 2^53, empty
initialization, and injection-tool conflicts. Browser tests cover domain filtering,
partial sources, exact values, pagination, and dark/light themes.

### Recorded H100 validation

Validated on `tst9` / `h10096VKQ6` with H100 80GB HBM3, driver 580.105.08,
CUDA 13.0.88, CUPTI 2025.3.1 (`cuptiGetVersion=130001`), and Rust 1.90.0.

| Check | Result |
| --- | --- |
| One-device fixture | 6 kernels, 6 copies / 98,304 B, 2 memsets / 32,768 B |
| Two-device fixture | Same counts/bytes on each device; separate GPU/stream tracks |
| One-device kernel sum / GPU activity window | 35.737523 ms / 18.178616 ms |
| CUDA events, streams 0 / 1 | 18.100 ms / 17.931 ms |
| Nsight Systems 2026.1.3 kernel sum | 36.008150 ms, 6 launches |
| Kernel-sum difference from separate reference run | 0.752% |
| Native / CUPTI drops, source errors | 0 / 0 / 0 on both captures |
| Unusable timing / unresolved / ambiguous correlations | 0 / 0 / 0 |
| Subscriber conflict / `_Exit` missing flush | Both detected; application exit preserved |
| One-event ingress queue overload control | 570 attempted = 78 received + 492 ingress drops; CUPTI drops 0 |

The reference was collected separately, with `nsys profile --trace=cuda
--sample=none --cpuctxsw=none`, then `nsys stats --report
cuda_gpu_kern_sum,cuda_gpu_mem_time_sum,cuda_gpu_mem_size_sum --format csv`.
It also recorded four H2D copies, two D2H copies, and two memsets with matching
volumes. Small-copy durations vary between runs and tools; no transfer-latency
equivalence or general overhead bound is claimed by this fixture.

The exported one/two-device HTML artifacts were opened in Chromium and checked
for device counts, exact totals, source health, unknown clock uncertainty, and
JavaScript errors. The full UI suite passed all 17 browser tests, including the
native I/O regression. Saved local artifacts are under `reports/phase6/`, with
source captures under `captures/phase6/` (both ignored by Git).
