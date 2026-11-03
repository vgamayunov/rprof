# Quick start: Anemoi training with PyTorch, Python and I/O profiles

**Recommended first run:** use Anemoi's built-in PyTorch profiler for a few training
batches, and collect POSIX I/O alongside it with rprof. Import the PyTorch trace
afterward. No training-loop changes are needed for operator/GPU and I/O profiling.

Keep your working model, dataset, precision, GPU layout and launcher. A sharded
Anemoi model may require multiple GPUs even for a small profiling run. Use the
smallest **already-working** configuration rather than forcing one GPU.

## 1. Prepare rprof and your existing Anemoi environment

On the training system (or in the training container), from the rprof repository:

```bash
cargo build --release -p rprof-cli
make preload PROFILE=release
export RPROF="$PWD/target/release/rprof"
```

Keep `librprof_io.so` alongside `rprof`, or pass `--io-library /absolute/path/to/librprof_io.so`.
Build the native library against the target system's glibc. Importing/reporting alone
needs only the CLI, no PyTorch installation or GPU.

Activate the environment that already runs your benchmark, then check it:

```bash
anemoi-training profile --help
python -c 'import torch; from importlib.metadata import version; print("Anemoi", version("anemoi-training")); print("Torch", torch.__version__, "CUDA", torch.version.cuda, "available", torch.cuda.is_available())'
```

If the `profile` command needs extra dependencies, install the `profile` extra for
**your existing Anemoi version/checkout**, e.g. for an editable anemoi-core checkout:

```bash
python -m pip install -e '/path/to/anemoi-core/training[profile]'
```

Anemoi owns the PyTorch/Kineto GPU profiler in this workflow. Launch without an
existing `CUDA_INJECTION64_PATH` and leave rprof's `--collect cuda` off.

## 2. Configure a short trace window

These are Bash examples. Substitute your config directory/name, retain your usual
model/data overrides, and use a fresh output directory for each run:

```bash
CONFIG_DIR=/absolute/path/to/your/anemoi/configs
CONFIG_NAME=your-benchmark
OUT=/shared/path/to/profiles/anemoi-$(date -u +%Y%m%dT%H%M%SZ)
mkdir -p "$OUT/native" "$OUT/torch" "$OUT/reports"
export RPROF_SESSION_ID=$(cat /proc/sys/kernel/random/uuid)

PROFILE_ARGS=(
  --config-path "$CONFIG_DIR" --config-name "$CONFIG_NAME"
  training.max_epochs=1
  training.num_sanity_val_steps=0
  dataloader.limit_batches.training=20
  dataloader.limit_batches.validation=0
  diagnostics.benchmark_profiler.memory.enabled=true
  diagnostics.benchmark_profiler.memory.warmup=10
  diagnostics.benchmark_profiler.memory.steps=3
  diagnostics.benchmark_profiler.memory.extra_plots=false
  diagnostics.benchmark_profiler.memory.trace_rank0_only=false
  diagnostics.benchmark_profiler.time.enabled=true
  diagnostics.benchmark_profiler.speed.enabled=false
  diagnostics.benchmark_profiler.system.enabled=false
  diagnostics.benchmark_profiler.model_summary.enabled=false
  diagnostics.benchmark_profiler.snapshot.enabled=false
  "system.output.profiler=$OUT/torch"
)
```

This requests 10 profiler warmup steps and 3 recorded steps within a short training
run. Warmup activity is discarded; it is not included in the exported active window.
Ensure other stopping limits, dataset length and resume settings allow the schedule
to finish. Increase warmup/run length if dataset caches or `torch.compile` have not
settled. This is a training-only first pass; profile validation/checkpoint phases
separately when those are the question. Existing checkpoint/plot callbacks are not
disabled by this example.

Inspect the composed configuration without starting training:

```bash
anemoi-training profile "${PROFILE_ARGS[@]}" --cfg job > "$OUT/profile-config.yaml"
```

The keys above are checked against upstream Anemoi source, not your installed
benchmark. If Hydra rejects a key, check your version's config/schema rather than
adding `+` to silently create an unused field. Older versions with a separate
`diagnostics.profiler` flag should set it to `false` when using the `profile` command.
Current upstream exports `*.pt.trace.json` with `extra_plots=false`; that option
controls extra memory visualizations. Some older releases differ.

## 3. Collect PyTorch and I/O together

For a working direct Anemoi launch:

```bash
"$RPROF" record --collect python,io --python-backend ranges \
  --output-dir "$OUT/native" -- \
  anemoi-training profile "${PROFILE_ARGS[@]}"
```

For your existing **Slurm launch that starts one training process per task**, put
rprof after `srun`, preserving the allocation, task/GPU binding and usual `srun`
options from the working benchmark:

```bash
srun "$RPROF" record --collect python,io --python-backend ranges \
  --output-dir "$OUT/native" -- \
  anemoi-training profile "${PROFILE_ARGS[@]}"
```

Define the common `OUT` and session UUID once in the job script before `srun`.
The executable/library and outputs must be accessible from each rank. In a container,
run rprof **inside the same container/environment** as Anemoi. For a different
launcher, place rprof around the rank-local training executable, not around the
remote launcher. Do not combine independent launchers that each spawn all ranks.

What this produces:

| Source | Output and timing |
| --- | --- |
| PyTorch/Kineto | CPU operators, available GPU kernels/copies, annotations and framework memory from the 3-step window; Anemoi writes Chrome JSON under `$OUT/torch`, possibly in a run-ID subdirectory |
| Native I/O | Intercepted POSIX file operations/bytes/latencies throughout the supervised run, including startup and trace export; one unique capture per rprof invocation under `$OUT/native` |
| Native Python `ranges` | Explicit `rprof_python.range(...)` annotations and interpreter diagnostics; **no automatic function tracing**, so an unmodified Anemoi run may have no Python range spans |

