"""Bounded selected-frame instrumentation; no user arguments/locals are retained."""
import atexit
import contextvars
import ctypes
import dis
import hashlib
import json
import os
import socket
import sys
import threading
import time
import uuid
import weakref

MAX_DEPTH = 128
MAX_THREADS = 1024
MAX_CODES = 65536
MAX_PACKET = 8192
_state = None
_ranges = contextvars.ContextVar("rprof_ranges", default=())
_own_directory = os.path.realpath(os.path.dirname(__file__))


def _clean(value, limit):
    encoded = value.encode("utf-8", "replace")
    return encoded[:limit].decode("utf-8", "replace"), len(encoded) > limit or encoded.decode("utf-8") != value


def _image():
    libc = ctypes.CDLL(None)
    libc.getauxval.argtypes = [ctypes.c_ulong]
    libc.getauxval.restype = ctypes.c_ulong
    address = libc.getauxval(25)  # AT_RANDOM; same process-image material as native adapters.
    seed = ctypes.string_at(address, 16) if address else bytes(16)
    material = seed + os.getpid().to_bytes(4, sys.byteorder)
    return max(1, int.from_bytes(hashlib.sha256(material).digest()[:8], sys.byteorder))


class _Thread:
    def __init__(self, identity):
        self.identity = identity
        self.tid = threading.get_native_id()
        self.sequence = 0
        self.stack = []
        self.overflow = 0
        self.busy = False
        self.sending = False


