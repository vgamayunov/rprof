#include <mpi.h>
#include <pthread.h>
#include <assert.h>
#include <stdio.h>

enum { WORKERS = 2, ITERATIONS = 64 };

struct worker_args { int rank, peer, thread; };

static void *worker(void *opaque) {
    const struct worker_args *args = opaque;
    const int tag = 100 + args->thread; /* Rank-matched threads never cross-match. */
    for (int i = 0; i < ITERATIONS; ++i) {
        int sent = args->rank * 10000 + args->thread * 100 + i;
        int received = -1;
        MPI_Request requests[2];
        assert(MPI_Irecv(&received, 1, MPI_INT, args->peer, tag,
                         MPI_COMM_WORLD, &requests[0]) == MPI_SUCCESS);
        assert(MPI_Isend(&sent, 1, MPI_INT, args->peer, tag,
                         MPI_COMM_WORLD, &requests[1]) == MPI_SUCCESS);
        assert(MPI_Waitall(2, requests, MPI_STATUSES_IGNORE) == MPI_SUCCESS);
        assert(received == args->peer * 10000 + args->thread * 100 + i);
    }
    return NULL;
}

int main(int argc, char **argv) {
    int provided = -1, rank = -1, size = -1;
    assert(MPI_Init_thread(&argc, &argv, MPI_THREAD_MULTIPLE, &provided) == MPI_SUCCESS);
    MPI_Comm_rank(MPI_COMM_WORLD, &rank);
    MPI_Comm_size(MPI_COMM_WORLD, &size);
    if (size != 2 || provided < MPI_THREAD_MULTIPLE) {
        if (rank == 0) fprintf(stderr, "requires two ranks and MPI_THREAD_MULTIPLE\n");
        MPI_Finalize();
        return 77;
    }
    pthread_t threads[WORKERS];
    struct worker_args args[WORKERS];
    for (int t = 0; t < WORKERS; ++t) {
        args[t] = (struct worker_args){rank, 1 - rank, t};
        assert(pthread_create(&threads[t], NULL, worker, &args[t]) == 0);
    }
    for (int t = 0; t < WORKERS; ++t) assert(pthread_join(threads[t], NULL) == 0);
    assert(MPI_Finalize() == MPI_SUCCESS);
    return 0;
}
