# Profiler mini-apps

Small, deterministic workloads for exercising real collectors on supported
hardware. These are opt-in hardware tests, not part of the CPU-only Cargo suite.
Keep build products in `target/` and captures/reports in their existing ignored
directories. Run on allocated compute resources, not a cluster login node.

## CPU/GPU GEMM

`gemm/` runs double-precision matrix multiplication first on the CPU using Intel
MKL, then on one GPU using NVIDIA cuBLAS. Both use the same dense, nonuniform,
column-major inputs and check every output element against an independent
closed-form result. Each phase performs one warm-up followed by timed iterations;
validation and GPU transfers are outside the GEMM timing. The profiler still
records setup, warm-up, validation, transfers and cleanup.

Requirements: Linux, an NVIDIA GPU, CUDA toolkit/cuBLAS, Intel MKL with the LP64
interface and Intel OpenMP threading, and an NVIDIA `nvcc`-supported host compiler.
The source is built with NVIDIA `nvcc`; its host compiler handles CPU code. No
NVHPC installation is required. The CPU BLAS implementation is MKL, not cuBLAS or
an OpenMP loop replacing DGEMM.

```bash
make -C miniapps/gemm \
  CUDA_HOME=/usr/local/cuda \
  MKLROOT=/opt/intel/oneapi/mkl/latest \
  IOMP_LIBDIR=/opt/intel/oneapi/compiler/latest/lib

# Requires a GPU; checks small and odd-sized matrices with one and two CPU threads.
make -C miniapps/gemm check
```

Compiler/library locations can be overridden with `NVCC`, `CUDA_HOME`, `MKLROOT`
and `IOMP_LIBDIR`. The default executable is `target/miniapps/gemm`.

The default workload uses size 4096 and 10 timed iterations per phase. Options are
`--size N`, `--iterations N` and `--threads N`. By default MKL uses all logical
CPUs in the process's Linux affinity mask, with dynamic thread reduction disabled.
An explicit thread count must not exceed that mask. There are no application
OpenMP regions and no second OpenMP runtime; MKL creates its own Intel OpenMP team.
Small matrices may still use fewer workers internally. `MKL_VERBOSE=1` exposes
MKL's requested thread count; the OpenMP capture shows observed runtime activity.

### Profile on an allocated node

Build the CLI and adapters as described in the [quick start](../docs/quickstart.md).
The following Slurm example requests all 96 physical cores on the tested H100 node
and one GPU, binds the process to those cores, and limits the job to five minutes.
Adjust CPU count, partition and memory for another cluster.

```bash
mkdir -p captures/miniapps reports/miniapps
run=$(date -u +%Y%m%dT%H%M%SZ)
srun --partition=h100 --nodes=1 --ntasks=1 --cpus-per-task=96 \
  --hint=nomultithread --cpu-bind=cores --gres=gpu:1 --mem=16G \
  --time=00:05:00 --job-name=rprof-gemm \
  env OMP_PROC_BIND=spread OMP_PLACES=cores MKL_VERBOSE=1 \
  target/release/rprof record --collect openmp,cuda,system \
  --system-interval-ms 100 --queue-chunks 64 \
  -o "captures/miniapps/gemm-$run.rprof.jsonl" -- \
  target/miniapps/gemm --size 8192 --iterations 8
target/release/rprof report "captures/miniapps/gemm-$run.rprof.jsonl" \
  --format html -o "reports/miniapps/gemm-$run.html"
```

Size 8192 requires about 1.5 GiB for the three host matrices and another 1.5 GiB
on the GPU, plus library/profiler overhead. Only the GPU assigned by the scheduler
is used (`CUDA_VISIBLE_DEVICES` is preserved; cuBLAS uses logical device 0).
The program prints affinity/thread counts, device identity, per-phase wall time,
GEMM GFLOP/s, GPU-event time and maximum absolute validation error. Timing covers
repeated DGEMMs, not end-to-end application throughput or profiler overhead.

Inspect the CPU worker lanes, GPU kernels and launch correlations, CPU/memory
graphs, source coverage and losses. MKL's internal OMPT coverage depends on the
installed runtime; it is not CPU stack sampling. CPU and GPU work are deliberately
sequential so their phases are easy to distinguish. Concurrent/overlap workloads
can be added as separate mini-apps.

The example increases the bounded writer queue from 8 to 64 chunks to accommodate
bursty OpenMP team callbacks alongside system samples. Always inspect writer loss
as well as individual collector loss; a successful workload need not be a lossless
capture.

### Recorded H100 run

On 2026-11-17, NVIDIA `nvcc` 13.0.88, MKL 2025.3 and cuBLAS 13 ran this workload
on two Intel Xeon Platinum 8480C sockets (96 physical cores total) and one H100
80 GB. Size 8192, eight timed iterations plus one warm-up per phase:

| Phase | Mean DGEMM wall time | Throughput |
| --- | ---: | ---: |
| CPU, 96 MKL threads | 235.085 ms | 4.677 TFLOP/s |
| GPU, one H100 | 16.846 ms | 65.267 TFLOP/s |

Both outputs matched every reference element exactly. The profiler observed all
96 threads in each of nine CPU parallel regions, nine GPU kernels and three
512 MiB transfers. The complete 4.656-second capture had zero writer, OpenMP ingress
or CUDA/CUPTI drops, and zero CUDA source/timing/correlation errors.
These are profiled measurements, not an overhead study or a general performance
claim; GPU-event time for the eight timed calls was 134.705 ms.
