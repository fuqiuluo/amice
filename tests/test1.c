#include <stdio.h>

int __attribute__((annotate("+vmp"))) main(void) {
    int c[3];
    int d[3][3];

    int a = 10;
    int b = 20;

    printf("%d\n", a + b);

    c[0] = 10;
    d[1][2] = 20;

    return a + b + c[0] + d[1][2];
}