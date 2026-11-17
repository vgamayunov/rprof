#define _POSIX_C_SOURCE 200112L

#include <errno.h>
#include <inttypes.h>
#include <limits.h>
#include <omp.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

enum { DEFAULT_ITERATIONS = 10, DEFAULT_THREADS = 0 };
static const double STREAM_SCALAR = 3.0;
static double *stream_a;
static double *stream_b;
static double *stream_c;

struct options {
    size_t elements;
    int iterations;
    int threads;
};

static void usage(FILE *stream) {
    fprintf(stream,
            "Usage: stream [--elements N] [--iterations N] [--threads N]\n"
            "  --elements N    elements per array (default: 16777216)\n"
            "  --iterations N  timed repetitions per kernel (default: 10)\n"
            "  --threads N     OpenMP threads (default: runtime maximum)\n"
            "Runs and validates Copy, Scale, Add, and Triad kernels.\n");
}

static uint64_t positive_integer(const char *text, const char *name) {
    char *end = NULL;
    errno = 0;
    const unsigned long long value = strtoull(text, &end, 10);
    if (errno || end == text || *end || text[0] == '-' || value == 0) {
        fprintf(stderr, "stream: %s must be a positive integer: %s\n", name, text);
        exit(EXIT_FAILURE);
    }
    return (uint64_t)value;
}

static struct options parse_options(int argc, char **argv) {
    struct options result = {
        .elements = 16777216,
        .iterations = DEFAULT_ITERATIONS,
        .threads = DEFAULT_THREADS,
    };
    for (int i = 1; i < argc; ++i) {
        const char *name = argv[i];
        uint64_t value;
        if (strcmp(name, "--help") == 0) {
            usage(stdout);
            exit(EXIT_SUCCESS);
        }
        if (strcmp(name, "--elements") != 0 && strcmp(name, "--iterations") != 0 &&
            strcmp(name, "--threads") != 0) {
            fprintf(stderr, "stream: unknown option: %s\n", name);
            usage(stderr);
            exit(EXIT_FAILURE);
        }
        if (i + 1 == argc) {
            fprintf(stderr, "stream: missing value for %s\n", name);
            exit(EXIT_FAILURE);
        }
        value = positive_integer(argv[++i], name);
        if (strcmp(name, "--elements") == 0) {
            if (value > SIZE_MAX / sizeof(double) || value > LLONG_MAX) {
                fprintf(stderr, "stream: element count is too large\n");
                exit(EXIT_FAILURE);
            }
            result.elements = (size_t)value;
        } else if (strcmp(name, "--iterations") == 0) {
            if (value > INT_MAX) {
                fprintf(stderr, "stream: iteration count is too large\n");
                exit(EXIT_FAILURE);
            }
            result.iterations = (int)value;
        } else if (strcmp(name, "--threads") == 0) {
            if (value > INT_MAX) {
                fprintf(stderr, "stream: thread count is too large\n");
                exit(EXIT_FAILURE);
            }
            result.threads = (int)value;
        } else {
            fprintf(stderr, "stream: unknown option: %s\n", name);
            usage(stderr);
            exit(EXIT_FAILURE);
        }
    }
    return result;
}

static double *allocate_array(size_t elements) {
    void *array = NULL;
    const int status = posix_memalign(&array, 64, elements * sizeof(double));
    if (status != 0) {
        fprintf(stderr, "stream: unable to allocate array: %s\n", strerror(status));
        exit(EXIT_FAILURE);
    }
    return array;
}

static double benchmark(const char *name, const char *units, size_t elements,
                        int iterations, size_t bytes_per_element,
                        void (*kernel)(double *, double *, double *, size_t)) {
    kernel(stream_a, stream_b, stream_c, elements);
    const double start = omp_get_wtime();
    for (int iteration = 0; iteration < iterations; ++iteration) {
        kernel(stream_a, stream_b, stream_c, elements);
    }
    const double elapsed = omp_get_wtime() - start;
    const double seconds_per_iteration = elapsed / iterations;
    const double bandwidth =
        ((double)bytes_per_element * (double)elements * (double)iterations) / elapsed / 1e9;
    printf("%-5s %10.3f ms/iteration %10.3f GB/s (%s)\n",
           name, seconds_per_iteration * 1000.0, bandwidth, units);
    return seconds_per_iteration;
}

