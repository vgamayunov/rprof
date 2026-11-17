#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

typedef void *device_t;
typedef struct { unsigned int gpu, memory; } utilization_t;
typedef struct { unsigned long long total, free, used; } memory_t;
static unsigned int round_number;

int nvmlInit_v2(void) { return 0; }
int nvmlShutdown(void) { return 0; }
const char *nvmlErrorString(int status) {
    return status == 3 ? "Not supported" : "Mock read failure";
}
int nvmlDeviceGetCount_v2(unsigned int *count) {
    *count = getenv("RPROF_TEST_NVML_EMPTY") ? 0 : 2;
    return 0;
}
int nvmlDeviceGetHandleByIndex_v2(unsigned int index, device_t *device) {
    *device = (device_t)(uintptr_t)(index + 1);
    return 0;
}
int nvmlDeviceGetName(device_t device, char *text, unsigned int length) {
    snprintf(text, length, "Mock GPU %u", (unsigned int)(uintptr_t)device - 1);
    return 0;
}
int nvmlDeviceGetUUID(device_t device, char *text, unsigned int length) {
    snprintf(text, length, "GPU-mock-%u", (unsigned int)(uintptr_t)device - 1);
    return 0;
}
int nvmlDeviceGetUtilizationRates(device_t device, utilization_t *value) {
    if ((uintptr_t)device == 1) ++round_number;
    if ((uintptr_t)device == 2 && round_number == 3) return 3;
    value->gpu = (uintptr_t)device == 1 ? 20 : 80;
    value->memory = 0;
    return 0;
}
int nvmlDeviceGetMemoryInfo(device_t device, memory_t *value) {
    if ((uintptr_t)device == 1 && round_number == 4) return 999;
    value->total = 17179869184ULL;
    value->used = (uintptr_t)device == 1 ? 4294967297ULL : 8589934593ULL;
    value->free = value->total - value->used;
    return 0;
}