If you only want PyTorch operators plus I/O, use `--collect io` and omit
`--python-backend ranges`. PyTorch collection still happens through Anemoi.

For explicit Python function/file/line attribution, use a separate short run on
supported CPython 3.12–3.14 with `--python-backend monitoring` and selected source
roots, e.g. `--python-include /path/to/anemoi-core/training/src`. Repeat the flag for
other roots. This can add substantial overhead. Avoid `profile`/automatic fallback
with the stack-recording PyTorch profiler; use ranges-only on older interpreters.

For PyTorch-only collection, run `anemoi-training profile "${PROFILE_ARGS[@]}"`
under your usual launcher and skip rprof recording entirely.

## 4. Import one PyTorch trace and open both reports

Find the actual runtime trace, not Anemoi's summary JSON, memory snapshot pickle or
memory-timeline JSON:

```bash
find "$OUT/torch" -type f \( -name '*.pt.trace.json' -o -name '*.pt.trace.json.gz' \)
find "$OUT/native" -type f -name '*.rprof.jsonl'
```

Select one rank/window and its native capture from the listing:

```bash
TRACE=/absolute/path/to/hostname_pid.timestamp.pt.trace.json
NATIVE=/absolute/path/to/session.shard.rprof.jsonl

"$RPROF" import-pytorch "$TRACE" --session "$RPROF_SESSION_ID" \
  -o "$OUT/reports/torch-rank0.rprof.jsonl"
"$RPROF" report "$OUT/reports/torch-rank0.rprof.jsonl" \
  --format html -o "$OUT/reports/torch-rank0.html"
"$RPROF" report "$NATIVE" --format html -o "$OUT/reports/native-rank0.html"
"$RPROF" view "$OUT/reports/torch-rank0.rprof.jsonl" --open
```

`rank0` is an example output label: select the actual rank using source metadata or
the run logs; file ordering does not establish rank. If your trace is gzipped,
decompress it to a new `.json` file first (`gzip -dc input.json.gz > trace.json`).
Outputs are no-clobber; use new names when repeating imports/reports.

Native Python and I/O share a monotonic clock. **Imported PyTorch traces have their
own source clock domains**, including separate domains for separate rank files.
You can import several files with the same `--session` and merge them with native
captures for one report, but merging does not align these clocks. Initially inspect
the native and PyTorch reports separately; do not infer exact I/O-to-GPU timing
across them. NCCL/Gloo activity is not MPI; `--collect mpi` does not profile NCCL.

For remote viewing, copy the HTML files to your workstation, or serve on the remote
host with `rprof view CAPTURE --port 8080` and tunnel from your workstation:

```bash
ssh -N -L 8080:127.0.0.1:8080 your-training-host
```

Open `http://127.0.0.1:8080` locally. No remote job-control service is needed.

## 5. Check the first result before increasing the run

- Confirm the training result/loss is sensible and compare timing to an unprofiled
  run with the same input and placement. Profiling changes throughput; do not use
  the profiled run as the benchmark baseline.
- In the PyTorch report, check CPU operators and actual GPU execution both appear
  when expected. CPU CUDA API calls alone do not prove GPU activity was captured.
  Inspect attention/GEMM, backward, optimizer, H2D and available NCCL activity, and
  gaps between batches. DataLoader worker internals need their own observations.
- In the I/O panel, look at dataset paths, bytes, errors and latency. These are
  observed POSIX API metrics, not physical storage throughput. Memory-mapped reads,
  direct syscalls and object-store/network paths may not be covered; see
  [I/O scope](io-profiling.md). Trace-export writes are also recorded.
- Check collector losses, incomplete intervals, summary omissions and timeline
  retention. Native collection is not restricted to PyTorch's active window.
- Current PyTorch import limits are **256 MiB uncompressed JSON and one million
  source records per file**. Start with 1–3 active steps; reduce the window if too
  large. Do not truncate JSON to fit. The default HTML timeline retains the first
  200,000 observations or 64 MiB; see [capacity](capacity.md).
- Start with all ranks in a small working job. `trace_rank0_only=true` in the checked
  Anemoi implementation reads `SLURM_PROCID`; it is not a reliable global-rank filter
  under every launcher, and rank zero alone may miss sharded-model behavior.

## Version and validation notes

Anemoi itself is not installed in this repository's local environment, so this guide
is not an end-to-end validation of your benchmark. The combined Python-ranges/I/O/
PyTorch path was smoke-tested locally with the repository's CPU training fixture:
13,521/13,521 I/O records, 14 Python packets, zero native/writer drops, and a PyTorch
trace exported during that same run. GPU framework-owned traces were separately
validated on H200; see [PyTorch validation](pytorch-import.md).

Anemoi configuration/export behavior was checked on 2026-11-02 against:

- [Anemoi benchmarking documentation](https://anemoi.readthedocs.io/projects/training/en/latest/user-guide/benchmarking.html)
- [BenchmarkProfiler source, commit 33288689707ee851001490e1a654de466f70bec8](https://github.com/ecmwf/anemoi-core/blob/33288689707ee851001490e1a654de466f70bec8/training/src/anemoi/training/diagnostics/profilers.py)
- [Benchmark profiler configuration at that commit](https://github.com/ecmwf/anemoi-core/blob/33288689707ee851001490e1a654de466f70bec8/training/src/anemoi/training/config/diagnostics/benchmark_profiler/detailed.yaml)

Further reference: [Python modes](python-profiling.md), [PyTorch import](pytorch-import.md),
[remote operation](remote-operation.md).
