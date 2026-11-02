import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("validate_projection", Path(__file__).parents[1] / "validate-projection.py")
validation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(validation)


class ProjectionValidation(unittest.TestCase):
    def test_target_held_out_durations_do_not_change_fitted_profile(self):
        manifest = {"hostname": "test", "kernel": "test", "rustc": "test", "fixture_sha256": "a" * 64,
                    "algorithm": "b" * 64, "date": "2026-10-31", "runs": [
                        {"split": "calibration", "iterations": 120000000, "work_ns": ns, "sha256": str(i) * 64}
                        for i, ns in enumerate([200000000, 210000000, 220000000])
                    ] + [{"split": "heldout", "iterations": 300000000, "work_ns": 999999999, "sha256": "d" * 64}]}
        original = validation.profile(manifest, "target")
        changed = copy.deepcopy(manifest)
        changed["runs"][-1]["work_ns"] = 1
        self.assertEqual(original, validation.profile(changed, "target"))
        self.assertEqual(validation.TOLERANCE, 0.15)

    def test_lossy_reference_is_rejected(self):
        frames = [
            {"record": "header", "data": {"format": "rprof-stream", "schema_version": 2}},
            {"record": "shard", "data": {"shard_id": "test"}},
            {"record": "end", "data": {"shard_id": "test", "exit_code": 0, "reason": "completed", "loss": {"ingress": 1}}},
        ]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "capture"
            path.write_text("\n".join(json.dumps(frame) for frame in frames) + "\n")
            with self.assertRaisesRegex(ValueError, "incomplete/lossy"):
                validation.observation(path)


if __name__ == "__main__":
    unittest.main()
