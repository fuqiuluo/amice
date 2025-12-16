// Test PHI node handling with flatten (should work correctly)
#define OBFUSCATE __attribute__((annotate("+flatten")))

// Complex PHI scenario with flatten
OBFUSCATE
int complex_phi_flatten(int x, int y, int z) {
    int a, b, c;

    // First decision - creates PHI nodes
    if (x > 0) {
        a = x * 2;
        b = y + 1;
    } else {
        a = x + 10;
        b = y * 2;
    }

    // Second decision - more PHI nodes
    if (b > a) {
        c = a + b;
    } else {
        c = a - b;
    }

    // Loop - PHI nodes for loop variables
    int result = c;
    for (int i = 0; i < z; i++) {
        if (i % 2 == 0) {
            result += i;
        } else {
            result -= i;
        }
    }

    return result;
}

int main() {
    int r1 = complex_phi_flatten(5, 3, 4);   // 10, 4 -> 6 + (0-1+2-3) = 4
    int r2 = complex_phi_flatten(-2, 5, 3);  // 8, 10 -> 18 + (0-1+2) = 19

    return (r1 == 4 && r2 == 19) ? 0 : 1;
}