class _State:
    def __init__(self, roots, backend):
        self.pid = os.getpid()
        self.image = _image()
        try:
            get_interpreter = ctypes.pythonapi.PyInterpreterState_Get
            get_interpreter.restype = ctypes.c_void_p
            get_interpreter.argtypes = []
            get_id = ctypes.pythonapi.PyInterpreterState_GetID
            get_id.restype = ctypes.c_int64
            get_id.argtypes = [ctypes.c_void_p]
            self.interpreter = get_id(get_interpreter())
            if self.interpreter < 0:
                self.interpreter = (1 << 64) - 1
        except AttributeError:
            self.interpreter = (1 << 64) - 1  # Explicit unknown, never guessed main.
        self.source = uuid.uuid4().hex
        self.roots = roots
        self.backend = backend
        self.inline = os.environ.get("RPROF_PYTHON_INLINE") == "1"
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        self.socket.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 65536)
        self.socket.settimeout(2.0)
        self.socket.connect(os.environ["RPROF_PYTHON_SOCKET"])
        self.socket.setblocking(False)
        self.local = threading.local()
        self.threads = weakref.WeakSet()
        self.codes = {}
        self.next_thread = self.next_code = self.next_call = 1
        self.attempted = self.dropped = self.omitted = self.unmatched = self.errors = 0
        self.ranges_open = 0
        self.active_ranges = set()
        self.active_callbacks = 0
        self.stopped = False
        self.tool = None
        self.tool_name = "rprof-" + self.source
        self.initialized = False

    def thread(self):
        current = getattr(self.local, "thread", None)
        if current is None:
            if len(self.threads) >= MAX_THREADS:
                self.omitted += 1
                return None
            current = _Thread(self.next_thread)
            self.next_thread += 1
            self.threads.add(current)
            self.local.thread = current
        return current

    def emit(self, kind, current=None, **fields):
        current = current or self.thread()
        if current is None:
            return
        if current.sending:
            self.omitted += 1
            return
        current.sending = True
        current.sequence += 1
        self.attempted += 1
        packet = {"v": 1, "kind": kind, "source": self.source, "pid": self.pid,
                  "tid": current.tid, "thread": current.identity, "image": self.image,
                  "interpreter": self.interpreter, "seq": current.sequence,
                  "attempted": self.attempted, "dropped": self.dropped, "omitted": self.omitted,
                  "unmatched": self.unmatched, "errors": self.errors, "at": time.monotonic_ns(), **fields}
        try:
            encoded = json.dumps(packet, ensure_ascii=True, separators=(",", ":")).encode("ascii")
            if len(encoded) > MAX_PACKET:
                self.dropped += 1
                return
            self.socket.send(encoded, socket.MSG_DONTWAIT | socket.MSG_NOSIGNAL)
        except (OSError, ValueError, UnicodeError):
            self.dropped += 1
        finally:
            current.sending = False

    def error(self, detail):
        self.errors += 1
        self.emit("diagnostic", detail=_clean(str(detail), 1024)[0])

    def describe(self, code):
        key = id(code)
        cached = self.codes.get(key)
        if cached is not None and cached[0]() is code:
            return cached[1]
        filename = code.co_filename
        if filename in ("<stdin>", "<string>"):
            included = self.inline
        elif filename.startswith("<"):
            included = False
        else:
            filename = os.path.realpath(filename)
            included = not filename.startswith(_own_directory + os.sep) and filename != os.path.join(os.path.dirname(_own_directory), "sitecustomize.py") and any(
                filename == root or filename.startswith(root + os.sep) for root in self.roots)
        if not included:
            if len(self.codes) < MAX_CODES:
                cache = self.codes
                self.codes[key] = (weakref.ref(code, lambda ref: cache.pop(key, None)), None)
            return None
        if self.next_code > MAX_CODES or len(self.codes) >= MAX_CODES:
            self.omitted += 1
            return None
        name, truncated_name = _clean(getattr(code, "co_qualname", code.co_name), 256)
        filename, truncated_file = _clean(filename, 768)
        kind = "async_generator" if code.co_flags & 0x200 else "coroutine" if code.co_flags & 0x80 else "generator" if code.co_flags & 0x20 else "function"
        result = {"symbol": self.next_code, "name": name, "file": filename,
                  "line": code.co_firstlineno, "execution_kind": kind,
                  "truncated": truncated_name or truncated_file}
        self.next_code += 1
        cache = self.codes
        self.codes[key] = (weakref.ref(code, lambda ref: cache.pop(key, None)), result)
        return result

    def begin(self, code, frame_id, reason):
        current = self.thread()
        if current is None or current.busy or self.stopped:
            return
        current.busy = True
        try:
            descriptor = self.describe(code)
            if descriptor is None:
                return
            if current.overflow or len(current.stack) >= MAX_DEPTH:
                current.overflow += 1
                self.omitted += 1
                return
            call = self.next_call
            self.next_call += 1
            start = time.monotonic_ns()
            parent = current.stack[-1][1] if current.stack else 0
            current.stack.append([frame_id, call, start, 0, descriptor])
            self.emit("begin", current, call=call, parent=parent, start=start,
                      boundary=reason, **descriptor)
        finally:
            current.busy = False

    def end(self, code, frame_id, reason, end):
        current = self.thread()
        if current is None or current.busy or self.stopped:
            return
        current.busy = True
        try:
            if self.describe(code) is None:
                return
            if current.overflow:
                current.overflow -= 1
                self.omitted += 1
                return
            if not current.stack or current.stack[-1][0] != frame_id:
                self.unmatched += 1
                return
            _, call, start, children, descriptor = current.stack.pop()
            if end < start:
                self.error("monotonic clock moved backwards")
                return
            duration = end - start
            if current.stack:
                current.stack[-1][3] += duration
            self.emit("end", current, call=call, start=start, end=end,
                      self_ns=max(0, duration - children), boundary=reason, **descriptor)
        finally:
            current.busy = False


def _callback_failure(state, error):
    # Profiler implementation failures must not replace application results.
    try:
        state.error("profiling callback failed: " + type(error).__name__)
        stop()
    except Exception:
        state.stopped = True


def _monitor(reason):
    def callback(code, *unused):
        end = time.monotonic_ns()
        state = _state
        if state is None or state.stopped:
            return
        state.active_callbacks += 1
        try:
            if reason == "start":
                if state.describe(code) is None:
                    return sys.monitoring.DISABLE
                events = sys.monitoring.events
                # PY_THROW/PY_UNWIND are global exception events, not local events.
                sys.monitoring.set_local_events(state.tool, code,
                                                events.PY_RETURN | events.PY_YIELD | events.PY_RESUME)
            frame_id = id(sys._getframe(1))
            if reason in ("start", "resume", "throw"):
                state.begin(code, frame_id, reason)
            else:
                state.end(code, frame_id, reason, end)
        except Exception as error:
            _callback_failure(state, error)
        finally:
            state.active_callbacks -= 1
    return callback


