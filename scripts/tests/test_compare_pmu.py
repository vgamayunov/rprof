import importlib.util
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

spec = importlib.util.spec_from_file_location("compare_pmu", Path(__file__).parents[1] / "compare-pmu.py")
compare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compare)


class PerfComparisonParsing(unittest.TestCase):
    def test_command_modifiers_are_not_silently_discarded(self):
        with self.assertRaisesRegex(ValueError, "without modifiers"):
            compare.compare(SimpleNamespace(runs=1, events="cycles:k"))

    def test_reference_units_coverage_aliases_and_unavailable_counts(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "perf.csv"
            path.write_text("# started\n\n123456;;cpu-cycles:u;100000;75.25;;\n"
                            "2.50;msec;task-clock:uk;2500000;100.00;;\n"
                            "<not supported>;;bus-cycles:u;0;100.00;;\n"
                            "<not counted>;;ref-cycles:u;0;0.00;;\n")
            values = compare.read_reference(path)
            self.assertEqual(values["cycles"]["scaled"], 123456)
            self.assertEqual(values["cycles"]["running_percent"], 75.25)
            self.assertEqual(values["task-clock"]["scaled"], 2500000)
            self.assertIsNone(values["bus-cycles"]["scaled"])
            self.assertEqual(values["ref-cycles"]["status"], "<not counted>")

    def test_capture_uses_final_counts_and_keeps_raw_scaling_evidence_exact(self):
        def event(raw, final):
            values = {"pmu.event": "r00C0", "pmu.status": "measured", "pmu.final": final,
                      "pmu.scaled_estimate": str(raw * 2), "pmu.time_enabled_ns": 20,
                      "pmu.time_running_ns": 10}
            return {"attributes": {k: {"value": v} for k, v in values.items()},
                    "data": {"value": {"value": raw}}}
        raw = 9007199254740993
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "capture"
            path.write_text(json.dumps({"record": "chunk", "data": {
                "events": [event(1, False), event(raw, True)]}}) + "\n")
            readings, statuses = compare.read_capture(path)
            self.assertEqual(readings["rc0"], {"raw": raw, "scaled": raw * 2, "enabled_ns": 20, "running_ns": 10})
            self.assertEqual(statuses["rc0"], "measured")


if __name__ == "__main__":
    unittest.main()
