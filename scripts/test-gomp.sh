#!/bin/bash
# Actual GCC 13/libgomp integration; no substitute OpenMP runtime.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
export RPROF_TEST_GOMP_CC=${RPROF_TEST_GOMP_CC:-gcc}
export RPROF_TEST_GOMP_CXX=${RPROF_TEST_GOMP_CXX:-g++}
case "$($RPROF_TEST_GOMP_CC -dumpfullversion)" in
  13.*) ;;
  *) echo 'GNU validation requires GCC 13' >&2; exit 1 ;;
esac
make gomp GOMP_CC="$RPROF_TEST_GOMP_CC"
if readelf -d target/debug/librprof_gomp.so | grep NEEDED | grep -Eq 'lib(gomp|omp|iomp)'; then
  echo 'GNU adapter unexpectedly links an OpenMP runtime' >&2
  exit 1
fi
cargo test -p rprof-cli --test gomp -- --test-threads=1
