"""rprof launch shim. Run any pre-existing sitecustomize before installing hooks."""
import importlib.machinery
import importlib.util
import os
import sys

_directory = os.path.realpath(os.path.dirname(__file__))
_paths = [p for p in sys.path if os.path.realpath(p or os.getcwd()) != _directory]
_original = importlib.machinery.PathFinder.find_spec("sitecustomize", _paths)
if _original is not None and _original.loader is not None:
    _module = importlib.util.module_from_spec(_original)
    sys.modules["sitecustomize"] = _module
    _original.loader.exec_module(_module)

from rprof_python import install
install()
