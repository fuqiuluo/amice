// Simple test program for shuffle blocks functionality
#include <stdio.h>

int test_function(int x) {
    // Create multiple basic blocks to test shuffling
    if (x > 10) {
        printf("Block 1: x > 10\n");
        x = x * 2;
    } else if (x > 5) {
        printf("Block 2: x > 5\n");  
        x = x + 10;
    } else if (x > 0) {
        printf("Block 3: x > 0\n");
        x = x - 1;
    } else {
        printf("Block 4: x <= 0\n");
        x = 0;
    }
    
    // Additional blocks
    if (x % 2 == 0) {
        printf("Block 5: x is even\n");
        x = x / 2;
    } else {
        printf("Block 6: x is odd\n");
        x = x * 3;
    }
    
    return x;
}

int main() {
    int result1 = test_function(15);
    int result2 = test_function(7);
    int result3 = test_function(3);
    int result4 = test_function(-1);
    
    printf("Results: %d, %d, %d, %d\n", result1, result2, result3, result4);
    return 0;
}