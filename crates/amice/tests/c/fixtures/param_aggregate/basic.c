#include <stdio.h>
#include <stdlib.h>

__attribute__((noinline))
static long combine(int first, long second, unsigned third) {
    return (long)first * 17 + second * 5 - (long)third * 3;
}

__attribute__((noinline))
static signed char combine_narrow(signed char first, unsigned char second,
                                  signed char third) {
    return (signed char)(first + second - third);
}

int main(int argc, char **argv) {
    int value = argc > 1 ? atoi(argv[1]) : 7;
    printf("%ld %d\n", combine(value, 31, 9),
           (int)combine_narrow(5, 7, 3));
    return 0;
}
