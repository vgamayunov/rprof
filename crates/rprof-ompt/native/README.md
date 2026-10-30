# OMPT adapter

`omp-tools.h` is the unmodified generated public header from LLVM OpenMP 20.1.8
(`llvmorg-20.1.8`), distributed under Apache-2.0 WITH LLVM-exception. See
`LLVM-LICENSE.txt`. Its source is
https://github.com/llvm/llvm-project/tree/llvmorg-20.1.8/openmp.

The adapter uses the standard runtime lookup/registration interface and records
each registration result. It does not link to, replace, or select an OpenMP runtime.

Build the adapter into an OMPT shared tool and select it with `OMP_TOOL=enabled`
and `OMP_TOOL_LIBRARIES=/absolute/path/to/tool.so` (not `LD_PRELOAD`). The
profiler's native socket/counter descriptors must be present for initialization
to succeed. The adapter rejects a second OpenMP runtime in the same process
rather than overwriting the first runtime's callback state. Dispatch is omitted
because the 376-byte wire format has no dispatch event kind. Task-schedule
statuses, including detach and early/late fulfill, are emitted unchanged; the
decoder, not this adapter, decides how to pair or close task intervals.
