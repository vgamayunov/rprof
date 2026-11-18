#define _DEFAULT_SOURCE
#include <stdint.h>
#include <time.h>
#include <unistd.h>

static uint64_t thread_cpu_ns(void) {
    struct timespec time;
    if (clock_gettime(CLOCK_THREAD_CPUTIME_ID, &time) != 0) _exit(6);
    return (uint64_t)time.tv_sec * 1000000000 + (uint64_t)time.tv_nsec;
}
__attribute__((noinline)) void stack_spin(void) {
    volatile uint64_t total = 0;
    uint64_t start = thread_cpu_ns();
    do {
        for (uint64_t i = 0; i < 1000000; i++) total += i ^ (i >> 3);
    } while (thread_cpu_ns() - start < 300000000);
    (void)total;
    const char message[] = "stack-work\n";
    if (write(STDOUT_FILENO, message, sizeof(message) - 1) != (ssize_t)(sizeof(message) - 1)) _exit(5);
    usleep(50000);
}
__attribute__((noinline)) void stack_outer(void) {
    stack_spin();
    __asm__ volatile("" ::: "memory");
}

#ifdef RPROF_PYTHON
#include <Python.h>
static PyObject *spin(PyObject *self, PyObject *unused) {
    (void)self; (void)unused;
    Py_BEGIN_ALLOW_THREADS
    stack_outer();
    Py_END_ALLOW_THREADS
    Py_RETURN_NONE;
}
static PyMethodDef methods[] = {
    {"spin", spin, METH_NOARGS, NULL}, {NULL, NULL, 0, NULL}
};
static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT, "_rprof_stack_workload", NULL, -1, methods, NULL, NULL, NULL, NULL
};
PyMODINIT_FUNC PyInit__rprof_stack_workload(void) { return PyModule_Create(&module); }
#else
#include <pthread.h>
#include <sys/wait.h>
static void *worker(void *unused) { (void)unused; stack_outer(); return NULL; }
int main(void) {
    pid_t child = fork();
    if (child < 0) return 2;
    if (child == 0) { stack_outer(); _exit(0); }
    pthread_t first, second;
    if (pthread_create(&first, NULL, worker, NULL) || pthread_create(&second, NULL, worker, NULL)) return 3;
    stack_outer();
    pthread_join(first, NULL); pthread_join(second, NULL);
    int status;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status)) return 4;
    return WEXITSTATUS(status);
}
#endif
