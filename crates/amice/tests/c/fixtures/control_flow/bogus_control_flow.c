#include <stdio.h>

// Simple test program with multiple basic blocks for bogus control flow obfuscation
volatile int global_sink;

void test_simple_branches(int x) {
    if (x > 10) {
        global_sink = x * 2;
    } else {
        global_sink = x + 1;
    }
}

void test_nested_conditions(int a, int b) {
    if (a > 5) {
        if (b > 3) {
            global_sink = a + b;
        } else {
            global_sink = a - b;
        }
    } else {
        global_sink = a * b;
    }
}

void test_loop(int n) {
    int sum = 0;
    for (int i = 0; i < n; i++) {
        sum += i;
    }
    global_sink = sum;
}

int main() {
    printf("Testing bogus control flow obfuscation...\n");
    
    test_simple_branches(15);
    printf("Simple branches result: %d\n", global_sink);
    
    test_nested_conditions(7, 4);
    printf("Nested conditions result: %d\n", global_sink);
    
    test_loop(5);
    printf("Loop result: %d\n", global_sink);
    
    return 0;
}