_start, _resume, _throw = _monitor("start"), _monitor("resume"), _monitor("throw")
_return, _yield, _unwind = _monitor("return"), _monitor("yield"), _monitor("unwind")


def _profile(frame, event, arg):
    state = _state
    if state is None or state.stopped:
        return
    state.active_callbacks += 1
    try:
        if event == "call":
            state.begin(frame.f_code, id(frame), "call_or_resume")
        elif event == "return":
            end = time.monotonic_ns()
            opcode = dis.opname[frame.f_code.co_code[frame.f_lasti]] if frame.f_lasti >= 0 else ""
            reason = "yield" if opcode in ("YIELD_VALUE", "YIELD_FROM") else "return" if opcode.startswith("RETURN") else "unwind"
            state.end(frame.f_code, id(frame), reason, end)
    except Exception as error:
        _callback_failure(state, error)
    finally:
        state.active_callbacks -= 1


class _AfterFork:
    pass


_fork_anchor = _AfterFork()


def _mp_finalize(_=None):
    from multiprocessing.util import Finalize
    Finalize(None, stop, exitpriority=-100)


def _fork_child():
    global _state
    prior = _state
    if prior is None or prior.stopped:
        return
    try:
        state = _State(prior.roots, prior.backend)
        state.tool, state.tool_name = prior.tool, prior.tool_name
        state.initialized = prior.initialized
        _state = state
        _ranges.set(())
        prior.socket.close()
        _runtime_metadata(state, forked=True)
    except Exception:
        # Never use inherited frame/call state if resetting failed.
        _state = None


def _runtime_metadata(state, forked=False):
    state.emit("runtime", initialized=state.initialized, backend=state.backend,
               version=sys.version.split()[0], implementation=sys.implementation.name,
               clock=time.get_clock_info("monotonic").implementation,
               ppid=os.getppid(), forked=forked,
               detail=_clean(json.dumps({"include": state.roots, "depth_limit": MAX_DEPTH,
                                        "inline": state.inline,
                                        "code_limit": MAX_CODES, "tool_id": state.tool,
                                        "scope": "user range lifetimes only" if state.backend == "ranges" else "selected frame execution segments; user ranges are elapsed lifetimes"}), 2048)[0])


def install():
    """Install once from the explicit rprof launch environment; no-op otherwise."""
    global _state
    if _state is not None or not os.environ.get("RPROF_PYTHON_SOCKET"):
        return
    try:
        roots = [os.path.realpath(p) for p in json.loads(os.environ.get("RPROF_PYTHON_INCLUDE", "[]"))]
        backend = os.environ.get("RPROF_PYTHON_BACKEND", "auto")
        state = _State(roots or [os.getcwd()], backend)
        _state = state
        if sys.implementation.name != "cpython" or not (3, 10) <= sys.version_info[:2] <= (3, 14):
            raise RuntimeError("supported interpreter API matrix is CPython 3.10–3.14")
        if hasattr(sys, "_is_gil_enabled") and not sys._is_gil_enabled():
            raise RuntimeError("free-threaded execution is not supported by this adapter")
        # Register a finalizer after multiprocessing clears inherited registries.
        from multiprocessing.util import register_after_fork
        register_after_fork(_fork_anchor, _mp_finalize)
        _mp_finalize()
        os.register_at_fork(after_in_child=_fork_child)
        atexit.register(stop)
        if backend == "auto":
            backend = "monitoring" if hasattr(sys, "monitoring") else "profile"
        state.backend = backend
        if backend == "monitoring":
            if not hasattr(sys, "monitoring"):
                raise RuntimeError("sys.monitoring requires CPython 3.12+")
            for tool in (3, 4, 5, 2):
                if sys.monitoring.get_tool(tool) is None:
                    sys.monitoring.use_tool_id(tool, state.tool_name)
                    state.tool = tool
                    break
            if state.tool is None:
                raise RuntimeError("no available sys.monitoring tool slot; existing tools were preserved")
            events = sys.monitoring.events
            for event, callback in [(events.PY_START, _start), (events.PY_RESUME, _resume),
                                    (events.PY_THROW, _throw), (events.PY_RETURN, _return),
                                    (events.PY_YIELD, _yield), (events.PY_UNWIND, _unwind)]:
                sys.monitoring.register_callback(state.tool, event, callback)
            state.initialized = True
            _runtime_metadata(state)
            sys.monitoring.set_events(state.tool, events.PY_START | events.PY_THROW | events.PY_UNWIND)
        elif backend == "profile":
            if sys.getprofile() is not None or threading.getprofile() is not None:
                raise RuntimeError("an existing Python profile hook owns instrumentation; it was preserved")
            state.initialized = True
            _runtime_metadata(state)
            threading.setprofile(_profile)
            sys.setprofile(_profile)
        elif backend == "ranges":
            state.initialized = True
            _runtime_metadata(state)
        else:
            raise RuntimeError("unknown Python backend")
    except Exception as error:
        if _state is not None:
            _state.error(error)
            _runtime_metadata(_state)


