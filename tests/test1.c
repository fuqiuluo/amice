#include <stdio.h>
#include <stdlib.h>

int compute_value(int base, int multiplier, int offset) {
    int result = base * multiplier;
    result += offset;

    // Some additional computation to make it worth specializing
    if (multiplier > 10) {
        result += base / 2;
    } else {
        result -= base / 4;
    }

    return result;
}

int main() {
    int val1 = compute_value(5, 10, 3);    // Should create specialized version for (multiplier=10, offset=3)
    int val2 = compute_value(5, val1, 3);

    printf("compute_value(5, 10, 3) = %d\n", val1);
    printf("compute_value(5, val1, 3) = %d\n", val2);

    return 0;
}