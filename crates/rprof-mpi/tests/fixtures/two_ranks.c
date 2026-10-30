#include <mpi.h>
#include <assert.h>
#include <stdio.h>

/* Run with exactly two ranks; exercises the PMPI wrapper surfaces without profiling hooks. */
int main(int argc, char **argv) {
    int provided, rank, size, peer, x = 0, y = 0, sum = 0;
    assert(MPI_Init_thread(&argc, &argv, MPI_THREAD_FUNNELED, &provided) == MPI_SUCCESS);
    MPI_Comm_rank(MPI_COMM_WORLD, &rank);
    MPI_Comm_size(MPI_COMM_WORLD, &size);
    if (size != 2) { if (rank == 0) fprintf(stderr, "requires two ranks\n"); MPI_Abort(MPI_COMM_WORLD, 1); }
    assert(provided >= MPI_THREAD_FUNNELED);
    peer = 1 - rank;
    MPI_Comm duplicate, split;
    MPI_Comm_dup(MPI_COMM_WORLD, &duplicate);
    MPI_Comm_split(duplicate, 0, 1 - rank, &split);
    MPI_Barrier(split);
    if (rank == 0) MPI_Send(&rank, 1, MPI_INT, peer, 3, duplicate);
    else { MPI_Status s; MPI_Recv(&x, 1, MPI_INT, MPI_ANY_SOURCE, MPI_ANY_TAG, duplicate, &s); assert(s.MPI_SOURCE == 0 && s.MPI_TAG == 3); }
    MPI_Request req[2];
    x = rank;
    MPI_Irecv(&y, 1, MPI_INT, peer, 4, duplicate, &req[0]);
    int flag = 1;
    MPI_Test(&req[0], &flag, MPI_STATUS_IGNORE);
    if (rank == 0) assert(!flag); /* peer cannot send before barrier below */
    MPI_Barrier(duplicate);
    MPI_Isend(&x, 1, MPI_INT, peer, 4, duplicate, &req[1]);
    MPI_Waitall(2, req, MPI_STATUSES_IGNORE);
    assert(y == peer);
    MPI_Ibarrier(duplicate, &req[0]); MPI_Wait(&req[0], MPI_STATUS_IGNORE);
    MPI_Bcast(&x, 1, MPI_INT, 0, duplicate);
    MPI_Allreduce(&rank, &sum, 1, MPI_INT, MPI_SUM, duplicate);
    assert(sum == 1 && x == 0);
    /* A fixed-source receive with a larger posted capacity than the message. */
    if (rank == 0) {
        int short_message[4] = {-1, -1, -1, -1};
        MPI_Irecv(short_message, 4, MPI_INT, 1, 21, duplicate, &req[0]);
        MPI_Barrier(duplicate);
        MPI_Wait(&req[0], MPI_STATUS_IGNORE);
        assert(short_message[0] == 71 && short_message[1] == -1);
    } else {
        MPI_Barrier(duplicate);
        int value = 71;
        MPI_Send(&value, 1, MPI_INT, 0, 21, duplicate);
    }
    int gathered[2] = {-1, -1};
    gathered[rank] = rank + 10;
    MPI_Allgather(MPI_IN_PLACE, 0, MPI_DATATYPE_NULL, gathered, 1, MPI_INT, duplicate);
    assert(gathered[0] == 10 && gathered[1] == 11);
    int root_data[2] = {23, 24}, scattered = -1;
    MPI_Scatter(root_data, 1, MPI_INT, rank == 0 ? MPI_IN_PLACE : &scattered,
                1, MPI_INT, 0, duplicate);
    if (rank == 1) assert(scattered == 24);
    int to_root = 30 + rank, on_root[2] = {30, -1};
    MPI_Gather(rank == 0 ? MPI_IN_PLACE : &to_root, 1, MPI_INT,
               on_root, 1, MPI_INT, 0, duplicate);
    if (rank == 0) assert(on_root[0] == 30 && on_root[1] == 31);
    int exchange[2] = {10 * rank, 10 * rank + 1};
    MPI_Alltoall(MPI_IN_PLACE, 0, MPI_DATATYPE_NULL, exchange, 1, MPI_INT, duplicate);
    assert(exchange[0] == rank && exchange[1] == rank + 10);
    /* No matching send for this receive: successful cancellation is deterministic. */
    if (rank == 0) {
        MPI_Status cancelled_status;
        int cancelled = 0;
        MPI_Irecv(&cancelled, 1, MPI_INT, 1, 91, duplicate, &req[0]);
        MPI_Cancel(&req[0]);
        MPI_Wait(&req[0], &cancelled_status);
        int was_cancelled = 0;
        MPI_Test_cancelled(&cancelled_status, &was_cancelled);
        assert(was_cancelled);
    }
    MPI_Barrier(duplicate);
    if (rank == 0) {
        x = 81;
        MPI_Isend(&x, 1, MPI_INT, 1, 92, duplicate, &req[0]);
        MPI_Request_free(&req[0]);
    } else {
        int detached_value = -1;
        MPI_Recv(&detached_value, 1, MPI_INT, 0, 92, duplicate, MPI_STATUS_IGNORE);
        assert(detached_value == 81);
    }
    MPI_Barrier(duplicate); /* sender's stack buffer remains live until receiver completes */
    MPI_Comm_free(&split);
    MPI_Comm_free(&duplicate);
    MPI_Finalize();
    return 0;
}