def stop():
    """Finish this interpreter's observation window without changing application status."""
    state = _state
    if state is None or state.stopped:
        return
    state.stopped = True
    if state.tool is not None:
        if sys.monitoring.get_tool(state.tool) == state.tool_name:
            sys.monitoring.set_events(state.tool, 0)
            for reference, _ in list(state.codes.values()):
                code = reference()
                if code is not None:
                    sys.monitoring.set_local_events(state.tool, code, 0)
            sys.monitoring.free_tool_id(state.tool)
        else:
            state.error("monitoring tool ownership changed during collection")
    elif state.initialized and state.backend == "profile":
        if sys.getprofile() is _profile:
            sys.setprofile(None)
        else:
            state.error("Python profile hook changed during collection")
        if threading.getprofile() is _profile:
            threading.setprofile(None)
    state.emit("final", finalized=True,
               abandoned=sum(len(t.stack) for t in state.threads) + state.ranges_open,
               inflight=state.active_callbacks)
    state.socket.close()


class range:
    """Elapsed user range; may include suspension, GIL/native waits and nested work."""
    def __init__(self, name):
        if not isinstance(name, str):
            raise TypeError("range name must be a string")
        self.name, self.truncated = _clean(name, 256)
        self.active = False

    def __enter__(self):
        if self.active:
            raise RuntimeError("a user range cannot be re-entered while active")
        state = _state
        if state is None or state.stopped or not state.initialized:
            return self
        current = state.thread()
        if current is None:
            return self
        self.pid = state.pid
        self.start = time.monotonic_ns()
        self.call = state.next_call
        state.next_call += 1
        self.current = current
        parents = tuple(call for call in _ranges.get() if call in state.active_ranges)
        if len(parents) >= MAX_DEPTH or state.ranges_open >= MAX_CODES:
            state.omitted += 2
            return self
        parent = parents[-1] if parents else current.stack[-1][1] if current.stack else 0
        self.token = _ranges.set(parents + (self.call,))
        state.ranges_open += 1
        state.active_ranges.add(self.call)
        self.active = True
        state.emit("range_begin", current, call=self.call, start=self.start, parent=parent,
                   name=self.name, truncated=self.truncated)
        return self

    def __exit__(self, *exc):
        state = _state
        if self.active and state is not None and not state.stopped and state.pid == self.pid:
            if threading.get_native_id() != self.current.tid:
                state.error("user range closed on a different thread; no elapsed interval was attributed")
                state.ranges_open -= 1
                state.active_ranges.discard(self.call)
                self.active = False
                return False
            end = time.monotonic_ns()
            state.emit("range_end", self.current, call=self.call, start=self.start, end=end,
                       name=self.name, truncated=self.truncated, boundary="exception" if exc[0] else "return")
            state.ranges_open -= 1
            state.active_ranges.discard(self.call)
            try:
                _ranges.reset(self.token)
            except ValueError:
                state.error("user range crossed Python contexts; parent nesting may be incomplete")
        self.active = False
        return False
