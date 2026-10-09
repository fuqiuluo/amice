#include <stdio.h>

static volatile int selector = 2;

// Branch-heavy counterpart of llvm22_switch_cases.c: nested if/else chains,
// loops with unconditional back-edges, and short-circuit operators, so both
// conditional-branch successor directions and unconditional branches are
// exercised through the obfuscation passes.
__attribute__((noinline))
static int branchy(int x) {
    int value = 0;

    if (x > 0) {
        if (x % 3 == 0) {
            value = 30;
        } else if (x % 5 == 0) {
            value = 50;
        } else {
            value = 7;
        }
    } else {
        value = -x;
    }

    int steps = 0;
    for (int i = 0; i < x % 4; i++) {
        steps += i;
    }

    if (x > 2 && steps < 10) {
        value += 100;
    } else if (x < -2 || steps > 5) {
        value -= 100;
    }

    return value + steps;
}

int main(void) {
    int a = branchy(selector);          /* 2   -> 8   */
    int b = branchy(selector + 4);      /* 6   -> 131 */
    int c = branchy(selector + 3);      /* 5   -> 150 */
    int d = branchy(selector - 6);      /* -4  -> -96 */
    int e = branchy(selector + 13);     /* 15  -> 133 */
    int f = branchy(selector + 23);     /* 25  -> 150 */

    printf("%d %d %d %d %d %d\n", a, b, c, d, e, f);
    return (a == 8 && b == 131 && c == 150 && d == -96 && e == 133 && f == 150) ? 0 : 1;
}
