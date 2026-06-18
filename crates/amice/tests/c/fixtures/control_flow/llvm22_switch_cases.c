#include <stdio.h>

static volatile int selector = 2;

__attribute__((noinline))
static int dispatch(int x) {
    int value = 0;

    switch (x) {
        case -7:
            value = 11;
            break;
        case 0:
            value = 13;
            break;
        case 2:
            value = 17;
            break;
        case 1000:
            value = 19;
            break;
        default:
            value = 23;
            break;
    }

    if (x > value) {
        return value + x;
    }
    return value - x;
}

int main(void) {
    int a = dispatch(selector);
    int b = dispatch(selector + 998);
    int c = dispatch(selector - 9);
    int d = dispatch(selector + 41);

    printf("%d %d %d %d\n", a, b, c, d);
    return (a == 15 && b == 1019 && c == 18 && d == 66) ? 0 : 1;
}
