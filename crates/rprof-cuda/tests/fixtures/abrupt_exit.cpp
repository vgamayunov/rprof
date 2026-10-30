// Initialized CUDA followed by _Exit deliberately bypasses the CUPTI exit flush.
#include <cuda.h>
#include <cstdlib>
int main() {
  if (cuInit(0) != CUDA_SUCCESS) return 2;
  std::_Exit(0);
}
