"""A deliberately tiny-function overhead probe, not a representative workload."""
import time


def tiny(value):
    return value + 1


def main():
    start = time.perf_counter_ns()
    total = 0
    for i in range(5000):
        total += tiny(i)
    elapsed = time.perf_counter_ns() - start
    assert total == 12502500
    print("body_ns", elapsed)


main()
