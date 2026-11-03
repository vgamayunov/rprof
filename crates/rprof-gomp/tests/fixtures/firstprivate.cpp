/* Nontrivial C++ firstprivate copy callback and 64-byte-aligned task data. */
#include <atomic>
#include <cstdint>
#include <cstdio>
#include <omp.h>

static std::atomic<int> copies{0}, destructors{0}, errors{0}, completed{0};
struct alignas(64) Payload {
  int value;
  int *owner;
  Payload(int v, int *p) : value(v), owner(p) {}
  Payload(const Payload &p) : value(p.value), owner(p.owner) { ++copies; }
  ~Payload() { ++destructors; }
};
int main() {
  int proof[48] = {};
  omp_set_dynamic(0);
  #pragma omp parallel num_threads(4)
  #pragma omp single
  {
    #pragma omp taskgroup
    {
      for (int i=0;i<48;i++) {
        Payload p(i+1, &proof[i]);
        #pragma omp task if(p.value % 2) firstprivate(p)
        {
          if (reinterpret_cast<uintptr_t>(&p)%alignof(Payload) || p.owner!=&proof[p.value-1])
            ++errors;
          *p.owner=p.value;
          ++completed;
        }
      }
    }
  }
  for(int i=0;i<48;i++) if(proof[i]!=i+1) return 1;
  if(completed!=48 || errors!=0 || copies<48 || destructors<copies) return 2;
  std::printf("firstprivate ok copies=%d destructors=%d\n",int(copies.load()),int(destructors.load()));
}
