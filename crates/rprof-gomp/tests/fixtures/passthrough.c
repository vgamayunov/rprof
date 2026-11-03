/* Unsupported bodies must still run against the original runtime data. */
#include <omp.h>
#include <stdio.h>

int main(void) {
  int sum=0, untied=0, detached=0;
  omp_event_handle_t event;
  omp_set_dynamic(0);
  #pragma omp parallel num_threads(2)
  #pragma omp single
  {
    #pragma omp taskloop grainsize(3) reduction(+:sum)
    for (int i=0;i<60;i++) sum+=i;
    #pragma omp task untied shared(untied)
    {
      #pragma omp atomic update
      untied++;
    }
    #pragma omp task detach(event) shared(detached)
    {
      #pragma omp atomic update
      detached++;
      omp_fulfill_event(event);
    }
    #pragma omp taskwait
  }
  if(sum!=1770 || untied!=1 || detached!=1) return 1;
  puts("passthrough ok");
  return 0;
}
