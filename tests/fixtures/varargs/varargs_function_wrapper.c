// Test varargs function with function wrapper obfuscation
#include <stdio.h>
#include <stdarg.h>

#define OBFUSCATE __attribute__((annotate("+function_wrapper")))

// Custom varargs function that should not be wrapped
int max_varargs(int count, ...) {
    va_list args;
    va_start(args, count);

    int max_val = va_arg(args, int);
    for (int i = 1; i < count; i++) {
        int val = va_arg(args, int);
        if (val > max_val) {
            max_val = val;
        }
    }

    va_end(args);
    return max_val;
}

// Function that calls varargs - wrapper should handle this correctly
OBFUSCATE
int call_varargs(int a, int b, int c) {
    return max_varargs(3, a, b, c);
}

// Function using printf - should not break
OBFUSCATE
void print_values(int x, int y, int z) {
    printf("Values: %d, %d, %d\n", x, y, z);
    printf("Product: %d\n", x * y * z);
}

int main() {
    int r1 = call_varargs(5, 10, 3);  // Should return 10
    print_values(2, 3, 4);

    return r1 == 10 ? 0 : 1;
}
