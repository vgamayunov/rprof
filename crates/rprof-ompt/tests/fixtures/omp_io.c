#include <assert.h>
#include <fcntl.h>
#include <omp.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
int main(int argc,char **argv){
    assert(argc==2);omp_set_dynamic(0);
    #pragma omp parallel num_threads(4)
    {
        char path[4096],bytes[16]={0};
        snprintf(path,sizeof(path),"%s/thread-%d",argv[1],omp_get_thread_num());
        int fd=open(path,O_CREAT|O_TRUNC|O_RDWR,0600);assert(fd>=0);
        assert(write(fd,"openmp-io",9)==9);assert(pread(fd,bytes,9,0)==9);assert(!memcmp(bytes,"openmp-io",9));assert(close(fd)==0);
        #pragma omp barrier
    }
    return 0;
}
