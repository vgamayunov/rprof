# CPython collection

```sh
cargo build -p rprof-cli
target/debug/rprof record --collect python -o python.rprof.jsonl -- python3 application.py
target/debug/rprof report python.rprof.jsonl --format html -o python.html
```

The CLI embeds a small optional Python package and launch shim in a private
temporary directory. `PYTHONPATH` loads the shim through normal `sitecustomize`.
An existing `sitecustomize` runs first. No Python installation is modified, and
the Rust CLI/import/report build has no Python or PyTorch SDK dependency.
Isolated/site-disabled launches (`-I`, `-E`, `-S`), embedded interpreters that bypass
site initialization, and cleaned child environments can bypass the shim. Missing
initialization is a collector failure with the application's exit status preserved.

## Modes and attribution

- `--python-backend auto`: `sys.monitoring` on CPython 3.12+, otherwise the profile
  hook fallback. API support is gated to GIL-enabled CPython 3.10–3.14.
- `--python-backend monitoring`: instrument start/resume/return/yield/unwind events.
  An unused monitoring slot is claimed; other tools' slots are not replaced.
- `--python-backend profile`: `sys.setprofile` and `threading.setprofile` for the
  main and subsequently created Python threads. Existing profile hooks are rejected
  rather than displaced. C-call events are not collected.
- `--python-backend ranges`: explicit user ranges only, without function hooks.

Default function inclusion is the launch working directory. Repeat
`--python-include PATH` for other source roots. The tool's own package is excluded.
`--python-inline` enables `<string>`/`<stdin>` code; direct `python -c`/`python -`
launches enable it automatically. Other pseudo-files and library code outside
selected roots are excluded. Imports/loader work can therefore appear in a selected
parent's wall time without separate child intervals.

Function intervals are **instrumented execution segments**, not sampled stacks,
CPU utilization, or coroutine lifetimes. Yield ends a segment; resume starts a
new one. No suspended interval is invented, and segments are not linked to a
guessed logical coroutine/task identity. File, qualified function name, line,
execution kind, thread, interpreter, and parent segment are retained. User arguments,
return values, and locals are not captured, and frames are not retained.

Inclusive duration includes instrumentation, native calls, GIL waits, and OS
scheduling. “Self” subtracts only recorded included-child segments on that thread;
it is not exclusive CPU execution. Totals can overlap across threads/layers.

The transport uses the same process-image hash, Linux TID, and `CLOCK_MONOTONIC`
domain as native collectors. `--collect python,io` and `python,cuda` share physical
CPU thread tracks; GPU execution stays on native CUDA device/stream tracks.
Interpreter/source and thread-lifetime identifiers remain in attributes. OS TIDs
can be reused, so a track's numeric TID is not a permanent Python thread-object ID.

## User ranges and package installation

```python
import rprof_python as rprof

with rprof.range("training step"):
    train_step()
```

The package is available automatically to an instrumented interpreter and its local
children. It can also be installed explicitly with `python -m pip install ./python`;
installation does not install a global `sitecustomize` hook. Without an active
capture, ranges are no-ops. For annotation-only collection:

```sh
rprof record --collect python --python-backend ranges -o ranges.jsonl -- python3 app.py
```

Ranges represent elapsed lifetimes and may include suspension/native work. Nested
range identity uses Python contexts. Cross-thread or changed-context completion
is diagnosed; the profiler does not fabricate an executing thread or suppress an
application exception. Stopping manually with `rprof_python.stop()` ends this
interpreter's observation window; unfinished intervals remain visibly incomplete.

## Workers, bounds, and failure handling

A private Unix `SOCK_SEQPACKET` endpoint permits spawn/forkserver children to
connect even when process launch closes inherited descriptors. Callbacks send
bounded JSON packets nonblocking. Fork resets image/source identity, stack state,
and transport; already-active parent frames are not reused as child intervals.
Multiprocessing finalizers checkpoint supported workers. Helper/daemon processes
and `_exit`/fatal termination can lack finalization and retain lower-bound counts.

Limits include 8 KiB packets, 128 selected frames/context-range depth, 65,536 code
descriptors/open ranges, 1,024 active thread states per interpreter, 4,096 live
connections/sources, and 65,536 received thread-lifetime streams. The Rust raw
queue uses `--native-buffer-events` (Python maximum 32,768). Names/paths are bounded
with truncation flags. Source omissions, send failures, receiver drops, malformed
packets, unmatched endpoints, abandoned frames, and in-flight callbacks are separate
diagnostics. Unknown source tails are not claimed loss-free. Callback implementation
failures stop instrumentation and are reported without replacing application results.

Run one profiler per MPI/torchrun rank for rank-local captures. A socket and bootstrap
directory on one node are not a remote collection service. Nested Python captures,
free-threaded execution, arbitrary profiler-hook replacement, embedded interpreters,
and subinterpreter lifecycle coverage are not validated. Actual interpreter IDs are
recorded when available; absence is explicitly unknown.

## Validation and overhead

The six integration tests cover generator/coroutine suspension, threads, ranges-only
mode, fork/spawn/forkserver, shared Python/I/O track identity, preserved hooks and
site customization, no initialization, exit code 23, and bounded recursion omissions.
Function, worker and native-I/O checks passed on CPython 3.12.9 locally and on
`tst3` HBv4; ranges-only mode is additionally checked locally. CI covers 3.10–3.14.
Other matrix versions should be treated as pending until their CI jobs run.

The deliberately tiny-function probe (`python/tests/overhead.py`, 5,000 calls,
three local runs, body timing only) measured medians:

| Mode | Body time |
| --- | ---: |
| Uninstrumented | 0.574 ms |
| Monitoring | 777.715 ms |
| Profile fallback | 696.644 ms |
| Ranges-only, no range inside the loop | 1.134 ms |

This is **very high perturbation for tiny functions**, not a low-overhead sampler.
Use selected coarse functions, explicit ranges, or the framework-owned operator
profiler for performance measurements. The ranges-only probe measures disabled
function tracing, not per-range cost. Startup/import and native collector overhead
are additional. Lower-overhead native callbacks and statistical sampling remain
future work; no general overhead bound is claimed.
