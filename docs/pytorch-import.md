# PyTorch/Kineto import and scheduled collection

## Import existing framework traces

```sh
rprof import-pytorch torch-trace.json -o torch.rprof.jsonl
rprof report torch.rprof.jsonl --format html -o torch.html
rprof report torch.rprof.jsonl --format trace -o torch.perfetto.json
```

The Rust importer needs no Python, PyTorch, or GPU. It snapshots and hashes input,
streams two parsing passes, and publishes output atomically without overwriting
existing files. Inputs may be a Chrome event array or an object with `traceEvents`.
Malformed JSON does not publish a successful artifact.

Complete `X` events, nested `B/E` events, counters, metadata, instants, and explicit
`s/t/f` flow chains are supported. Open slices remain incomplete; unknown record
types, malformed durations, ambiguous links, and index limits are diagnosed.
Host CUDA runtime/driver API calls remain CPU intervals even when their names contain
`memcpy` or `kernel`. Actual kernels/copies/memsets and GPU annotations have distinct
roles on device/stream tracks. GPU ranges are elapsed annotations, not kernel time.
Source PID/TID/category and available device/context/stream, argument, shape/type,
stack, external/correlation IDs are retained. Numeric GPU lane coordinates can
identify device/stream when args omit them; context remains unknown if absent.

Chrome timestamps/durations are **microseconds**, regardless of `displayTimeUnit`.
Raw decimal/exponent tokens are converted to integer nanoseconds without rounding
absolute timestamps through floating point. Sub-nanosecond rounding is counted.
Non-temporal metadata timestamps do not shift the origin. Every imported file gets
a distinct source clock domain, rebased to its first activity. `baseTimeNanoseconds`
is retained as source metadata; it is not treated as a native monotonic mapping.
Merging a native capture with an import does not silently align their clocks.

Provided flow IDs are preserved in scoped namespaces. Legacy Kineto `ac2g` flow IDs
cross CPU process and GPU device PID namespaces; explicit `id2.local` stays local.
Endpoints resolve to unique innermost source-lane intervals. Raw correlation-ID
coincidence is not used to invent links. Source-unpaired or ambiguous endpoints
remain unresolved. Declared distributed rank/backend metadata is retained; Gloo
and NCCL are not relabeled MPI.

## Summary interpretation

Imported trace provenance is visibly distinct from native measurements. Conversion
completion is not proof of loss-free source profiling: source CUPTI/Kineto losses
and export coverage remain **unknown** unless the source supplies evidence.
Source errors/warnings and import omissions stay visible.

Activity tables count raw imported observations and inclusive durations. PyTorch's
`key_averages()` may collapse a sole nested child with the same name as its parent;
the importer preserves both original records. Reference validation accounts for
that explicit normalization rather than discarding trace evidence.

CPU contained-self time subtracts contained CPU children (including API/annotation/
Python layers) per source lane. Crossing/tied intervals produce unknown self totals
for affected groups. GPU execution/ranges have no CPU self time. The bounded
self-time index can also make self totals unavailable. No inclusive sum is GPU
utilization or application elapsed time.

`[memory]` instants retain signed allocation deltas and produce allocated/reserved
byte counters only when the corresponding totals are present. Peaks are **peak
observed framework allocator totals within the trace window**, not physical GPU
memory use or whole-application peaks. Missing byte fields are unknown, not zero.
Generic source counters and marker samples retain their own meanings.

Limits: 256 MiB input, one million records, 65,000 source lanes/open slice entries,
200,000 indexed spans/flow endpoints, bounded overlap searches, and bounded text/
metadata. Summary groups are bounded; CPU self-time retains at most 200,000 intervals.
Large-array UI tables paginate. Shapes/stacks/work metadata cannot be recovered if
the original profiler did not export them.

## Opt-in scheduled collection

```python
from rprof_python import torch_profile

with torch_profile("traces", activities=("cpu", "cuda"),
                   wait=0, warmup=1, active=2, repeat=1,
                   record_shapes=True, profile_memory=True) as profiler:
    for batch in batches:
        train_step(batch)
        profiler.step()

print(profiler.rprof_trace_files)
```

The optional package is supplied by `rprof record --collect python`, or installed
with `python -m pip install ./python`. Torch is imported lazily. Each active window
exports a uniquely named Chrome JSON trace atomically. `step()` defines schedule
boundaries; warmup is not an active trace window. Shapes, stacks, memory and FLOP
estimates are opt-in. The helper records its schedule, versions, framework-owned
mode, and available distributed identity through Torch metadata APIs.

Framework-owned collection requires `CUDA_INJECTION64_PATH` to be unset: Kineto
and rprof's native CUDA collector cannot own CUPTI concurrently. For the native
path, omit the Torch profiling context and run `--collect python,cuda`; this gives
Python/native API/GPU timelines on the shared monotonic clock. For operator-level
attribution, use the framework-owned trace and importer. Combined native Python
instrumentation and the helper require monitoring or ranges-only mode; the profile
fallback is rejected to avoid profile-hook ownership conflicts. Imported framework
traces remain separate clocks from any simultaneous native capture.

## Recorded validation

CPython 3.12.9 / PyTorch 2.13.0+cu130 were tested locally and on `tst9` H200
(`h200SHTFGX`). The temporary GPU allocation was released.

| Fixture | Compared CPU operator groups | GPU kernels | Resolved flows |
| --- | ---: | ---: | ---: |
| CPU eager/autograd | 26 | 0 | 8 |
| H200 eager + DataLoader spawn workers | 25 | 12 | 24 |
| H200 compiled training | 16 | 10 | 16 |
| Native Python + framework-owned H200 profile | 25 | 12 | 24 |
| Two-rank, single-host Gloo | 5 per rank | 0 | 0 |

Raw source counts/durations matched imports; CPU reference counts/times matched
`key_averages()` after its duplicate-name normalization. Available self totals
matched within timestamp-rounding tolerance. Every H200 kernel/copy had its supplied
host API correlation. Seven original unpaired flow endpoints per H200 fixture
remain explicit; no endpoints were invented for them. Compiled-region names and
fused kernel identities were retained, without assuming one operator per kernel.

The separate native Python+CUPTI path recorded **4,841/4,841 CUDA records**, zero
native/source drops, and successful finalization; Python recorded 40 packets with
no omissions. The DataLoader combined run recorded four interpreter sources and
58 Python packets with zero drops. Worker/helper finalization coverage remains
visible per source. CPU-side tests also passed on `tst3` HBv4.

Reproduce with `python/tests/torch_workload.py`, `python/tests/torch_distributed.py`,
and `scripts/check-pytorch.py`. The checker reads reference exports, verifies raw
event preservation and normalized CPU summaries, and requires real CPU→GPU execution
links. Saved local artifacts are under `reports/python/` (ignored by Git).
Broader Python/Torch versions, multi-node DDP/FSDP/NCCL, and automatic native/import
clock alignment remain follow-ups; this matrix does not claim their validation.
