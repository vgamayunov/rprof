#!/usr/bin/env python3
"""Import a fixture's traces and compare operator counts/times to torch.profiler."""
import argparse
import json
from pathlib import Path
import re
import subprocess
from decimal import Decimal, ROUND_HALF_UP


def ns(value):
    return int((Decimal(str(value)) * 1000).to_integral_value(rounding=ROUND_HALF_UP))


def cpu_nodes(events):
    """Torch EventList collapses a sole child with the same name as its parent.

    Keep the raw trace untouched; reproduce only that normalization for comparing
    key_averages(), which is not a one-to-one inventory of Chrome X records.
    """
    categories = {"cpu_op", "user_annotation", "python_function", "cuda_runtime", "cuda_driver"}
    nodes = [{"name": e["name"], "lane": (str(e["pid"]), str(e["tid"])),
              "start": ns(e["ts"]), "duration": ns(e["dur"]), "children": [], "parent": None}
             for e in events if e.get("ph") == "X" and e.get("cat") in categories]
    order = sorted(range(len(nodes)), key=lambda i: (nodes[i]["lane"], nodes[i]["start"], -nodes[i]["duration"]))
    stacks = {}
    for index in order:
        node = nodes[index]
        stack = stacks.setdefault(node["lane"], [])
        end = node["start"] + node["duration"]
        while stack and (nodes[stack[-1]]["start"] + nodes[stack[-1]]["duration"] <= node["start"] or
                         nodes[stack[-1]]["start"] + nodes[stack[-1]]["duration"] < end):
            stack.pop()
        if stack:
            node["parent"] = stack[-1]
            nodes[stack[-1]]["children"].append(index)
        stack.append(index)
    removed = set()
    while True:
        before = len(removed)
        for index in order:
            if index in removed:
                continue
            child = nodes[index]
            if child["parent"] is None:
                continue
            parent = nodes[child["parent"]]
            if parent["name"] == child["name"] and len(parent["children"]) == 1:
                parent["children"] = child["children"]
                for other in child["children"]:
                    nodes[other]["parent"] = child["parent"]
                removed.add(index)
        if len(removed) == before:
            break
    return [node for index, node in enumerate(nodes) if index not in removed], len(removed)

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--rprof", type=Path, required=True)
parser.add_argument("--reference", type=Path, required=True)
parser.add_argument("--output-dir", type=Path, required=True)
args = parser.parse_args()
reference = json.loads(args.reference.read_text())
args.output_dir.mkdir(parents=True, exist_ok=False)
assert len(reference["traces"]) == 1, "fixture comparison expects one active profiling window"
trace = args.reference.parent / Path(reference["traces"][0]).name
capture = args.output_dir / "import.jsonl"
html = args.output_dir / "report.html"
subprocess.run([args.rprof, "import-pytorch", trace, "-o", capture], check=True)
subprocess.run([args.rprof, "report", capture, "--format", "html", "-o", html], check=True)
report = json.loads(re.search(r'<script type="application/json" id="rprof-data">(.*?)</script>', html.read_text(), re.S)[1])
rows = report["pytorch"]["activities"]
raw = json.loads(trace.read_text(), parse_float=Decimal)["traceEvents"]
normalized, collapsed = cpu_nodes(raw)
checked = []
self_checked = []
for expected in reference["operators"]:
    if not expected["name"].startswith("aten::") or expected["cpu_ns"] <= 0:
        continue
    found = [row for row in rows if row["role"] == "operator" and row["name"] == expected["name"]]
    assert found, expected["name"]
    count = sum(int(row["count"]) for row in found)
    elapsed = sum(int(row["total_ns"]) for row in found)
    source = [e for e in raw if e.get("ph") == "X" and e.get("cat") == "cpu_op" and e.get("name") == expected["name"]]
    assert count == len(source), (expected["name"], count, len(source))
    assert elapsed == sum(ns(e["dur"]) for e in source), (expected["name"], elapsed)
    comparable = [e for e in normalized if e["name"] == expected["name"]]
    assert len(comparable) == expected["count"], (expected, len(comparable))
    assert abs(sum(e["duration"] for e in comparable) - expected["cpu_ns"]) <= max(2, count * 2), expected
    checked.append(expected["name"])
    if all(row["self_ns"] is not None for row in found) and expected["self_cpu_ns"] >= 0:
        observed_self = sum(int(row["self_ns"]) for row in found)
        # Source JSON timestamps can round independently at nanosecond precision.
        assert abs(observed_self - expected["self_cpu_ns"]) <= max(100, count * 4), (expected, observed_self)
        self_checked.append(expected["name"])
assert checked, "no CPU operators compared"
for category, role in [("kernel", "kernel"), ("gpu_memcpy", "memcpy"), ("gpu_memset", "memset")]:
    expected_count = sum(e.get("ph") == "X" and e.get("cat") == category for e in raw)
    actual_count = sum(int(r["count"]) for r in rows if r["role"] == role)
    assert expected_count == actual_count, (category, expected_count, actual_count)
tracks = {t["id"]: t for t in report["tracks"]}
for span in report["spans"]:
    role = span["attributes"].get("pytorch.role")
    if role in ("kernel", "memcpy", "memset"):
        assert tracks[span["track"]]["kind"] == "gpu_stream"
    if role == "runtime_api":
        assert tracks[span["track"]]["kind"] == "thread"
if reference["device"] == "cuda":
    assert any(r["role"] == "kernel" for r in rows), "CUDA fixture has no GPU kernels"
    spans = {s["correlation"]: s for s in report["spans"] if s["correlation"]}
    for event in spans.values():
        if event["attributes"].get("pytorch.role") not in ("kernel", "memcpy", "memset"):
            continue
        assert any(edge["to"] == event["correlation"] and spans.get(edge["from"], {}).get("attributes", {}).get("pytorch.role") == "runtime_api"
                   for edge in report["dependencies"]), "GPU execution has no provided launch/copy API correlation"
if "backend" in reference:
    metadata = report["pytorch"]["sources"][0]["metadata"]
    assert reference["backend"] in json.dumps(metadata)
    assert all(t["rank"] == reference["rank"] for t in report["tracks"])
    assert all(s["category"] != "mpi" for s in report["spans"])
if reference["compiled"]:
    assert any("Compiled" in r["name"] or "compiled" in r["name"] for r in rows)
assert report["summary"]["dropped"] == "0"
result = {"operators_compared": len(checked), "self_time_rows_compared": len(self_checked),
          "duplicate_name_nodes_collapsed_for_reference": collapsed,
          "gpu_kernels": sum(int(r["count"]) for r in rows if r["role"] == "kernel"),
          "flows": len(report["dependencies"]), "torch": reference["torch"],
          "device": reference["device"], "compiled": reference["compiled"], "passed": True}
(args.output_dir / "comparison.json").write_text(json.dumps(result, indent=2) + "\n")
print(json.dumps(result, indent=2))
