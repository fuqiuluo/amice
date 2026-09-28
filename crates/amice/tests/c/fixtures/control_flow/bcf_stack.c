#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#if defined(__linux__)
#include <sys/resource.h>
#endif

// Five MiB fits the eight MiB stack used below; two copies do not.
__attribute__((noinline)) uint32_t bcf_large_stack(uint32_t x) {
    volatile unsigned char bytes[5 * 1024 * 1024];
    for (unsigned i = 0; i < sizeof(bytes); i += 4096)
        bytes[i] = (unsigned char)(x + i);
    return bytes[0];
}

int main(void) {
#if defined(__linux__)
    struct rlimit limit = {8 * 1024 * 1024, 8 * 1024 * 1024};
    if (setrlimit(RLIMIT_STACK, &limit) != 0) return EXIT_FAILURE;
#endif
    uint32_t result = bcf_large_stack(42);
    if (result != 42) return EXIT_FAILURE;
    puts("PASS bounded stack");
}
