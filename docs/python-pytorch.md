# Python and PyTorch profiling

## First implementation

The first delivery is implemented: selected CPython function execution segments,
elapsed user ranges, native thread/image identity, multiprocessing bootstrap,
SDK-free Kineto/Chrome trace import, and an opt-in scheduled `torch.profiler` helper.
Text/HTML panels distinguish native Python observations, imported CPU operators,
GPU execution, framework memory counters, and source-health diagnostics.

```sh
rprof record --collect python -- python3 application.py
rprof import-pytorch torch-trace.json -o torch.rprof.jsonl
rprof report torch.rprof.jsonl --format html -o torch.html
```

See [Python collection](python-profiling.md) and [PyTorch import and collection](pytorch-import.md)
for exact contracts and limits. Function tracing is opt-in and substantially
perturbs tiny functions; `--python-backend ranges` supports coarser annotations.
Statistical stack sampling, broader interpreter matrices, and automatic alignment
of imported framework traces with native captures remain future work.

## Goal and implementation boundary

Profile Python applications and, specifically, PyTorch training/inference while
retaining rprof's unified native timeline. Rust remains the collection/analysis
core; an optional Python-facing package or native extension provides the language
and framework integration. The base CLI continues to work without Python/PyTorch.

## Python support goals

- Start with CPython and an explicit interpreter-version matrix. Evaluate stack
  sampling for low-overhead attribution and opt-in instrumented function intervals.
  Use `sys.monitoring` where appropriate on supported versions, with clearly labeled
  fallback coverage. Do not assign exact durations to statistical stack samples.
- Capture Python function/file/line attribution, threads/interpreters, and user
  ranges. Model generator/coroutine suspension and resumption separately from
  elapsed lifetime. Treat GIL-enabled and free-threaded builds as distinct targets.
- Associate Python frames with native MPI/OpenMP/I/O/CUDA work through shared
  process/thread identity and calibrated clocks; LD_PRELOAD alone does not provide
  Python stack or operator attribution.
- Handle multiprocessing spawn/fork/forkserver and DataLoader workers with distinct
  process identities and fresh collector state. Test supported startup methods.
- Provide text/HTML stack/function summaries and timeline filtering, preserving
  sampling rate, missing data, instrumentation mode, and measured overhead.

## PyTorch support

**First increment: import `torch.profiler`/Kineto output.** Preserve CPU operators,
user `record_function` ranges, available stacks, CUDA activities, source flow IDs,
and step boundaries. Normalize timestamps with explicit source precision and clock
metadata; importing a trace cannot recover information it did not contain.

The helper now supports opt-in runtime collection with warmup/active schedules.
The broader framework coverage goals remain:

- Forward/backward/autograd operations, module/user ranges, optimizer steps, and
  training versus inference iterations.
- CPU operator calls versus actual asynchronous GPU kernels/copies/synchronization.
  Link through supplied correlation data; host duration is not GPU execution time.
- Optional tensor shapes, dtypes, stacks, and allocator events/counters. Separate
  allocated from reserved memory and framework bookkeeping from device measurements.
- DataLoader worker activity, queueing/loading stalls, pinned-memory and H2D transfers,
  and underlying observable I/O.
- Distributed training: rank/device/process-group identity, DDP/FSDP activity, and
  communication/computation overlap. Label NCCL, Gloo, and MPI according to the
  actual backend rather than treating all distributed calls as MPI.
- Eager and `torch.compile` execution, graph breaks, compiled regions, and fused
  kernels where the selected versions expose attribution. One operator does not
  necessarily map to one GPU kernel.

Coordinate ownership of GPU profiling APIs. PyTorch/Kineto and rprof's CUDA collector
must not independently start incompatible CUPTI sessions; support an imported/
framework-owned path and explicitly validated coexistence modes. Keep these choices
visible in capture metadata.

## Reports and projection evidence

Add Python stacks/ranges and framework operators to the existing hierarchical
timeline with exact native identities and source correlation namespaces. Provide
operator self/inclusive CPU time, device time, memory, data-loading and communication
breakdowns. Preserve the distinction between overlapping layers so that their
durations are not simply added into an elapsed-time total.

Shapes, dtypes, work/transfer volumes, device topology, execution mode, and iteration
structure can feed later hardware projection. FLOP estimates, fusion behavior, and
coverage must retain their assumptions; a recorded operator name alone is not a
complete hardware performance model.

## Delivery and validation

1. Define source/version/clock contracts and import representative PyTorch traces.
2. Add CPython attribution and optional bootstrap/user-range integration.
3. Add scheduled PyTorch collection and native CPU/GPU correlation.
4. Cover workers, distributed backends, compiled execution, and overhead tuning.

Validate CPU operator/autograd fixtures, small CUDA workloads on `tst9` H100/H200,
DataLoader workers, and two-rank distributed runs. Compare with PyTorch's reference
profiler and known fixture outcomes. Track CPython/PyTorch/CUDA/collector ABI
combinations independently, including unsupported or absent metadata.

Recorded validation uses CPython 3.12.9 and PyTorch 2.13.0+cu130: CPU operators,
two-rank single-host Gloo, H200 eager and compiled training, DataLoader spawn workers,
and native Python+CUPTI collection. CPU-side Python/IO and multiprocessing checks
also passed on `tst3` HBv4 (`hbv404EX3U`); `tst4` was unavailable. All scheduled
H200 kernel/copy execution records had their supplied API correlations imported.
Unpaired flow endpoints from the original trace remain explicit diagnostics.
