// Best-effort ownership control: claim CUPTI's single subscriber slot before
// invoking the CUDA driver. With CUDA_INJECTION64_PATH set, the adapter should
// emit a subscriber-conflict diagnostic rather than displace this subscriber.
// No GPU work, allocations, or CUDA synchronization are performed here.
#include <cuda.h>
#include <cupti.h>
#include <cupti_callbacks.h>
#include <cstdio>

static void CUPTIAPI callback(void *, CUpti_CallbackDomain, CUpti_CallbackId,
                              const void *) {}

int main() {
  CUpti_SubscriberHandle owner = nullptr;
  CUpti_SubscriberParams params{};
  params.structSize = CUpti_SubscriberParams_STRUCT_SIZE;
  params.subscriberName = "rprof-test-existing-subscriber";
  CUptiResult claimed = cuptiSubscribe_v2(&owner, callback, nullptr, &params);
  if (claimed != CUPTI_SUCCESS) {
    std::fprintf(stderr, "test subscriber unavailable: CUPTI result=%d\n", claimed);
    return 2;
  }
  CUresult initialized = cuInit(0);
  (void)cuptiUnsubscribe(owner);
  if (initialized != CUDA_SUCCESS) {
    std::fprintf(stderr, "cuInit result=%d\n", initialized);
    return 1;
  }
  std::puts("existing CUPTI subscriber retained ownership");
  return 0;
}
