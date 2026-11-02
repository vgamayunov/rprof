#!/usr/bin/env python3
"""Offline, measured LCG throughput-ratio calibration and held-out validation.

Collect separately on each host; copy capture directories to one machine before compare.
All rates are workload-specific effective rates, not universal CPU specifications.
"""

import argparse
import datetime
import hashlib
import json
import os
import platform
import statistics
import subprocess
import sys
from pathlib import Path

TOLERANCE = 0.15  # Predeclared; never fitted or relaxed after observing held-out runs.
PLAN = [("warmup", 120_000_000, 1), ("calibration", 120_000_000, 3),
        ("heldout", 300_000_000, 3)]
METRIC = "lcg_iterations_per_second"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def dump_new(path, value):
    with path.open("x", encoding="utf-8") as output:
        json.dump(value, output, indent=2, sort_keys=True)
        output.write("\n")


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run(command, **kwargs):
    return subprocess.run([str(x) for x in command], check=True, text=True, **kwargs)


def observation(path):
    """Read stream-v2 frames, not a report/SDK summary or fixture stdout."""
    header = shard = end = None
    spans = []
    with path.open(encoding="utf-8") as capture:
        for line in capture:
            frame = json.loads(line)
            record, data = frame["record"], frame["data"]
            if record == "header":
                require(header is None, "duplicate header")
                header = data
            elif record == "shard":
                require(shard is None, "duplicate shard")
                shard = data
            elif record == "end":
                require(end is None, "duplicate footer")
                end = data
            elif record == "chunk":
                for event in data["events"]:
                    if event.get("kind") == "span" and event["data"].get("name") == "lcg.work":
                        spans.append(event)
    require(header is not None and header["format"] == "rprof-stream" and
            header["schema_version"] == 2, f"not a stream-v2 capture: {path}")
    require(shard is not None and end is not None and end["shard_id"] == shard["shard_id"] and
            end["exit_code"] == 0 and end["reason"] == "completed" and
            sum(end["loss"].values()) == 0, f"incomplete/lossy capture: {path}")
    require(len(spans) == 1, f"expected one lcg.work span in {path}")
    event = spans[0]
    span = event["data"]
    require(span["track_id"] == 0 and span["category"] == "process", "unexpected work span")
    attributes = event["attributes"]

    def attribute(name, kind):
        value = attributes[name]
        require(value["type"] == kind, f"unexpected {name} attribute type")
        return value["value"]

    iterations = attribute("benchmark.iterations", "unsigned")
    work_ns = span["duration_ns"]
    require(type(iterations) is int and 1 <= iterations <= 2_000_000_000 and
            type(work_ns) is int and work_ns > 0 and span["start_ns"] >= 0 and
            span["start_ns"] + work_ns == event["timestamp_ns"] and
            event["timestamp_ns"] <= end["duration_ns"], "invalid work timing or count")
    require(attribute("benchmark.work_unit", "text") == "lcg iterations", "unexpected work unit")
    require([c["name"] for c in shard["collectors"]] == ["benchmark-annotation"],
            "capture is not a measured benchmark annotation")
    return {"iterations": iterations, "work_ns": work_ns,
            "algorithm": attribute("benchmark.algorithm", "text"),
            "checksum": attribute("benchmark.checksum", "unsigned"),
            "hostname": shard["hostname"], "boot_id": shard["boot_id"]}


