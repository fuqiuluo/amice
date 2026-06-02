// Test C++ exception handling with flatten (should be properly handled)
#include <stdexcept>
#include <cstdio>

#define OBFUSCATE __attribute__((annotate("+flatten")))

// Function with exception and flatten - should be skipped by flatten
OBFUSCATE
int exception_with_flatten(int x) {
    try {
        if (x < 0) {
            throw std::runtime_error("Negative");
        }
        if (x > 100) {
            throw std::range_error("Too large");
        }
        return x * 2;
    } catch (const std::range_error& e) {
        return -2;
    } catch (const std::runtime_error& e) {
        return -1;
    }
}

int main() {
    int r1 = exception_with_flatten(10);   // Should return 20
    int r2 = exception_with_flatten(-5);   // Should return -1
    int r3 = exception_with_flatten(150);  // Should return -2

    return r1 == 20 && r2 == -1 && r3 == -2 ? 0 : 1;
}
