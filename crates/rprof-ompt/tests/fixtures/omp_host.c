/* Four-thread host fixture: worksharing, nested teams, scheduling, and deps.
 * Build the GCC-generated object separately when linking with LLVM libomp:
 *   gcc -O0 -fopenmp -c omp_host.c -o omp_host.o
 *   gcc omp_host.o -L/path/to/llvm/lib -Wl,-rpath,/path/to/llvm/lib -lomp -pthread -o omp_host
 * (Do not use -fopenmp at link time: it would select libgomp.) */
#include <assert.h>
#include <omp.h>
#include <stdio.h>

int main(void) {
    enum { N = 128, TASKS = 12 };
    int values[N] = {0};
    int task_values[TASKS] = {0};
    int sections = 0, singles = 0, nested = 0, chain = 0;
    omp_set_dynamic(0);
    omp_set_max_active_levels(2);

#pragma omp parallel num_threads(4) shared(values, task_values, sections, singles, nested, chain)
    {
        /* A heavy last iteration creates measurable work/barrier imbalance. */
#pragma omp for schedule(static)
        for (int i = 0; i < N; ++i) {
            volatile unsigned spin = 0;
            for (unsigned j = 0; j < (i == N - 1 ? 300000u : 300u); ++j)
                spin += j & 1u;
            (void)spin;
            values[i] = i * i;
        }
#pragma omp sections reduction(+:sections)
        {
#pragma omp section
            sections += 3;
#pragma omp section
            sections += 5;
        }
#pragma omp single
        {
            ++singles;
#pragma omp parallel num_threads(2) reduction(+:nested)
            { nested += omp_get_thread_num() + 1; }
#pragma omp taskgroup
            {
                for (int i = 0; i < TASKS; ++i) {
#pragma omp task firstprivate(i) shared(task_values)
                    {
#pragma omp taskyield
                        task_values[i] = i + 1;
                    }
                }
            }
            /* The inout chain provides dependence edges with a known result. */
            for (int i = 0; i < TASKS; ++i) {
#pragma omp task firstprivate(i) depend(inout:chain)
                { chain += i + 1; }
            }
#pragma omp taskwait
        }
#pragma omp barrier
    }
    long long squares = 0;
    for (int i = 0; i < N; ++i) {
        assert(values[i] == i * i);
        squares += values[i];
    }
    for (int i = 0; i < TASKS; ++i) assert(task_values[i] == i + 1);
    assert(squares == 690880 && sections == 8 && singles == 1);
    assert(nested == 3 && chain == TASKS * (TASKS + 1) / 2);
    printf("omp_host ok: squares=%lld sections=%d nested=%d chain=%d\n",
           squares, sections, nested, chain);
    return 0;
}
