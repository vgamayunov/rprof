#!/usr/bin/env bash
# Run on an allocated CUDA 13 GPU host; this script does not allocate GPUs.
set -euo pipefail
cd "$(dirname "$0")/.."
export CUDA_HOME="${CUDA_HOME:-/usr/local/cuda}"
profile="${PROFILE:-debug}"
make build cuda PROFILE="$profile" CUDA_HOME="$CUDA_HOME"
target="$PWD/target/$profile"
"$CUDA_HOME/bin/nvcc" -O2 -lineinfo "${RPROF_CUDA_ARCH:--arch=sm_90}" \
  crates/rprof-cuda/tests/fixtures/multistream.cu -o "$target/cuda-multistream"
for fixture in conflicting_subscriber abrupt_exit; do
  binary=cuda-conflict
  if [[ "$fixture" == abrupt_exit ]]; then binary=cuda-abrupt; fi
  "${CXX:-c++}" -std=c++17 -Wall -Wextra -Werror \
    -I"$CUDA_HOME/targets/x86_64-linux/include" \
    "crates/rprof-cuda/tests/fixtures/$fixture.cpp" \
    -L"$CUDA_HOME/targets/x86_64-linux/lib" \
    -Wl,-rpath,"$CUDA_HOME/targets/x86_64-linux/lib" -lcupti -lcuda \
    -o "$target/$binary"
done
export RPROF_TEST_CUDA_DIR="$target"
flags=()
if [[ "$profile" == release ]]; then flags+=(--release); fi
cargo test -p rprof-cli "${flags[@]}" --test cuda -- --include-ignored --test-threads=1
