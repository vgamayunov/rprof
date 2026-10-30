# glibc and MPI binary compatibility

## Recommended deployment

**Build once on the target cluster's login/build host and run those artifacts on
compatible compute nodes.** A Rust toolchain is needed on the build host, not on
every node. Put the CLI and preload libraries on shared storage or copy the built
artifacts to the nodes. Build the PMPI adapter using the same `mpicc`/MPI module as
the application.

For a wider binary distribution, build the CLI and native libraries against the
**oldest supported glibc baseline**, using a container/sysroot matching that baseline.
A binary built against an older glibc generally runs on newer glibc; the reverse is
not guaranteed. Declare/test the supported baseline rather than assuming that any
Linux binary is portable.

The CLI can optionally be built as a **static-musl executable** to remove its glibc
dependency. This does not make a PMPI/preload library portable: native collectors
still run inside the application's dynamic runtime and need compatible glibc, MPI,
architecture, and vendor-library ABIs. Static-musl packaging is a distribution
option, not the target-native build used for the current validation.

## Check actual artifacts

```sh
getconf GNU_LIBC_VERSION
sh scripts/check-elf-abi.sh target/release/rprof target/release/librprof_mpi.so
```

The script uses `readelf`, without executing the inspected files. It reports ELF
architecture, the highest directly referenced `GLIBC_*` symbol version, and direct
shared-library dependencies. Also check the MPI/UCX/plugin dependency tree in the
target environment; direct symbol requirements alone are not sufficient.

The phase-4 build on `tst4` used Azure Linux 3.0, glibc **2.38**, Rust **1.90**, and
HPC-X **2.24.1 / Open MPI 4.1.9a1**. Inspection of that build showed:

| Artifact | Highest direct glibc symbol version |
| --- | --- |
| `rprof` | `GLIBC_2.34` |
| `librprof_mpi.so` | `GLIBC_2.33` |

Those are properties of that build, not permanent project-wide minimums. Both HBv4
validation nodes had glibc 2.38. The PMPI library also requires the matching
`libmpi.so.40`; a matching SONAME by itself is not a guarantee that unrelated MPI
builds can be substituted.

## Preserve the complete MPI runtime environment

```sh
module load mpi/hpcx
make build PROFILE=release
make mpi PROFILE=release MPICC=mpicc
```

Propagate the module's library paths to every rank (for Open MPI, explicitly use
`-x LD_LIBRARY_PATH` when launching through SSH as in the tested workflow). Keep
MPI, UCX, PMIx, and collective plugins from a consistent installation. The first
HBv4 test exposed a system UCX 1.19 / HPC-X UCX 1.20 mix; propagating the intended
paths resolved that mismatch. CPU-only testing also needed an explicit CPU/IB UCX
module set to avoid initializing CUDA/NVML plugins on HBv4.

The multithreaded fixture required HPC-X's `hpcx-mt` module, selecting `ucx/mt`.
This is independent of glibc: matching symbol versions alone does not establish
MPI_THREAD_MULTIPLE support.

Do not solve a glibc mismatch by copying a workstation's `libc.so` into the preload
path. The dynamic loader and core runtime libraries must remain compatible as a
set. Rebuild against the intended baseline or use an appropriate complete runtime
image instead.

Future release artifacts should record architecture, build glibc floor, Rust
toolchain, MPI implementation/version, compiler-wrapper command, and resolved
vendor dependencies. Native adapters should be packaged per supported MPI/runtime
combination rather than advertised as a universal `.so`.
