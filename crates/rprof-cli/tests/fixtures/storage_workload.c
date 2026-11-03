#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/* Run against fresh paths on the filesystems under test. Direct I/O makes the
 * block/client counters observable without dropping caches or changing settings. */
int main(int argc, char **argv) {
    if (argc < 2) return 2;
    const size_t size = 65536;
    void *buffer = NULL;
    if (posix_memalign(&buffer, 4096, size)) return 3;
    sleep(2); /* Let the sampler establish a baseline. */
    for (int path = 1; path < argc; path++) {
        memset(buffer, 0x44, size);
        int fd = open(argv[path], O_CREAT | O_EXCL | O_WRONLY | O_DIRECT, 0600);
        if (fd < 0) { perror("direct open for write"); return 4; }
        for (int i = 0; i < 16; i++) if (write(fd, buffer, size) != (ssize_t)size) return 5;
        if (fsync(fd) || close(fd)) return 6;
        fd = open(argv[path], O_RDONLY | O_DIRECT);
        if (fd < 0) { perror("direct open for read"); return 7; }
        for (int i = 0; i < 16; i++) {
            memset(buffer, 0, size);
            if (read(fd, buffer, size) != (ssize_t)size) return 8;
            for (size_t n = 0; n < size; n++) if (((unsigned char *)buffer)[n] != 0x44) return 9;
        }
        if (close(fd)) return 10;
        printf("storage workload ok: %s, 1048576 written/read bytes\n", argv[path]);
    }
    sleep(2); /* Include device/client completions before sampler shutdown. */
    for (int path = 1; path < argc; path++) if (unlink(argv[path])) return 11;
    free(buffer);
    return 0;
}
