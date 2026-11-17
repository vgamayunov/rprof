#include <mpi.h>

#include <errno.h>
#include <limits.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int world_rank = -1;

static _Noreturn void fail(const char *message) {
    fprintf(stderr, "rank %d: %s\n", world_rank, message);
    MPI_Abort(MPI_COMM_WORLD, EXIT_FAILURE);
    exit(EXIT_FAILURE);
}

static void mpi_check(int status, const char *operation) {
    if (status == MPI_SUCCESS) return;
    char message[MPI_MAX_ERROR_STRING];
    int length = 0;
    if (MPI_Error_string(status, message, &length) == MPI_SUCCESS) {
        fprintf(stderr, "rank %d: %s: %.*s\n", world_rank, operation, length, message);
    } else {
        fprintf(stderr, "rank %d: %s failed with MPI status %d\n", world_rank, operation, status);
    }
    fail("MPI operation failed");
}

#define MPI_CHECK(call) mpi_check((call), #call)

static int finish(int result) {
    int status = MPI_Finalize();
    if (status != MPI_SUCCESS) {
        fprintf(stderr, "rank %d: MPI_Finalize failed with status %d\n", world_rank, status);
        return EXIT_FAILURE;
    }
    return result;
}

static int positive_integer(const char *text) {
    char *end = NULL;
    errno = 0;
    long value = strtol(text, &end, 10);
    if (errno || end == text || *end || value < 1 || value > INT_MAX) {
        fail("count and iterations must be positive integers no larger than INT_MAX");
    }
    return (int)value;
}

static void usage(FILE *stream) {
    fprintf(stream, "Usage: allreduce [--count N] [--iterations N]\n"
                    "  --count N       MPI_DOUBLE elements per rank (default: 1048576)\n"
                    "  --iterations N  timed MPI_Allreduce calls (default: 32)\n"
                    "Runs one warm-up; validates every output element on every rank.\n");
}

int main(int argc, char **argv) {
    _Static_assert(sizeof(double) == 8, "this mini-app requires FP64 double");
    if (MPI_Init(&argc, &argv) != MPI_SUCCESS) {
        fprintf(stderr, "MPI_Init failed\n");
        return EXIT_FAILURE;
    }
    MPI_CHECK(MPI_Comm_set_errhandler(MPI_COMM_WORLD, MPI_ERRORS_RETURN));
    int world_size = 0;
    MPI_CHECK(MPI_Comm_rank(MPI_COMM_WORLD, &world_rank));
    MPI_CHECK(MPI_Comm_size(MPI_COMM_WORLD, &world_size));

    int count = 1048576;
    int iterations = 32;
    for (int i = 1; i < argc; ++i) {
        int *option = NULL;
        if (strcmp(argv[i], "--help") == 0) {
            if (world_rank == 0) usage(stdout);
            return finish(EXIT_SUCCESS);
        }
        if (strcmp(argv[i], "--count") == 0) option = &count;
        else if (strcmp(argv[i], "--iterations") == 0) option = &iterations;
        else {
            if (world_rank == 0) usage(stderr);
            fail("unknown argument");
        }
        if (++i == argc) fail("missing option value");
        *option = positive_integer(argv[i]);
    }
    if ((size_t)count > SIZE_MAX / sizeof(double)) fail("buffer size overflows size_t");
    const size_t bytes = (size_t)count * sizeof(double);
    double *input = malloc(bytes);
    double *output = malloc(bytes);
    if (!input || !output) fail("unable to allocate Allreduce buffers");
    for (int i = 0; i < count; ++i) {
        input[i] = (double)(world_rank % 31 + 1 + i % 1024);
        output[i] = NAN;
    }
    char host[MPI_MAX_PROCESSOR_NAME];
    int host_length = 0;
    MPI_CHECK(MPI_Get_processor_name(host, &host_length));
    printf("rank=%d/%d host=%.*s count=%d bytes=%zu iterations=%d\n",
           world_rank, world_size, host_length, host, count, bytes, iterations);
    if (world_rank == 0) {
        char version[MPI_MAX_LIBRARY_VERSION_STRING];
        int length = 0;
        MPI_CHECK(MPI_Get_library_version(version, &length));
        printf("MPI library: %.*s\n", length, version);
    }
    fflush(stdout);

    MPI_CHECK(MPI_Allreduce(input, output, count, MPI_DOUBLE, MPI_SUM, MPI_COMM_WORLD));
    MPI_CHECK(MPI_Barrier(MPI_COMM_WORLD));
    double start = MPI_Wtime();
    for (int iteration = 0; iteration < iterations; ++iteration) {
        MPI_CHECK(MPI_Allreduce(input, output, count, MPI_DOUBLE, MPI_SUM, MPI_COMM_WORLD));
    }
    double elapsed = MPI_Wtime() - start;

    // Bounded rank contributions keep every intermediate sum exactly representable.
    uint64_t ranks = (uint64_t)world_size;
    uint64_t remainder = ranks % 31;
    uint64_t rank_sum = (ranks / 31) * (31 * 32 / 2) + remainder * (remainder + 1) / 2;
    int mismatches = 0;
    for (int i = 0; i < count; ++i) {
        double expected = (double)(rank_sum + ranks * (uint64_t)(i % 1024));
        if (output[i] != expected) {
            if (mismatches == 0) {
                fprintf(stderr, "rank %d: validation failed at element %d: expected %.17g, got %.17g\n",
                        world_rank, i, expected, output[i]);
            }
            ++mismatches;
        }
    }
    int global_mismatches = 0;
    MPI_CHECK(MPI_Allreduce(&mismatches, &global_mismatches, 1, MPI_INT, MPI_MAX, MPI_COMM_WORLD));
    double slowest = 0.0;
    MPI_CHECK(MPI_Reduce(&elapsed, &slowest, 1, MPI_DOUBLE, MPI_MAX, 0, MPI_COMM_WORLD));
    printf("rank=%d validation=%s total_ms=%.3f mean_ms=%.3f\n",
           world_rank, mismatches ? "FAIL" : "PASS", elapsed * 1000.0, elapsed * 1000.0 / iterations);
    if (world_rank == 0) {
        printf("%s: MPI_Allreduce SUM verified on %d ranks; count=%d warmups=1 iterations=%d\n",
               global_mismatches ? "FAIL" : "PASS", world_size, count, iterations);
        printf("max_rank_total_ms=%.3f max_rank_mean_ms=%.3f\n",
               slowest * 1000.0, slowest * 1000.0 / iterations);
        if (slowest > 0.0) {
            printf("logical_payload_gib_s=%.3f (payload per call / max-rank mean time; not wire bandwidth)\n",
                   ((double)bytes / (1024.0 * 1024.0 * 1024.0)) * iterations / slowest);
        }
    }
    free(output);
    free(input);
    return finish(global_mismatches ? EXIT_FAILURE : EXIT_SUCCESS);
}
