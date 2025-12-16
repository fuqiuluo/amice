// Test C++ exception handling with bogus control flow
#include <stdexcept>
#include <cstdio>

#define OBFUSCATE __attribute__((annotate("+bcf")))

// Function that throws exception - BCF should skip this
OBFUSCATE
int may_throw(int x) {
    if (x < 0) {
        throw std::runtime_error("Negative value");
    }
    return x * 2;
}

// Function with try-catch - BCF should skip this
OBFUSCATE
int catch_exception(int x) {
    try {
        return may_throw(x);
    } catch (const std::exception& e) {
        printf("Caught: %s\n", e.what());
        return -1;
    }
}

// Function with multiple catch blocks
OBFUSCATE
int multiple_catches(int x) {
    try {
        if (x == 0) {
            throw std::runtime_error("Runtime error");
        } else if (x < 0) {
            throw std::invalid_argument("Invalid argument");
        }
        return x;
    } catch (const std::runtime_error& e) {
        return -1;
    } catch (const std::invalid_argument& e) {
        return -2;
    } catch (...) {
        return -3;
    }
}

// Function with nested try-catch
OBFUSCATE
int nested_exception(int x) {
    try {
        try {
            if (x < 0) {
                throw std::runtime_error("Inner exception");
            }
            return x;
        } catch (const std::runtime_error& e) {
            if (x < -10) {
                throw;  // Re-throw
            }
            return 0;
        }
    } catch (...) {
        return -1;
    }
}

int main() {
    int r1 = catch_exception(10);  // Should return 20
    int r2 = catch_exception(-5);  // Should return -1
    int r3 = multiple_catches(5);   // Should return 5
    int r4 = multiple_catches(0);   // Should return -1
    int r5 = multiple_catches(-1);  // Should return -2
    int r6 = nested_exception(5);   // Should return 5
    int r7 = nested_exception(-5);  // Should return 0
    int r8 = nested_exception(-15); // Should return -1

    if (r1 == 20 && r2 == -1 && r3 == 5 && r4 == -1 &&
        r5 == -2 && r6 == 5 && r7 == 0 && r8 == -1) {
        return 0;
    }
    return 1;
}
