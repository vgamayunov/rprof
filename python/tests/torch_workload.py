"""Small eager/compiled training and DataLoader fixture; optional CUDA."""
import argparse
import json
from pathlib import Path
import torch
import rprof_python as rprof


class Samples(torch.utils.data.Dataset):
    def __len__(self):
        return 4

    def __getitem__(self, index):
        with rprof.range("load sample"):
            return torch.full((16, 16), float(index + 1))


def train_step(x, weight):
    with rprof.range("python train step"):
        return (x @ weight).relu().sum()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--device", choices=("cpu", "cuda"), default="cpu")
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--workers", type=int, default=0)
    parser.add_argument("--compiled", action="store_true")
    parser.add_argument("--native", action="store_true")
    args = parser.parse_args()
    torch.set_num_threads(1)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    weight = torch.ones((16, 16), device=args.device, requires_grad=True)
    loader = torch.utils.data.DataLoader(Samples(), batch_size=1, num_workers=args.workers,
        **({"multiprocessing_context": "spawn"} if args.workers else {}))
    samples = iter(loader)
    step = torch.compile(train_step, backend="inductor") if args.compiled else train_step
    if args.compiled:
        step(torch.ones((1, 16, 16), device=args.device), weight).backward()
        weight.grad = None

    def iteration():
        with torch.profiler.record_function("training iteration"):
            x = next(samples).to(args.device)
            loss = step(x, weight)
            loss.backward()
            assert loss.item() > 0 and weight.grad is not None
            weight.grad = None

    if args.native:
        for _ in range(3):
            iteration()
        files = []
        reference = []
    else:
        activities = ("cpu", "cuda") if args.device == "cuda" else ("cpu",)
        with rprof.torch_profile(args.output_dir, warmup=1, active=2, repeat=1,
                                activities=activities, record_shapes=True,
                                profile_memory=True) as profiler:
            for _ in range(3):
                iteration()
                profiler.step()
        files = profiler.rprof_trace_files
        reference = [{"name": e.key, "count": e.count,
                      "cpu_ns": round(e.cpu_time_total * 1000),
                      "self_cpu_ns": round(e.self_cpu_time_total * 1000)} for e in profiler.key_averages()]
        assert len(files) == 1, files
    if args.device == "cuda":
        torch.cuda.synchronize()
    reference_file = args.output_dir / "reference.json"
    with reference_file.open("x") as output:
        json.dump({"torch": torch.__version__, "cuda": torch.version.cuda,
                   "device": args.device, "compiled": args.compiled,
                   "workers": args.workers, "traces": files, "operators": reference}, output, indent=2)
    print("torch workload verified", args.device, files)


if __name__ == "__main__":
    main()
