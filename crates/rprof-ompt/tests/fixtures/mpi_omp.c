/* Run with exactly two MPI ranks and four OpenMP threads per rank.
 * Compile with mpicc -fopenmp -c; link without -fopenmp when selecting libomp.
 * Only the main (MPI_THREAD_FUNNELED) thread ever calls MPI. */
#include <assert.h>
#include <mpi.h>
#include <omp.h>
#include <stdio.h>

int main(int argc, char **argv) {
    int provided = 0, rank = -1, size = 0;
    int rc = MPI_Init_thread(&argc, &argv, MPI_THREAD_FUNNELED, &provided);
    assert(rc == MPI_SUCCESS);
    assert(provided >= MPI_THREAD_FUNNELED);
    rc = MPI_Comm_rank(MPI_COMM_WORLD, &rank);
    assert(rc == MPI_SUCCESS);
    rc = MPI_Comm_size(MPI_COMM_WORLD, &size);
    assert(rc == MPI_SUCCESS);
    assert(size == 2);
    omp_set_dynamic(0);

    long long local = 0, global = 0;
#pragma omp parallel num_threads(4) reduction(+:local)
    {
        assert(omp_get_num_threads() == 4);
#pragma omp for schedule(static)
        for (int i = 0; i < 128; ++i) local += (long long)(rank + 1) * (i + 1);
#pragma omp barrier
#pragma omp single
        { assert(omp_get_num_threads() == 4); }
#pragma omp barrier
    }
    assert(local == (long long)(rank + 1) * 8256);
    rc = MPI_Barrier(MPI_COMM_WORLD);
    assert(rc == MPI_SUCCESS);
    rc = MPI_Allreduce(&local, &global, 1, MPI_LONG_LONG_INT, MPI_SUM, MPI_COMM_WORLD);
    assert(rc == MPI_SUCCESS);
    assert(global == 24768);
    rc = MPI_Barrier(MPI_COMM_WORLD);
    assert(rc == MPI_SUCCESS);
    if (rank == 0) printf("mpi_omp ok: ranks=%d threads=4 checksum=%lld\n", size, global);
    rc = MPI_Finalize();
    assert(rc == MPI_SUCCESS);
    return 0;
}
