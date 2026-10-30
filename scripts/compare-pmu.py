#!/usr/bin/env python3
"""Independent perf-stat comparison, including measured multiplexing coverage.

Each run uses fresh capture/reference files. Unsupported events are recorded
explicitly and never treated as measured zeros. No system settings are changed.
"""
import argparse
import csv
import json
import os
from pathlib import Path
import statistics
import subprocess


def canonical_name(name):
    name = name.split(":", 1)[0].strip()
    name = {"cpu-cycles": "cycles", "branch-instructions": "branches"}.get(name, name)
    if name.startswith("r") and 1 <= len(name[1:]) <= 16 and all(c in "0123456789abcdefABCDEF" for c in name[1:]):
        return f"r{int(name[1:], 16):x}"
    return name


def read_capture(path):
    readings, statuses = {}, {}
    for line in path.read_text().splitlines():
        record = json.loads(line)
        if record["record"] != "chunk":
            continue
        for event in record["data"]["events"]:
            attrs = {key: value["value"] for key, value in event.get("attributes", {}).items()}
            if "pmu.event" not in attrs:
                continue
            name = canonical_name(attrs["pmu.event"])
            statuses[name] = attrs.get("pmu.status")
            if attrs.get("pmu.final") and attrs.get("pmu.scaled_estimate") is not None:
                readings[name] = {
                    "raw": event["data"]["value"]["value"],
                    "scaled": int(attrs["pmu.scaled_estimate"]),
                    "enabled_ns": attrs["pmu.time_enabled_ns"],
                    "running_ns": attrs["pmu.time_running_ns"],
                }
    return readings, statuses


def read_reference(path):
    refs = {}
    for fields in csv.reader(path.read_text().splitlines(), delimiter=";"):
        if len(fields) < 5 or fields[0].startswith("#"):
            continue
        name = canonical_name(fields[2])
        status = fields[0].strip()
        if status.startswith("<"):
            refs[name] = {"scaled": None, "status": status}
        else:
            unit = fields[1].strip()
            refs[name] = {
                "scaled": float(status) * {"msec": 1e6, "usec": 1e3, "sec": 1e9}.get(unit, 1),
                "runtime_ns": int(fields[3]),
                "running_percent": float(fields[4]),
            }
    return refs


def compare(args):
    if not 1 <= args.runs <= 20:
        raise ValueError("--runs must be between 1 and 20")
    if ":" in args.events:
        raise ValueError("--events accepts names without modifiers; use --kernel to include kernel execution")
    args.output_dir.mkdir(parents=True, exist_ok=False)
    names = [canonical_name(name) for name in args.events.split(",")]
    environment = dict(os.environ, LC_ALL="C")
    measured = {name: {"rprof": [], "perf": []} for name in names}
    readings = {name: {"rprof": [], "perf": []} for name in names}
    unavailable = {name: [] for name in names}
    multiplexed = []
    for index in range(args.runs):
        capture = args.output_dir / f"rprof-{index}.jsonl"
        reference = args.output_dir / f"perf-{index}.csv"
        workload = [str(args.workload.resolve()), *args.workload_arg]
        command = [str(args.rprof.resolve()), "record", "--collect", "pmu", "--pmu-events",
                   ",".join(names), "--pmu-scope", args.scope, "-o", str(capture)]
        if args.kernel:
            command.append("--pmu-kernel")
        subprocess.run([*command, "--", *workload], check=True, env=environment)
        command = ["perf", "stat", "--no-big-num", "-x", ";", "-o", str(reference)]
        if args.scope == "root-thread":
            command.append("--no-inherit")
        modifier = ":uk" if args.kernel else ":u"
        subprocess.run([*command, "-e", ",".join(name + modifier for name in names),
                        "--", *workload], check=True, env=environment)
        counts, statuses = read_capture(capture)
        refs = read_reference(reference)
        for name in names:
            if name not in statuses or name not in refs:
                raise ValueError(f"missing diagnostics for {name}: rprof={statuses.get(name)}, perf={refs.get(name)}")
            actual = counts.get(name, {}).get("scaled")
            expected = refs[name]["scaled"]
            if actual is None and expected is None:
                unavailable[name].append({"rprof": statuses[name], "perf": refs[name]["status"]})
                continue
            if actual is None or expected is None:
                raise ValueError(f"availability mismatch {name}: rprof={actual} ({statuses[name]}), perf={expected}")
            measured[name]["rprof"].append(actual)
            measured[name]["perf"].append(expected)
            readings[name]["rprof"].append(counts[name])
            readings[name]["perf"].append(refs[name])
        # Less than 98% avoids calling tiny scheduling/read skew multiplexing.
        multiplexed.append({
            "rprof": any(r["running_ns"] * 100 < r["enabled_ns"] * 98 for r in counts.values()),
            "perf": any(r.get("running_percent", 100) < 98 for r in refs.values() if r["scaled"] is not None),
        })

    summary = {"runs": args.runs, "scope": args.scope,
               "privilege_requested": "user+kernel" if args.kernel else "user",
               "workload_args": args.workload_arg, "multiplexing_by_run": multiplexed, "events": {}}
    failures = []
    for name in names:
        values = measured[name]
        if not values["rprof"]:
            summary["events"][name] = {"status": "unavailable", "diagnostics": unavailable[name]}
            continue
        if len(values["rprof"]) != args.runs:
            failures.append(name + " intermittent availability")
        actual, expected = (statistics.median(values[key]) for key in ("rprof", "perf"))
        relative = abs(actual - expected) / expected if expected else (0 if actual == 0 else float("inf"))
        tolerance = 0.10 if name in ("instructions", "minor-faults", "page-faults") else 0.50 if name == "cache-misses" else 0.20
        summary["events"][name] = {"status": "measured", "rprof_median": actual, "perf_median": expected,
                                   "relative_difference": relative, "tolerance": tolerance,
                                   "runs": values, "readings": readings[name]}
        if relative > tolerance:
            failures.append(name)
    if not any(values["rprof"] for values in measured.values()):
        failures.append("no events measured")
    if args.require_multiplexing and not all(run["rprof"] and run["perf"] for run in multiplexed):
        failures.append("multiplexing was not observed in both tools on every run")
    summary["passed"] = not failures
    (args.output_dir / "comparison.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))
    if failures:
        raise SystemExit("comparison failed: " + ", ".join(failures))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rprof", type=Path, required=True)
    parser.add_argument("--workload", type=Path, required=True)
    parser.add_argument("--workload-arg", action="append", default=[])
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--events", default="cycles,instructions,cache-misses,task-clock,minor-faults")
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--scope", choices=("workload-tree", "root-thread"), default="workload-tree")
    parser.add_argument("--kernel", action="store_true")
    parser.add_argument("--require-multiplexing", action="store_true")
    compare(parser.parse_args())


if __name__ == "__main__":
    main()
