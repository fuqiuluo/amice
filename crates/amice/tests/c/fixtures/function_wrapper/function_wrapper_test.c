#include <stdio.h>
#include <stdlib.h>

int add(int a, int b) {
    printf("In add function: %d + %d\n", a, b);
    return a + b;
}

int multiply(int a, int b) {
    printf("In multiply function: %d * %d\n", a, b);
    return a * b;
}

void greet(const char* name) {
    printf("Hello, %s!\n", name);
}

int main() {
    printf("Testing function wrapper pass\n");
    
    int result1 = add(5, 3);
    printf("Result of add: %d\n", result1);
    
    int result2 = multiply(4, 7);
    printf("Result of multiply: %d\n", result2);
    
    greet("Function Wrapper");
    
    return 0;
}