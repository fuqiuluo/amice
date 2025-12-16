// Test varargs function with clone function obfuscation
#include <stdio.h>
#include <stdarg.h>

#define OBFUSCATE __attribute__((annotate("+clone_function")))

// Varargs function should not be cloned
int min_varargs(int count, ...) {
    va_list args;
    va_start(args, count);

    int min_val = va_arg(args, int);
    for (int i = 1; i < count; i++) {
        int val = va_arg(args, int);
        if (val < min_val) {
            min_val = val;
        }
    }

    va_end(args);
    return min_val;
}

// Function that might be cloned - calls varargs
OBFUSCATE
int test_min(int a, int b, int c, int d) {
    return min_varargs(4, a, b, c, d);
}

int main() {
    int r1 = test_min(5, 2, 8, 3);  // Should return 2
    int r2 = test_min(1, 1, 1, 1);  // Should return 1

    return (r1 == 2 && r2 == 1) ? 0 : 1;
}
