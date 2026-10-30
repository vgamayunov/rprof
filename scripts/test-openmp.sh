#!/bin/bash
# Optional runtime integration tests. Clang/libomp are the default; set
# RPROF_TEST_OMP_{CC,INCLUDE,LIBDIR,RUNTIME,COMPILER_LD} for another installation.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
make ompt
export RPROF_TEST_OMPT_LIBRARY="$root/target/debug/librprof_ompt.so"
cargo test -p rprof-cli --test openmp -- --include-ignored
