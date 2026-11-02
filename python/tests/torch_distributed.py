"""Two-rank, single-host Gloo reference; backend identity must survive import."""
import json
from pathlib import Path
import sys
import torch
import torch.distributed as dist
import rprof_python as rprof


def worker(rank, directory):
    root = Path(directory)
    torch.set_num_threads(1)
    dist.init_process_group("gloo", init_method=(root / "rendezvous").as_uri(), rank=rank, world_size=2)
    try:
        output = root / f"rank-{rank}"
        with rprof.torch_profile(output, warmup=1, active=1) as profiler:
            for _ in range(2):
                with torch.profiler.record_function("distributed step"):
                    value = torch.full((8,), float(rank + 1))
                    dist.all_reduce(value)
                    assert torch.equal(value, torch.full((8,), 3.0))
                profiler.step()
        reference = [{"name": e.key, "count": e.count, "cpu_ns": round(e.cpu_time_total * 1000),
                      "self_cpu_ns": round(e.self_cpu_time_total * 1000)} for e in profiler.key_averages()]
        with (output / "reference.json").open("x") as file:
            json.dump({"torch": torch.__version__, "cuda": torch.version.cuda, "device": "cpu",
                       "compiled": False, "rank": rank, "backend": "gloo",
                       "traces": profiler.rprof_trace_files, "operators": reference}, file, indent=2)
    finally:
        dist.destroy_process_group()


if __name__ == "__main__":
    directory = Path(sys.argv[1]).resolve()
    directory.mkdir(parents=True, exist_ok=False)
    torch.multiprocessing.spawn(worker, args=(str(directory),), nprocs=2, join=True)
    print("two-rank Gloo verified")
