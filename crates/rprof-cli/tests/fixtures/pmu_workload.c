#define _GNU_SOURCE
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

static void *work(void *unused) {
    (void)unused;
    const size_t bytes = 4096u * 4096u;
    volatile unsigned char *memory = mmap(NULL, bytes, PROT_READ | PROT_WRITE,
                                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (memory == MAP_FAILED) _exit(2);
    // Avoid transparent-huge-page allocation making the fault count ambiguous.
    if (madvise((void *)memory, bytes, MADV_NOHUGEPAGE)) _exit(3);
    for (size_t offset = 0; offset < bytes; offset += 4096) memory[offset] = 1;
    volatile uint64_t value = 1;
    for (unsigned i = 0; i < 120000000; ++i) value = value * 6364136223846793005ull + 1;
    if (!value) _exit(4);
    munmap((void *)memory, bytes);
    return NULL;
}
int main(int argc, char **argv) {
    if (argc == 1) work(NULL);
    else if (!strcmp(argv[1], "threads")) {
        pthread_t threads[2];
        for (int i = 0; i < 2; ++i) if (pthread_create(&threads[i], NULL, work, NULL)) return 5;
        for (int i = 0; i < 2; ++i) if (pthread_join(threads[i], NULL)) return 6;
    } else if (!strcmp(argv[1], "fork")) {
        pid_t pid = fork();
        if (pid < 0) return 7;
        if (!pid) { work(NULL); _exit(0); }
        int status = 0;
        if (waitpid(pid, &status, 0) < 0 || !WIFEXITED(status) || WEXITSTATUS(status)) return 8;
    } else return 9;
    puts("pmu workload verified");
    return 0;
}
