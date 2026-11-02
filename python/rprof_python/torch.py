"""Opt-in scheduled framework-owned collection, exported for rprof import-pytorch."""
from contextlib import contextmanager
import os
import json
import sys
from pathlib import Path
import tempfile
import time
import uuid


@contextmanager
def torch_profile(output_dir, *, wait=0, warmup=1, active=2, repeat=1,
                  activities=("cpu",), record_shapes=False, with_stack=False,
                  profile_memory=False, with_flops=False):
    """Yield torch.profiler.profile; call step() after each training/inference step.

    CPU is always explicit; request activities=("cpu", "cuda") for CUDA.
    Each completed active window is exported to a new Chrome JSON file.
    """
    if os.environ.get("CUDA_INJECTION64_PATH"):
        raise RuntimeError("framework-owned torch profiling requires CUDA_INJECTION64_PATH to be unset")
    if min(wait, warmup) < 0 or active < 1 or repeat < 1:
        raise ValueError("invalid torch profiling schedule")
    if not activities or any(a not in ("cpu", "cuda") for a in activities):
        raise ValueError("activities must contain cpu and/or cuda")
    import torch
    if with_stack and sys.getprofile() is not None:
        raise RuntimeError("an existing Python profile hook owns stack instrumentation")
    from . import _runtime
    if _runtime._state is not None and _runtime._state.initialized and not _runtime._state.stopped and _runtime._state.backend == "profile":
        raise RuntimeError("combined Python/torch profiling requires the sys.monitoring backend")
    if "cuda" in activities and not torch.cuda.is_available():
        raise RuntimeError("CUDA was requested but torch.cuda.is_available() is false")
    destination = Path(output_dir)
    destination.mkdir(parents=True, exist_ok=True)
    files = []

    def export(profiler):
        name = f"torch-{os.getpid()}-{time.time_ns()}-{uuid.uuid4().hex}.json"
        with tempfile.TemporaryDirectory(prefix=".rprof-torch-", dir=destination) as staging:
            temporary = Path(staging) / "trace.json"
            profiler.export_chrome_trace(str(temporary))
            path = destination / name
            os.link(temporary, path)  # Atomic no-clobber publication.
            files.append(str(path))

    selected = [torch.profiler.ProfilerActivity.CPU if a == "cpu" else torch.profiler.ProfilerActivity.CUDA for a in activities]
    context = torch.profiler.profile(activities=selected,
                                schedule=torch.profiler.schedule(wait=wait, warmup=warmup, active=active, repeat=repeat),
                                on_trace_ready=export, record_shapes=record_shapes,
                                with_stack=with_stack, profile_memory=profile_memory,
                                with_flops=with_flops)
    metadata = {"collection": "framework-owned", "source_loss_known": False,
                "torch_version": torch.__version__, "cuda_version": torch.version.cuda,
                "schedule": {"wait": wait, "warmup": warmup, "active": active, "repeat": repeat}}
    if torch.distributed.is_available() and torch.distributed.is_initialized():
        metadata["distributed"] = {"backend": str(torch.distributed.get_backend()),
                                   "rank": torch.distributed.get_rank(), "world_size": torch.distributed.get_world_size()}
    if hasattr(context, "preset_metadata_json"):
        context.preset_metadata_json("rprof", json.dumps(metadata))
    with context as profiler:
        profiler.rprof_trace_files = files
        yield profiler
