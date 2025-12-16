#include <stdio.h>

#define OBFUSCATE_CC __attribute__((annotate("custom_calling_conv")))

OBFUSCATE_CC
int add(int a, int b) {
    return a + b;
}

OBFUSCATE_CC
int multiply(int x, int y) {
    return x * y;
}

int main() {
    int result1 = add(10, 20);
    int result2 = multiply(5, 6);

    printf("add(10, 20) = %d\n", result1);
    printf("multiply(5, 6) = %d\n", result2);

    return 0;
}