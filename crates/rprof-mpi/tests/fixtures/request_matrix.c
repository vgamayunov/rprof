#define _POSIX_C_SOURCE 200809L
#include <mpi.h>
#include <assert.h>
#include <time.h>

static void pause_progress(void) { struct timespec time = {0, 50000}; nanosleep(&time, 0); }
int main(int argc, char **argv) {
    int rank, size, provided;
    assert(MPI_Init_thread(&argc, &argv, MPI_THREAD_SERIALIZED, &provided) == MPI_SUCCESS);
    MPI_Comm_rank(MPI_COMM_WORLD, &rank); MPI_Comm_size(MPI_COMM_WORLD, &size);
    assert(size == 2); int peer = 1 - rank;
    for (int method = 0; method < 5; method++) {
        int sent = rank + 100, got = -1;
        MPI_Request requests[2];
        MPI_Irecv(&got, 1, MPI_INT, peer, 10 + method, MPI_COMM_WORLD, &requests[0]);
        MPI_Isend(&sent, 1, MPI_INT, peer, 10 + method, MPI_COMM_WORLD, &requests[1]);
        if (method == 0) {
            int index; MPI_Waitany(2, requests, &index, MPI_STATUS_IGNORE);
            MPI_Waitall(2, requests, MPI_STATUSES_IGNORE);
        } else if (method == 1) {
            int count, indices[2]; MPI_Waitsome(2, requests, &count, indices, MPI_STATUSES_IGNORE);
            MPI_Waitall(2, requests, MPI_STATUSES_IGNORE);
        } else if (method == 2) {
            int done = 0; while (!done) { MPI_Testall(2, requests, &done, MPI_STATUSES_IGNORE); if (!done) pause_progress(); }
        } else if (method == 3) {
            int left = 2; while (left) { int index, done; MPI_Testany(2, requests, &index, &done, MPI_STATUS_IGNORE); if (done && index != MPI_UNDEFINED) left--; else pause_progress(); }
        } else {
            int left = 2; while (left) { int count, indices[2]; MPI_Testsome(2, requests, &count, indices, MPI_STATUSES_IGNORE); if (count > 0) left -= count; else pause_progress(); }
        }
        assert(got == peer + 100);
    }
    int sent = rank, got = -1, sum = -1;
    MPI_Request request;
    MPI_Iallreduce(&sent, &sum, 1, MPI_INT, MPI_SUM, MPI_COMM_WORLD, &request);
    MPI_Wait(&request, MPI_STATUS_IGNORE); assert(sum == 1);
    MPI_Ibcast(&sent, 1, MPI_INT, 0, MPI_COMM_WORLD, &request);
    MPI_Wait(&request, MPI_STATUS_IGNORE); assert(sent == 0);
    MPI_Sendrecv(&rank, 1, MPI_INT, peer, 50, &got, 1, MPI_INT, peer, 50, MPI_COMM_WORLD, MPI_STATUS_IGNORE);
    assert(got == peer);
    if (rank == 0) { MPI_Ssend(&rank, 1, MPI_INT, 1, 51, MPI_COMM_WORLD); MPI_Issend(&rank, 1, MPI_INT, 1, 52, MPI_COMM_WORLD, &request); MPI_Wait(&request, MPI_STATUS_IGNORE); }
    else { MPI_Recv(&got, 1, MPI_INT, 0, 51, MPI_COMM_WORLD, MPI_STATUS_IGNORE); MPI_Recv(&got, 1, MPI_INT, 0, 52, MPI_COMM_WORLD, MPI_STATUS_IGNORE); }
    MPI_Comm_set_errhandler(MPI_COMM_WORLD, MPI_ERRORS_RETURN);
    int values[2] = {1, 2};
    if (rank == 0) {
        MPI_Irecv(&got, 1, MPI_INT, 1, 60, MPI_COMM_WORLD, &request);
        int rc = MPI_Wait(&request, MPI_STATUS_IGNORE), error;
        MPI_Error_class(rc, &error); assert(error == MPI_ERR_TRUNCATE);
        assert(request == MPI_REQUEST_NULL);
    } else { MPI_Send(values, 2, MPI_INT, 0, 60, MPI_COMM_WORLD); }
    MPI_Barrier(MPI_COMM_WORLD);
    request = MPI_REQUEST_NULL;
    int done = 0; MPI_Test(&request, &done, MPI_STATUS_IGNORE); assert(done);
    MPI_Wait(&request, MPI_STATUS_IGNORE);
    assert(MPI_Finalize() == MPI_SUCCESS);
    return 0;
}
