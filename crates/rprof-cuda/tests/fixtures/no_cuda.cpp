// Control fixture: linked/instrumented like a CUDA program, but deliberately
// makes no CUDA API call. Injection must not add work at process startup.
#include <cstdio>

int main() {
  std::puts("no CUDA initialization");
  return 0;
}
