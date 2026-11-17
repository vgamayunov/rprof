#include <cublas_v2.h>
#include <cuda_runtime.h>
#include <mkl.h>
#include <sched.h>

#include <algorithm>
#include <cerrno>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <limits>
#include <stdexcept>
#include <string>
#include <vector>

namespace {
using Clock = std::chrono::steady_clock;

struct Options {
    int size = 4096;
    int iterations = 10;
    int threads = 0;
};

int positive_integer(const char *text) {
    char *end = nullptr;
    errno = 0;
    const long value = std::strtol(text, &end, 10);
    if (errno || end == text || *end || value < 1 ||
        value > std::numeric_limits<int>::max()) {
        throw std::runtime_error(std::string("expected a positive integer: ") + text);
    }
    return static_cast<int>(value);
}

Options options(int argc, char **argv) {
    Options result;
    for (int i = 1; i < argc; ++i) {
        const char *name = argv[i];
        int *destination = nullptr;
        if (std::strcmp(name, "--size") == 0) destination = &result.size;
        else if (std::strcmp(name, "--iterations") == 0) destination = &result.iterations;
        else if (std::strcmp(name, "--threads") == 0) destination = &result.threads;
        else throw std::runtime_error(std::string("unknown option: ") + name);
        if (++i == argc) throw std::runtime_error(std::string("missing value for ") + name);
        *destination = positive_integer(argv[i]);
    }
    return result;
}

void cuda_check(cudaError_t status, const char *operation) {
    if (status != cudaSuccess) {
        throw std::runtime_error(std::string(operation) + ": " + cudaGetErrorString(status));
    }
}

void blas_check(cublasStatus_t status, const char *operation) {
    if (status != CUBLAS_STATUS_SUCCESS) {
        throw std::runtime_error(std::string(operation) + ": cuBLAS status " +
                                 std::to_string(static_cast<int>(status)));
    }
}

int available_cpus() {
    cpu_set_t mask;
    CPU_ZERO(&mask);
    if (sched_getaffinity(0, sizeof(mask), &mask) != 0) {
        throw std::runtime_error(std::string("sched_getaffinity: ") + std::strerror(errno));
    }
    const int count = CPU_COUNT(&mask);
    if (count < 1) throw std::runtime_error("CPU affinity mask is empty");
    return count;
}

double milliseconds(Clock::time_point start) {
    return std::chrono::duration<double, std::milli>(Clock::now() - start).count();
}

double validate(const std::vector<double> &c, int n, const char *phase) {
    long long sum_a = 0, sum_b = 0, sum_ab = 0;
    for (int k = 0; k < n; ++k) {
        const long long a = k % 13 - 6;
        const long long b = 4 * (k % 11 - 5);
        sum_a += a;
        sum_b += b;
        sum_ab += a * b;
    }
    double max_error = 0;
    for (int j = 0; j < n; ++j) {
        const long long b = j % 17 - 8;
        for (int i = 0; i < n; ++i) {
            const long long a = 16 * (i % 7 - 3);
            // A(i,k)=(a+a_k)/16, B(k,j)=(b_k+b)/32, so the sum is exact in FP64.
            const double expected =
                static_cast<double>(a * sum_b + a * b * n + sum_ab + b * sum_a) / 512.0;
            const double actual = c[static_cast<size_t>(j) * n + i];
            const double error = std::abs(actual - expected);
            if (!std::isfinite(actual) || error != 0.0) {
                throw std::runtime_error(std::string(phase) + " validation failed at (" +
                                         std::to_string(i) + "," + std::to_string(j) +
                                         "): expected " + std::to_string(expected) +
                                         ", got " + std::to_string(actual));
            }
            max_error = std::max(max_error, error);
        }
    }
    return max_error;
}

void run(Options config) {
    static_assert(sizeof(MKL_INT) == sizeof(int), "build against MKL LP64");
    const int cpus = available_cpus();
    if (config.threads == 0) config.threads = cpus;
    if (config.threads > cpus) {
        throw std::runtime_error("--threads exceeds the process CPU affinity count");
    }
    const int n = config.size;
    const size_t dimension = static_cast<size_t>(n);
    if (dimension > std::numeric_limits<size_t>::max() / sizeof(double) / dimension) {
        throw std::runtime_error("matrix byte size overflows size_t");
    }
    const size_t elements = dimension * dimension;
    const size_t bytes = elements * sizeof(double);
    std::printf("size=%d iterations=%d affinity_cpus=%d mkl_threads=%d matrix_bytes=%zu\n",
                n, config.iterations, cpus, config.threads, bytes);
    mkl_set_dynamic(0);
    mkl_set_num_threads(config.threads);
    if (mkl_get_max_threads() != config.threads) {
        throw std::runtime_error("MKL did not accept the requested thread count");
    }
    char version[256];
    mkl_get_version_string(version, sizeof(version));
    std::printf("MKL: %s\n", version);

    std::vector<double> a(elements), b(elements), c(elements);
    for (int j = 0; j < n; ++j) {
        for (int i = 0; i < n; ++i) {
            const size_t index = static_cast<size_t>(j) * n + i;
            a[index] = (16 * (i % 7 - 3) + (j % 13 - 6)) / 16.0;
            b[index] = (4 * (i % 11 - 5) + (j % 17 - 8)) / 32.0;
        }
    }
    const double operations = 2.0 * n * n * n * config.iterations;
    const auto cpu_gemm = [&] {
        cblas_dgemm(CblasColMajor, CblasNoTrans, CblasNoTrans,
                    n, n, n, 1.0, a.data(), n, b.data(), n, 0.0, c.data(), n);
    };
    std::puts("CPU phase: warm-up, then timed MKL DGEMMs");
    cpu_gemm();
    const auto cpu_start = Clock::now();
    for (int i = 0; i < config.iterations; ++i) cpu_gemm();
    const double cpu_ms = milliseconds(cpu_start);
    const double cpu_error = validate(c, n, "CPU");
    std::printf("CPU verified: total_ms=%.3f mean_ms=%.3f gflops=%.3f max_abs_error=%.1f\n",
                cpu_ms, cpu_ms / config.iterations, operations / (cpu_ms * 1e6), cpu_error);

    cuda_check(cudaSetDevice(0), "select GPU");
    cudaDeviceProp device{};
    cuda_check(cudaGetDeviceProperties(&device, 0), "get GPU properties");
    std::printf("GPU: %s (logical device 0), compute capability %d.%d\n",
                device.name, device.major, device.minor);
    double *device_a = nullptr, *device_b = nullptr, *device_c = nullptr;
    cuda_check(cudaMalloc(&device_a, bytes), "allocate GPU A");
    cuda_check(cudaMalloc(&device_b, bytes), "allocate GPU B");
    cuda_check(cudaMalloc(&device_c, bytes), "allocate GPU C");
    cuda_check(cudaMemcpy(device_a, a.data(), bytes, cudaMemcpyHostToDevice), "copy GPU A");
    cuda_check(cudaMemcpy(device_b, b.data(), bytes, cudaMemcpyHostToDevice), "copy GPU B");
    cublasHandle_t handle;
    blas_check(cublasCreate(&handle), "create cuBLAS handle");
    cudaEvent_t start, end;
    cuda_check(cudaEventCreate(&start), "create GPU start event");
    cuda_check(cudaEventCreate(&end), "create GPU end event");
    const double alpha = 1.0, beta = 0.0;
    const auto gpu_gemm = [&] {
        blas_check(cublasDgemm(handle, CUBLAS_OP_N, CUBLAS_OP_N,
                              n, n, n, &alpha, device_a, n, device_b, n,
                              &beta, device_c, n), "GPU DGEMM");
    };
    std::puts("GPU phase: warm-up, then timed cuBLAS DGEMMs");
    gpu_gemm();
    cuda_check(cudaDeviceSynchronize(), "finish GPU warm-up");
    const auto gpu_start = Clock::now();
    cuda_check(cudaEventRecord(start), "record GPU start");
    for (int i = 0; i < config.iterations; ++i) gpu_gemm();
    cuda_check(cudaEventRecord(end), "record GPU end");
    cuda_check(cudaEventSynchronize(end), "finish timed GPU GEMMs");
    const double gpu_ms = milliseconds(gpu_start);
    float event_ms = 0;
    cuda_check(cudaEventElapsedTime(&event_ms, start, end), "measure GPU events");
    cuda_check(cudaMemcpy(c.data(), device_c, bytes, cudaMemcpyDeviceToHost), "copy GPU C");
    const double gpu_error = validate(c, n, "GPU");
    std::printf("GPU verified: total_ms=%.3f mean_ms=%.3f gflops=%.3f event_ms=%.3f "
                "max_abs_error=%.1f\n",
                gpu_ms, gpu_ms / config.iterations, operations / (gpu_ms * 1e6),
                static_cast<double>(event_ms), gpu_error);
    cuda_check(cudaEventDestroy(start), "destroy GPU start event");
    cuda_check(cudaEventDestroy(end), "destroy GPU end event");
    blas_check(cublasDestroy(handle), "destroy cuBLAS handle");
    cuda_check(cudaFree(device_c), "free GPU C");
    cuda_check(cudaFree(device_b), "free GPU B");
    cuda_check(cudaFree(device_a), "free GPU A");
    std::puts("PASS: CPU and GPU results match the independent reference");
}
} // namespace

int main(int argc, char **argv) {
    std::setvbuf(stdout, nullptr, _IOLBF, 0);
    if (argc == 2 && std::strcmp(argv[1], "--help") == 0) {
        std::puts("Usage: gemm [--size N] [--iterations N] [--threads N]\n"
                  "Defaults: size=4096, iterations=10, threads=CPU affinity count.\n"
                  "Runs CPU MKL DGEMM, then GPU cuBLAS DGEMM; validates both.");
        return 0;
    }
    try {
        run(options(argc, argv));
        return 0;
    } catch (const std::exception &error) {
        std::fprintf(stderr, "gemm: %s\n", error.what());
        return 1;
    }
}
