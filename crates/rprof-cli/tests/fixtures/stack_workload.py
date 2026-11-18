from _rprof_stack_workload import spin


def python_inner():
    spin()


def python_outer():
    python_inner()


python_outer()
print("mixed stack workload completed")
