# Profiling quick start

## 1. Build

From the repository root, with a current stable Rust toolchain:

```bash
cargo build --release -p rprof-cli
export PATH="$PWD/target/release:$PATH"
```

Build only the native adapters you need, on a system compatible with the workload:

| Profile | Build command |
| --- | --- |
| POSIX I/O | `make preload PROFILE=release` |
| MPI | `make mpi PROFILE=release MPICC=mpicc` (use the application's MPI module) |
| LLVM/Intel OpenMP | `make ompt PROFILE=release` |
| GCC 13/libgomp, Linux x86_64 | `make gomp PROFILE=release GOMP_CC=gcc` (matching target runtime) |
| CUDA 13/CUPTI | `make cuda PROFILE=release CUDA_HOME=/usr/local/cuda` |
| Process, Python, Linux perf counters, PyTorch import | No native adapter build needed |

Keep the adapter libraries alongside `rprof`. Use fresh output names for each run.

## 2. Collect

Replace `./app` and its arguments with your workload. Choose a command below:

```bash
# Process lifetime and exit status (default)
rprof record -o process.jsonl -- ./app

# File operations, bytes, errors and latency
rprof record --collect io -o io.jsonl -- ./app

# LLVM libomp / Intel libiomp5
rprof record --collect openmp -o openmp.jsonl -- ./app

# Actual GCC 13/libgomp; start with coarse region detail
rprof record --collect openmp --openmp-backend gomp \
  --gomp-detail regions -o gomp.jsonl -- ./app

# GPU kernels/copies and CPU CUDA API calls
rprof record --collect cuda -o cuda.jsonl -- ./app

# Linux perf counters (requires permitted perf access; not stack sampling)
rprof record --collect pmu -o pmu.jsonl -- ./app

# Selected Python function execution (can add substantial overhead)
rprof record --collect python --python-include /absolute/path/to/source \
  -o python.jsonl -- python3 app.py

# Combine Python explicit ranges with file I/O; no automatic function tracing
rprof record --collect python,io --python-backend ranges \
  -o python-io.jsonl -- python3 app.py
```

Combine compatible collectors with commas, e.g. `--collect io,openmp,pmu`.
Python `ranges` records annotations added with `rprof_python.range("name")`;
without annotations it provides interpreter diagnostics, not function spans.
GNU OpenMP `bodies` adds task bodies; `full` adds API/chunk detail and more traffic.

### MPI / multiple ranks

Run one profiler **per training/application rank**, inside the launcher:

```bash
export RPROF_SESSION_ID=$(cat /proc/sys/kernel/random/uuid)
mpirun -np 2 -x RPROF_SESSION_ID -x LD_LIBRARY_PATH \
  rprof record --collect mpi,io --mpi-clock-sync \
  --output-dir captures/run1 -- ./mpi-app
rprof merge -o mpi.jsonl captures/run1/*.rprof.jsonl
```

This example uses Open MPI. With Slurm, preserve your working allocation/binding
and use `srun rprof record ... -- ./app`. Ensure binaries/libraries are available
on every node. Enable `--mpi-clock-sync` only when **all participating MPI ranks**
are profiled with that option. NCCL/Gloo are not MPI.

### PyTorch / training frameworks

Enable your framework's `torch.profiler`/Kineto Chrome JSON export for a short window
(start with 1–3 active steps after warmup), then import it:

```bash
rprof import-pytorch trace.json -o torch.jsonl
```

For simultaneous I/O and optional Python ranges, wrap the **rank-local** training
command while its framework profiler is enabled:

```bash
rprof record --collect python,io --python-backend ranges \
  --output-dir captures/training -- python3 train.py
```

This produces native captures plus the framework's separately exported trace.
Leave `--collect cuda` off and `CUDA_INJECTION64_PATH` unset while Kineto owns GPU
profiling. Native Python/I/O share a clock; imported PyTorch traces remain separate,
unaligned clock domains. See [PyTorch collection](pytorch-import.md) for a scheduled
Python helper, or the [Anemoi quick start](quickstart-anemoi.md).

## 3. View

Use any capture produced above:

```bash
rprof report io.jsonl                              # Text summary
rprof view io.jsonl --open                         # Local browser viewer
rprof report io.jsonl --format html -o io.html      # Portable offline HTML
```

For remote viewing, run `rprof view io.jsonl --port 8080` remotely, then run
`ssh -N -L 8080:127.0.0.1:8080 user@host` locally and open `http://127.0.0.1:8080`.

Start with a short representative run. Check slowdown, collector drops, incomplete
intervals and report omissions before scaling up. PyTorch import currently allows
256 MiB uncompressed JSON / one million source records per file; the default HTML
timeline retains the first 200,000 observations or 64 MiB. See [capacity](capacity.md)
and the linked collector guides for coverage and limits.
