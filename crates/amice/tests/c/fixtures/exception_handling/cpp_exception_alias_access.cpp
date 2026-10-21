// Test C++ exception handling with alias access
#include <stdexcept>

#define OBFUSCATE __attribute__((annotate("+alias_access")))

// Throws across multiple frames - AliasAccess should skip this
OBFUSCATE
static int depth_sum(int n) {
    if (n == 0) {
        throw std::runtime_error("bottom");
    }
    return n + depth_sum(n - 1);
}

struct Guard {
    int *sink;
    explicit Guard(int *s) : sink(s) { *sink += 1; }
    ~Guard() { *sink += 10; }
};

// Destructor runs during unwinding and the catch object is read through the
// catchpad storage slot (issue #102)
OBFUSCATE
int catch_with_cleanup() {
    int dtor = 0;
    int result = 0;
    try {
        Guard g(&dtor);
        result = depth_sum(8);
    } catch (const std::exception &e) {
        result = e.what()[0] == 'b' ? 1 : 2;
    }
    return result * 1000 + dtor;
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
    } catch (const std::runtime_error &e) {
        return -1;
    } catch (const std::invalid_argument &e) {
        return -2;
    } catch (...) {
        return -3;
    }
}

// Function without exception constructs - AliasAccess still rewrites this one
OBFUSCATE
int plain_local(int x) {
    int a = x * 3;
    int b = a + 7;
    return b - x;
}

int main() {
    int r1 = catch_with_cleanup();   // Should return 1011
    int r2 = multiple_catches(5);    // Should return 5
    int r3 = multiple_catches(0);    // Should return -1
    int r4 = multiple_catches(-1);   // Should return -2
    int r5 = plain_local(4);         // Should return 15

    if (r1 == 1011 && r2 == 5 && r3 == -1 && r4 == -2 && r5 == 15) {
        return 0;
    }
    return 1;
}
