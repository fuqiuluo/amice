// Test C++ exception handling with indirect branch
#include <stdexcept>

#define OBFUSCATE __attribute__((annotate("+indirect_branch")))

// Test invoke instruction handling
OBFUSCATE
int exception_with_indirect_branch(int x, int y) {
    try {
        if (x < 0) {
            throw std::runtime_error("Negative x");
        }

        int result = 0;
        for (int i = 0; i < y; i++) {
            if (i == 5) {
                throw std::invalid_argument("i is 5");
            }
            result += x;
        }
        return result;
    } catch (const std::runtime_error& e) {
        return -1;
    } catch (const std::invalid_argument& e) {
        return -2;
    }
}

int main() {
    int r1 = exception_with_indirect_branch(10, 3);   // Should return 30
    int r2 = exception_with_indirect_branch(-5, 3);   // Should return -1
    int r3 = exception_with_indirect_branch(10, 10);  // Should return -2

    return (r1 == 30 && r2 == -1 && r3 == -2) ? 0 : 1;
}
