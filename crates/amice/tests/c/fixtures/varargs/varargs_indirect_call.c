// Test varargs function with indirect call obfuscation
#include <stdio.h>
#include <stdarg.h>

#define OBFUSCATE __attribute__((annotate("+indirect_call")))

// Custom varargs function
int sum_varargs(int count, ...) {
    va_list args;
    va_start(args, count);

    int sum = 0;
    for (int i = 0; i < count; i++) {
        sum += va_arg(args, int);
    }

    va_end(args);
    return sum;
}

// Function that calls varargs function - should not break printf
OBFUSCATE
void test_printf(int x, int y) {
    printf("x=%d, y=%d\n", x, y);
    printf("sum=%d\n", x + y);
}

// Function calling custom varargs
OBFUSCATE
int test_custom_varargs(int a, int b, int c) {
    int result = sum_varargs(3, a, b, c);
    return result;
}

// Function using sprintf
OBFUSCATE
void test_sprintf(char* buffer, int x) {
    sprintf(buffer, "Value: %d", x);
}

// Function using fprintf
OBFUSCATE
void test_fprintf(int x, int y) {
    fprintf(stderr, "Error: x=%d, y=%d\n", x, y);
}

int main() {
    test_printf(10, 20);

    int r1 = test_custom_varargs(1, 2, 3);  // Should return 6

    char buffer[100];
    test_sprintf(buffer, 42);

    test_fprintf(100, 200);

    return r1 == 6 ? 0 : 1;
}