def collect(args):
    fixture = args.fixture.resolve(strict=True)
    require(fixture.is_file(), "fixture is not a file")
    args.output_dir.mkdir()  # A new directory; never modify existing captures.
    now = datetime.datetime.now(datetime.timezone.utc).isoformat()
    try:
        rustc = run(["rustc", "--version"], capture_output=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        rustc = None
    manifest = {"version": 1, "kind": "measured", "date": now,
                "tolerance": TOLERANCE, "plan": [{"split": s, "iterations": n, "count": count}
                                            for s, n, count in PLAN],
                "fixture_sha256": sha256(fixture), "hostname": platform.node(),
                 "kernel": platform.release(), "rustc": rustc, "runs": []}
    cpuinfo = Path("/proc/cpuinfo").read_text(encoding="utf-8")
    manifest["cpu_model"] = next((line.split(":", 1)[1].strip() for line in cpuinfo.splitlines()
                                  if line.startswith("model name")), platform.machine())
    manifest["affinity"] = sorted(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else None
    # Persist the split and tolerance before the first measurement.
    dump_new(args.output_dir / "plan.json", {k: v for k, v in manifest.items() if k != "runs"})
    checksums = {}
    algorithm = None
    for split, iterations, count in PLAN:
        for index in range(1, count + 1):
            filename = f"{split}-{index}.rprof.jsonl"
            path = args.output_dir / filename
            run([fixture, "--iterations", iterations, "--output", path])
            item = observation(path)
            require(item["iterations"] == iterations and item["hostname"] == manifest["hostname"],
                    "wrong work count or host in capture")
            require(algorithm is None or item["algorithm"] == algorithm, "algorithm changed")
            require(iterations not in checksums or item["checksum"] == checksums[iterations],
                    "non-deterministic checksum for same iteration count")
            algorithm = item["algorithm"]
            checksums[iterations] = item["checksum"]
            manifest["runs"].append({"split": split, "capture": filename,
                                      "sha256": sha256(path), **item})
    manifest["algorithm"] = algorithm
    dump_new(args.output_dir / "manifest.json", manifest)
    print(f"Measured captures: {args.output_dir / 'manifest.json'}")


def load_manifest(directory):
    manifest = json.loads((directory / "manifest.json").read_text(encoding="utf-8"))
    plan = json.loads((directory / "plan.json").read_text(encoding="utf-8"))
    require(all(manifest.get(key) == value for key, value in plan.items()),
            "collection metadata differs from predeclared plan")
    require(manifest["version"] == 1 and manifest["kind"] == "measured" and
            manifest["tolerance"] == plan["tolerance"] == TOLERANCE and
            manifest["plan"] == plan["plan"] ==
            [{"split": s, "iterations": n, "count": count} for s, n, count in PLAN] and
            manifest["fixture_sha256"] == plan["fixture_sha256"] and
            manifest["hostname"] == plan["hostname"], "manifest differs from predeclared plan")
    require(len(manifest["runs"]) == 7, "missing measured captures")
    expected = [(s, n, f"{s}-{i}.rprof.jsonl") for s, n, count in PLAN
                for i in range(1, count + 1)]
    for entry, (split, iterations, filename) in zip(manifest["runs"], expected):
        require((entry["split"], entry["iterations"], entry["capture"]) ==
                (split, iterations, filename), "capture split/work count changed")
        path = directory / filename
        require(path.is_file() and sha256(path) == entry["sha256"], f"capture hash mismatch: {path}")
        # Held-out duration is intentionally not read into the fitting path.
        if split != "heldout":
            require(all(entry[key] == value for key, value in observation(path).items()),
                    f"manifest observation mismatch: {path}")
    require(len({x["algorithm"] for x in manifest["runs"]}) == 1 and
            manifest["algorithm"] == manifest["runs"][0]["algorithm"], "algorithm changed")
    for n in (120_000_000, 300_000_000):
        require(len({x["checksum"] for x in manifest["runs"] if x["iterations"] == n}) == 1,
                "checksum mismatch for same work size")
    return manifest


def provenance(kind, source, date, conditions):
    return {"kind": kind, "source": source, "date": date, "conditions": conditions}


def profile(manifest, identifier):
    calibration = [r for r in manifest["runs"] if r["split"] == "calibration"]
    rates = sorted(r["iterations"] * 1e9 / r["work_ns"] for r in calibration)
    hashes = [r["sha256"] for r in calibration]
    conditions = (f"LCG-specific effective single-process rate, not universal CPU throughput; "
                  f"hostname={manifest['hostname']}; kernel={manifest['kernel']}; "
                   f"cpu={manifest.get('cpu_model')}; affinity={manifest.get('affinity')}; "
                   f"available_rustc_not_build_identity={manifest['rustc']}; fixture_sha256={manifest['fixture_sha256']}; "
                  f"algorithm={manifest['algorithm']}; iterations=120000000; slots=1; "
                  "ranges are measured calibration extrema, not confidence intervals")
    metric = {"unit": "work_per_second", "low": rates[0],
              "central": statistics.median(rates), "high": rates[-1],
              "provenance": provenance("calibrated", "capture sha256: " + ", ".join(hashes),
                                       manifest["date"], conditions)}
    return {"version": 1, "id": identifier, "name": f"{manifest['hostname']} LCG calibration",
            "nodes": 1, "placement": {"ranks_per_node": 1, "threads_per_rank": 1,
                                      "gpus_per_rank": 0},
            "provenance": provenance("measured", "capture sha256: " + ", ".join(hashes),
                                     manifest["date"], conditions),
            "resources": {"cpu": {"kind": "cpu", "description": "One LCG process service slot; capacity unknown",
                                  "slots_per_node": 1, "capacity_bytes_per_node": None,
                                  "features": [], "metrics": {METRIC: metric}}}}


def compare(args):
    source = load_manifest(args.source_dir)
    target = load_manifest(args.target_dir)
    require(args.source_dir.resolve() != args.target_dir.resolve(), "source and target must differ")
    require(source["algorithm"] == target["algorithm"] and
            source["fixture_sha256"] == target["fixture_sha256"], "fixture algorithm/identity differs")
    for iterations in (120_000_000, 300_000_000):
        require({r["checksum"] for r in source["runs"] if r["iterations"] == iterations} ==
                {r["checksum"] for r in target["runs"] if r["iterations"] == iterations},
                "source/target checksum mismatch")
    args.output_dir.mkdir()
    out = args.output_dir
    source_profile, target_profile = profile(source, "source"), profile(target, "target")
    dump_new(out / "source-hardware.json", source_profile)
    dump_new(out / "target-hardware.json", target_profile)
    source_heldout = [r for r in source["runs"] if r["split"] == "heldout"]
    for r in source_heldout:
        actual = observation(args.source_dir / r["capture"])
        require(all(r[key] == value for key, value in actual.items()),
                "source held-out manifest observation mismatch")
    baseline = sorted(source_heldout, key=lambda r: r["work_ns"])[1]
    baseline_path = args.source_dir / baseline["capture"]
    evidence_path = out / "evidence.json"
    run([args.rprof, "project", baseline_path, "--inspect", "--output", evidence_path])
    evidence = json.loads(evidence_path.read_text(encoding="utf-8"))
    require(evidence["capture_sha256"] == baseline["sha256"] and evidence["complete"] and
            evidence["dropped"] == "0" and evidence["omitted"] == "0", "partial baseline evidence")
    spans = [s for s in evidence["spans"] if s["name"] == "lcg.work"]
    require(len(spans) == 1 and not spans[0]["incomplete"] and not spans[0]["enclosing"] and
            int(spans[0]["duration_ns"]) == baseline["work_ns"], "baseline span mismatch")
    span = spans[0]
    workload = {"version": 1, "name": "Measured single-process LCG iterations",
                "baseline_sha256": baseline["sha256"], "domain": evidence["domain"],
                "window_start_ns": span["start_ns"], "window_duration_ns": span["duration_ns"],
                "assumptions": "Steady LCG iteration throughput scales linearly for this fixed work size; compiler, kernel, CPU frequency and contention can differ. No universal CPU claim.",
                "unmodeled": "reject",
                "phases": [{"id": "lcg", "span_id": span["id"], "name": "lcg.work",
                            "resource": "cpu", "model": {"kind": "throughput", "metric": METRIC},
                            "node_scaling": {"kind": "fixed"},
                            "assumption": "Fixed 300m LCG iterations; calibration uses only 120m runs. No held-out target duration fits this model."}]}
    dump_new(out / "workload-model.json", workload)
    command = [args.rprof, "project", baseline_path, "--source-hardware", out / "source-hardware.json",
               "--target-hardware", out / "target-hardware.json",
               "--workload-model", out / "workload-model.json"]
    prediction_path = out / "prediction.json"
    run([*command, "--output", prediction_path])
    prediction = json.loads(prediction_path.read_text(encoding="utf-8"))
    scenario = next(s for s in prediction["scenarios"] if s["target_id"] == "target" and s["nodes"] == 1)
    require(scenario["status"] == "estimated", f"projection unsupported: {scenario['issues']}")
    predicted_ns = int(scenario["runtime"]["central_ns"])
    # Prediction has been written before any target held-out duration is parsed/compared.
    heldout = [r for r in target["runs"] if r["split"] == "heldout"]
    durations = []
    for r in heldout:
        actual = observation(args.target_dir / r["capture"])
        require(all(r[key] == value for key, value in actual.items()), "held-out manifest observation mismatch")
        durations.append(actual["work_ns"])
    observed_ns = int(statistics.median(durations))
    validation = [{"target_id": "target", "nodes": 1, "observed_ns": str(observed_ns),
                   "workload_sha256": prediction["hashes"]["workload"],
                   "target_profile_sha256": prediction["hashes"]["target:target"],
                   "provenance": provenance("measured", "held-out capture sha256: " +
                                            ", ".join(r["sha256"] for r in heldout),
                                            target["date"], "Three 300m LCG iterations runs; median body nanoseconds; excluded from fitting") }]
    dump_new(out / "validation-observations.json", validation)
    run([*command, "--validation", out / "validation-observations.json", "--output", out / "validated.json"])
    run([*command, "--validation", out / "validation-observations.json", "--format", "html",
         "--output", out / "validated.html"])
    signed_error = (predicted_ns - observed_ns) / observed_ns
    result = {"prediction_ns": predicted_ns, "observed_ns": observed_ns,
              "prediction_range_ns": {key: int(value) for key, value in scenario["runtime"].items()},
              "target_heldout_body_ns": durations,
              "signed_error": signed_error, "relative_error": abs(signed_error),
              "tolerance": TOLERANCE, "passed": abs(signed_error) <= TOLERANCE,
              "source_calibration_sha256": [r["sha256"] for r in source["runs"] if r["split"] == "calibration"],
              "target_calibration_sha256": [r["sha256"] for r in target["runs"] if r["split"] == "calibration"],
              "source_heldout_sha256": [r["sha256"] for r in source_heldout],
              "target_heldout_sha256": [r["sha256"] for r in heldout],
              "source_calibration_rates": source_profile["resources"]["cpu"]["metrics"][METRIC],
              "target_calibration_rates": target_profile["resources"]["cpu"]["metrics"][METRIC],
              "note": "Parameter ranges reflect calibration extrema, not statistical confidence intervals."}
    dump_new(out / "empirical-validation.json", result)
    print("Calibration ranges (LCG iterations/s; not confidence intervals):")
    for label, hardware in (("source", source_profile), ("target", target_profile)):
        metric = hardware["resources"]["cpu"]["metrics"][METRIC]
        print(f"  {label}: {metric['low']:.3f} .. {metric['high']:.3f} (median {metric['central']:.3f})")
    print(f"Predicted {predicted_ns} ns; measured held-out median {observed_ns} ns; "
          f"relative error {abs(signed_error):.2%}; tolerance {TOLERANCE:.0%}; "
          f"{'PASS' if result['passed'] else 'FAIL'}")
    if not result["passed"]:
        raise ValueError("held-out relative error exceeds predeclared tolerance")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    collect_parser = sub.add_parser("collect")
    collect_parser.add_argument("--fixture", type=Path, required=True)
    collect_parser.add_argument("--output-dir", type=Path, required=True)
    compare_parser = sub.add_parser("compare")
    compare_parser.add_argument("--source-dir", type=Path, required=True)
    compare_parser.add_argument("--target-dir", type=Path, required=True)
    compare_parser.add_argument("--rprof", type=Path, required=True)
    compare_parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    try:
        (collect if args.command == "collect" else compare)(args)
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"error: {error}\n")


if __name__ == "__main__":
    main()
