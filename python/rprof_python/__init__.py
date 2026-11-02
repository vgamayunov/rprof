"""Optional CPython instrumentation and framework-owned torch.profiler helpers."""
from ._runtime import install, stop, range
from .torch import torch_profile

__all__ = ["install", "stop", "range", "torch_profile"]
