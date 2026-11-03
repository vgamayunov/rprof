#include <omp.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#ifdef RPROF_MPI
#include <mpi.h>
#endif

int main(int argc,char **argv) {
  int rank=0, size=1;
#ifdef RPROF_MPI
  int provided=0;
  if(MPI_Init_thread(&argc,&argv,MPI_THREAD_FUNNELED,&provided)!=MPI_SUCCESS || provided<MPI_THREAD_FUNNELED) return 2;
  MPI_Comm_rank(MPI_COMM_WORLD,&rank);
  MPI_Comm_size(MPI_COMM_WORLD,&size);
#endif
  int threads=argc>1?atoi(argv[1]):4;
  int regions=argc>2?atoi(argv[2]):20;
  if(threads<1 || threads>512 || regions<1 || regions>20000) return 3;
  omp_set_dynamic(0);
  long long total=0;
  for(int iteration=0;iteration<regions;iteration++) {
    long long value=0;
    #pragma omp parallel num_threads(threads) reduction(+:value)
    {
      volatile uint64_t state=(uint64_t)omp_get_thread_num()+1;
      for(int j=0;j<10000;j++) state=state*6364136223846793005ull+1;
      if(state==0) abort();
      usleep(1000); /* Controlled low-rate body: this is not a throughput benchmark. */
      value+=omp_get_thread_num()+1;
    }
    if(value!=(long long)threads*(threads+1)/2) return 4;
#ifdef RPROF_MPI
    long long global=0;
    MPI_Allreduce(&value,&global,1,MPI_LONG_LONG,MPI_SUM,MPI_COMM_WORLD);
    if(global!=value*size) MPI_Abort(MPI_COMM_WORLD,5);
#endif
    total+=value;
  }
  printf("capacity ok rank=%d threads=%d regions=%d checksum=%lld\n",rank,threads,regions,total);
#ifdef RPROF_MPI
  MPI_Finalize();
#endif
  return 0;
}
