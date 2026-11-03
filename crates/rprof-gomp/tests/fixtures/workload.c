/* Real GCC/libgomp workload. Compile with GCC 13 -fopenmp (not Clang). */
#include <omp.h>
#include <stdint.h>
#include <stdio.h>

static long expected_sum(int n) { return (long)n * (n - 1) / 2; }
int main(void) {
  enum { N = 2048 };
  long sums[4] = {0};
  omp_set_dynamic(0);
  omp_set_num_threads(4);
  #pragma omp parallel for schedule(static) reduction(+:sums[0])
  for (int i=0;i<N;i++) sums[0]+=i;
  #pragma omp parallel for schedule(dynamic,7) reduction(+:sums[1])
  for (int i=0;i<N;i++) sums[1]+=i;
  #pragma omp parallel for schedule(guided,11) reduction(+:sums[2])
  for (int i=0;i<N;i++) sums[2]+=i;
  omp_set_schedule(omp_sched_dynamic,13);
  #pragma omp parallel for schedule(runtime) reduction(+:sums[3])
  for (int i=0;i<N;i++) sums[3]+=i;
  for (int i=0;i<4;i++) if (sums[i]!=expected_sum(N)) return 10+i;

  omp_lock_t lock;
  omp_init_lock(&lock);
  long acc=0;
  int only=0, sections=0, nested=0;
  #pragma omp parallel num_threads(3) shared(acc,only,sections,nested)
  {
    #pragma omp for schedule(dynamic,5)
    for (int i=0;i<N;i++) {
      omp_set_lock(&lock);
      acc+=i;
      omp_unset_lock(&lock);
    }
    #pragma omp sections
    {
      #pragma omp section
      {
        #pragma omp atomic update
        sections+=1;
      }
      #pragma omp section
      {
        #pragma omp atomic update
        sections+=2;
      }
    }
    #pragma omp single
    {
      only++;
      #pragma omp parallel num_threads(2) reduction(+:nested)
      { nested++; }
    }
    #pragma omp barrier
  }
  omp_destroy_lock(&lock);
  if(acc!=expected_sum(N) || only!=1 || sections!=3 || nested<1 || nested>2) return 20;

  /* The undeferred child executes a nested worksharing loop while the
   * parent's dynamic-loop chunk remains open. Its work frame must not
   * overwrite or close the parent's frame when the task returns. */
  int nested_task_sum=0;
  #pragma omp parallel num_threads(2) shared(nested_task_sum)
  {
    #pragma omp for schedule(dynamic,4)
    for(int i=0;i<24;i++) {
      #pragma omp task if(0) firstprivate(i) shared(nested_task_sum)
      {
        int inner=0;
        #pragma omp parallel num_threads(1) reduction(+:inner)
        {
          #pragma omp for schedule(dynamic,1)
          for(int j=0;j<3;j++) inner+=i+j;
        }
        #pragma omp atomic update
        nested_task_sum+=inner;
      }
    }
  }
  if(nested_task_sum!=900) return 21;

  int deps[16]={0}, order=0, total=0;
  #pragma omp parallel num_threads(4)
  #pragma omp single
  {
    #pragma omp taskgroup
    {
      for(int i=0;i<16;i++) {
        #pragma omp task firstprivate(i) depend(out:deps[i]) priority(i % 3) shared(deps)
        deps[i]=i+1;
        #pragma omp task firstprivate(i) depend(in:deps[i]) shared(deps,order)
        {
          if(deps[i]!=i+1) {
            #pragma omp atomic update
            order++;
          }
        }
      }
      #pragma omp task if(0) final(1) shared(total)
      total+=5;
    }
    #pragma omp taskwait
  }
  for(int i=0;i<16;i++) total+=deps[i];
  if(total!=141 || order!=0) return 30;
  printf("workload ok %ld %ld %ld %ld\n",sums[0],sums[1],sums[2],sums[3]);
  return 0;
}
