#!/usr/bin/env bash
# CPU-only Linux counting validation; perf permissions are never altered.
set -euo pipefail
cd "$(dirname "$0")/.."
python3 -B -m unittest discover -s scripts/tests -p 'test_*.py'
cargo test -p rprof-perf -p rprof-cli --lib --bins
cargo test -p rprof-cli --test pmu -- --test-threads=1
cargo build -p rprof-cli --release
cc -O2 -Wall -Wextra -Werror -pthread crates/rprof-cli/tests/fixtures/pmu_workload.c -o target/release/pmu-workload
python3 scripts/compare-pmu.py --rprof target/release/rprof \
  --workload target/release/pmu-workload --output-dir "${1:?supply a fresh output directory}"
