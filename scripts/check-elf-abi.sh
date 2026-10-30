#!/bin/sh
# Inspect artifacts without executing them. Direct symbol requirements do not
# include dynamically loaded MPI/UCX plugins or every transitive dependency.
set -eu
if [ "$#" -eq 0 ]; then
    printf 'Usage: sh scripts/check-elf-abi.sh ELF_FILE...\n' >&2
    exit 2
fi
export LC_ALL=C
for artifact in "$@"; do
    readelf -h -- "$artifact" >/dev/null
    printf '\nArtifact: %s\n' "$artifact"
    readelf -h -- "$artifact" | grep -E 'Class:|Machine:'
    required=$(readelf --version-info -- "$artifact" | grep -oE 'GLIBC_[0-9]+(\.[0-9]+)*' | sort -Vu | tail -n 1)
    printf 'Highest direct glibc symbol requirement: %s\n' "${required:-none recorded (possibly static)}"
    readelf -d -- "$artifact" | grep NEEDED || true
done
