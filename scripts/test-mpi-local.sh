#!/bin/bash
# CPU-only local MPI validation, using an installed Open MPI toolchain.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
make build PROFILE=release
make mpi PROFILE=release
temporary=$(mktemp -d "${TMPDIR:-/tmp}/rprof-mpi-test.XXXXXX")
trap 'rm -rf "$temporary"' EXIT
export OMPI_ALLOW_RUN_AS_ROOT=1 OMPI_ALLOW_RUN_AS_ROOT_CONFIRM=1
for entry in 'two_ranks basic' 'request_matrix matrix' 'threads threads'; do
    read -r fixture mode <<< "$entry"
    mpicc -O2 -pthread -Wall -Wextra -Werror "crates/rprof-mpi/tests/fixtures/$fixture.c" -o "$temporary/workload"
    export RPROF_SESSION_ID=$(cat /proc/sys/kernel/random/uuid)
    timeout 120 mpirun --oversubscribe --bind-to none -n 2 -x RPROF_SESSION_ID \
        "$root/target/release/rprof" record --collect mpi --mpi-clock-sync \
        --output-dir "$temporary/$mode" -- "$temporary/workload"
    target/release/rprof merge -o "$temporary/$mode.jsonl" "$temporary/$mode/"*.rprof.jsonl
    target/release/rprof report "$temporary/$mode.jsonl" --format html -o "$temporary/$mode.html"
    node scripts/validate-mpi-report.mjs "$temporary/$mode.html" "$mode"
done
