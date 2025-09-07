#include <stdio.h>

int __attribute__((annotate("+vmp"))) main(void) {
    int c[3];

    int a = 10;
    int b = 20;

    return a + b + c[0];
}