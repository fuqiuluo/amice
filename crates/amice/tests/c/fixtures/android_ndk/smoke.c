#include <stdio.h>
#include <stdlib.h>

__attribute__((noinline, annotate("+flatten,+mba")))
static unsigned transform(unsigned value) {
    if (value & 1)
        value = (value * 7) ^ 0x55;
    else
        value = (value + 11) ^ 0x33;
    return value + 17;
}

int main(int argc, char **argv) {
    unsigned value = argc > 1 ? (unsigned)strtoul(argv[1], NULL, 10) : 7;
    puts("AMICE_NDK_C_MARKER");
    printf("%u\n", transform(value));
    return 0;
}
