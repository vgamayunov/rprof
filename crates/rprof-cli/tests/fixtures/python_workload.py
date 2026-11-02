import asyncio
import multiprocessing
import os
import sys
import threading
import time

import rprof_python as rprof


def leaf(value):
    return value + 1


def suspended_generator():
    yield leaf(1)
    yield leaf(2)


async def suspended_coroutine():
    leaf(3)
    await asyncio.sleep(0.04)
    return leaf(4)


def io_region(path):
    with open(path, "wb", buffering=0) as output:
        output.write(b"python-native-io")
    with open(path, "rb", buffering=0) as source:
        assert source.read() == b"python-native-io"


def thread_worker():
    with rprof.range("worker range"):
        assert leaf(9) == 10
        time.sleep(0.005)


def process_worker(path):
    with rprof.range("process work"):
        io_region(path)
        assert leaf(20) == 21


def recurse(n):
    return recurse(n - 1) + 1 if n else 0


def main():
    mode = sys.argv[1]
    if mode == "basic":
        with rprof.range("generator elapsed"):
            generator = suspended_generator()
            assert next(generator) == 2
            time.sleep(0.04)
            assert next(generator) == 3
            try:
                next(generator)
            except StopIteration:
                pass
        with rprof.range("coroutine elapsed"):
            assert asyncio.run(suspended_coroutine()) == 5
        threads = [threading.Thread(target=thread_worker) for _ in range(2)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
    elif mode == "io":
        io_region(sys.argv[2])
    elif mode == "overflow":
        assert recurse(180) == 180
    elif mode == "exit":
        raise SystemExit(23)
    elif mode in multiprocessing.get_all_start_methods():
        context = multiprocessing.get_context(mode)
        children = [context.Process(target=process_worker, args=(sys.argv[2] + str(i),)) for i in range(2)]
        for child in children:
            child.start()
        for child in children:
            child.join()
            assert child.exitcode == 0
    else:
        raise ValueError(mode)
    print("python workload verified", os.getpid())


if __name__ == "__main__":
    main()
