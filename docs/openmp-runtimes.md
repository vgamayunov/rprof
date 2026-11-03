# OpenMP runtime support

The supported-runtime goal includes **GCC libgomp, LLVM libomp, and Intel libiomp5**.
Runtime and compiler identities are separate: GCC-generated OpenMP code can use a
compatibility ABI in another runtime, but that does not constitute libgomp support.

| Runtime | Initial mechanism | Coverage strategy |
| --- | --- | --- |
| LLVM `libomp` | Standard OMPT tool | Validate callback availability and emitted events with Clang-generated host code; test supported version ranges |
| Intel `libiomp5` | Standard OMPT tool | Validate independently, including callback pairing/task scheduling; add Intel compiler (`icx`) coverage when available |
| GCC `libgomp` | Explicit `gomp` backend; target-fingerprinted GCC 13 GNU ABI adapter on Linux x86_64 | Actual GCC 13.2/libgomp fixtures and two-node 128-thread/rank hybrid pass; partial coverage is distinct from OMPT |

As checked on 2026-10-30, GCC's official
[OpenMP 5.0 implementation-status table](https://gcc.gnu.org/onlinedocs/libgomp/OpenMP-5_002e0.html)
lists the OMPT interface as **not implemented**. GCC 13.2/libgomp is installed on
the development host and tst4. Its lack of an OMPT handshake must be reported
explicitly rather than producing a misleading empty OpenMP profile.

## First OMPT implementation

Use the same tool through `OMP_TOOL_LIBRARIES` with LLVM and Intel. Record runtime
version, advertised OpenMP version, each `ompt_set_callback` result, and received
callback counts. Unsupported, conditionally supported, unobserved, and incomplete
data remain distinct. A runtime can report a callback as supported while a particular
compiler ABI path emits only part of its expected endpoints.

For example, the LLVM GNU-ABI bridge lacks a corresponding `GOMP_single_end`
entry point for a single-executor scope. Such a missing end must remain incomplete;
it must not be reconstructed as a completed interval from an unrelated barrier.

Validation targets include nested parallelism, static/dynamic work sharing,
sections/single, explicit tasks and taskgroups/taskwait, task dependencies,
synchronization waits, callback loss, and two-rank hybrid MPI+OpenMP execution.
GPU/offload callbacks are separate future coverage.

## GNU runtime backend

The implemented backend uses 72 reviewed entry points, transparent argument-preserving
dispatch, exact target-library SHA-256 verification, and the shared Rust transport.
Select `--openmp-backend gomp`; `--gomp-detail regions` is the coarse default.
Inclusive body/API observations are distinct from OMPT scheduler and pure-wait
callbacks. See [GNU profiling](gomp-profiling.md) for supported constructs, gaps,
build commands and validation results.

Expand the tested matrix beyond the initial GCC 13 target. Do not
replace libgomp with libomp/libiomp5 to claim GNU-runtime support, and do not probe
by silently rerunning a user's workload. Any automatic backend choice must be
determined before launch or use safe capability metadata from that single run.
