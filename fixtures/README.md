# Report fixtures

`multitrack.jsonl` is a deterministic synthetic capture with MPI ranks, CPU/OpenMP
threads, GPU streams, I/O, integer/float counters, correlations, incomplete
activities, and two explicitly unaligned clock domains. It needs no specialized
hardware. Counter values and original monotonic clocks exceed JavaScript's exact
integer range to exercise lossless report conversion.

Regenerate into a new path and compare before replacing the checked-in fixture:

```sh
cargo run -p rprof-report --example multitrack -- regenerated.jsonl
cmp fixtures/multitrack.jsonl regenerated.jsonl
```

Pass `--single-domain` to generate a two-rank same-host fixture for a single
reference-viewer timeline. Source metadata labels these captures as synthetic.
