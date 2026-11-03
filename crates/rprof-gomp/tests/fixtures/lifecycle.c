/* One-thread regions avoid exercising libgomp's unsupported live-pool fork. */
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

extern void GOMP_parallel(void (*)(void *), void *, unsigned, unsigned);
static void body(void *data) {
  int *value=data;
  if (errno!=EDOM) exit(10);
  *value+=1;
  errno=ERANGE;
}
static void run(void) {
  int value=0;
  errno=EDOM;
  GOMP_parallel(body,&value,1,0);
  if (errno!=ERANGE || value!=1) exit(11);
}
int main(int argc,char **argv) {
  (void)argv;
  if (argc>1) run();
  pid_t child=fork();
  if (child<0) return 2;
  if (child==0) { run(); exit(0); }
  int status=0;
  if (waitpid(child,&status,0)!=child || !WIFEXITED(status) || WEXITSTATUS(status)) return 3;
  run();
  puts("lifecycle ok");
  return 0;
}