static void copy_kernel(double *a, double *b, double *c, size_t n) {
    (void)c;
#pragma omp parallel for schedule(static)
    for (long long i = 0; i < (long long)n; ++i) a[i] = b[i];
}

static void scale_kernel(double *a, double *b, double *c, size_t n) {
    (void)a;
#pragma omp parallel for schedule(static)
    for (long long i = 0; i < (long long)n; ++i) b[i] = STREAM_SCALAR * c[i];
}

static void add_kernel(double *a, double *b, double *c, size_t n) {
#pragma omp parallel for schedule(static)
    for (long long i = 0; i < (long long)n; ++i) c[i] = a[i] + b[i];
}

static void triad_kernel(double *a, double *b, double *c, size_t n) {
#pragma omp parallel for schedule(static)
    for (long long i = 0; i < (long long)n; ++i) a[i] = b[i] + STREAM_SCALAR * c[i];
}

static int validate(const char *name, const double *actual, size_t elements,
                    double (*expected)(size_t)) {
    size_t errors = 0;
#pragma omp parallel for reduction(+ : errors) schedule(static)
    for (long long i = 0; i < (long long)elements; ++i) {
        if (actual[i] != expected((size_t)i)) ++errors;
    }
    if (errors != 0) {
        fprintf(stderr, "stream: %s validation failed at %" PRIu64 " element(s)\n",
                name, (uint64_t)errors);
        return 0;
    }
    return 1;
}

static double expected_copy(size_t i) { return (double)(i % 64) / 8.0; }
static double expected_scale(size_t i) { return STREAM_SCALAR * (double)(i % 32) / 4.0; }
static double expected_add(size_t i) {
    return (double)(i % 128) / 16.0 + (double)(i % 64) / 8.0;
}
static double expected_triad(size_t i) {
    return (double)(i % 64) / 8.0 + STREAM_SCALAR * (double)(i % 32) / 4.0;
}

static void initialize_arrays(size_t elements) {
#pragma omp parallel for schedule(static)
    for (long long i = 0; i < (long long)elements; ++i) {
        stream_a[i] = (double)(i % 128) / 16.0;
        stream_b[i] = (double)(i % 64) / 8.0;
        stream_c[i] = (double)(i % 32) / 4.0;
    }
}

int main(int argc, char **argv) {
    const struct options config = parse_options(argc, argv);
    if (config.threads > 0) omp_set_num_threads(config.threads);
    stream_a = allocate_array(config.elements);
    stream_b = allocate_array(config.elements);
    stream_c = allocate_array(config.elements);

    printf("elements=%zu array_bytes=%zu iterations=%d threads=%d scalar=%.1f\n",
           config.elements, config.elements * sizeof(double), config.iterations,
           omp_get_max_threads(), STREAM_SCALAR);
    int valid = 1;
    initialize_arrays(config.elements);
    benchmark("Copy", "2 * sizeof(double)", config.elements, config.iterations, 2 * sizeof(double),
              copy_kernel);
    valid &= validate("Copy", stream_a, config.elements, expected_copy);
    initialize_arrays(config.elements);
    benchmark("Scale", "2 * sizeof(double)", config.elements, config.iterations, 2 * sizeof(double),
              scale_kernel);
    valid &= validate("Scale", stream_b, config.elements, expected_scale);
    initialize_arrays(config.elements);
    benchmark("Add", "3 * sizeof(double)", config.elements, config.iterations, 3 * sizeof(double),
              add_kernel);
    valid &= validate("Add", stream_c, config.elements, expected_add);
    initialize_arrays(config.elements);
    benchmark("Triad", "3 * sizeof(double)", config.elements, config.iterations, 3 * sizeof(double),
              triad_kernel);
    valid &= validate("Triad", stream_a, config.elements, expected_triad);
    free(stream_c);
    free(stream_b);
    free(stream_a);
    puts(valid ? "PASS: all STREAM kernels validated" : "FAIL: STREAM validation failed");
    return valid ? EXIT_SUCCESS : EXIT_FAILURE;
}
