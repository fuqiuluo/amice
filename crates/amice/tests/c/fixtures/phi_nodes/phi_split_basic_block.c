// Test PHI node handling with split_basic_block
#define OBFUSCATE __attribute__((annotate("+split_basic_block")))

// Function with PHI nodes from if-else
OBFUSCATE
int phi_from_if_else(int x, int y) {
    int result;
    if (x > y) {
        result = x;
    } else {
        result = y;
    }
    return result * 2;
}

// Function with PHI nodes from loop
OBFUSCATE
int phi_from_loop(int n) {
    int sum = 0;
    for (int i = 0; i < n; i++) {
        sum += i;
    }
    return sum;
}

// Function with multiple PHI nodes
OBFUSCATE
int multiple_phi_nodes(int a, int b, int c) {
    int x, y;

    if (a > b) {
        x = a;
        y = b;
    } else {
        x = b;
        y = a;
    }

    int result;
    if (x > c) {
        result = x + y;
    } else {
        result = y + c;
    }

    return result;
}

// Function with nested control flow and PHI nodes
OBFUSCATE
int nested_phi(int n) {
    int result = 0;

    for (int i = 0; i < n; i++) {
        int temp;
        if (i % 2 == 0) {
            temp = i * 2;
        } else {
            temp = i + 1;
        }
        result += temp;
    }

    return result;
}

// Function with switch-like PHI pattern
OBFUSCATE
int switch_phi(int x) {
    int result;

    if (x == 0) {
        result = 1;
    } else if (x == 1) {
        result = 2;
    } else if (x == 2) {
        result = 4;
    } else {
        result = 0;
    }

    return result;
}

int main() {
    int r1 = phi_from_if_else(10, 5);    // Should return 20
    int r2 = phi_from_if_else(3, 8);     // Should return 16
    int r3 = phi_from_loop(5);           // Should return 10 (0+1+2+3+4)
    int r4 = multiple_phi_nodes(5, 3, 2); // Should return 8 (5+3)
    int r5 = multiple_phi_nodes(2, 8, 10); // Should return 12
    int r6 = nested_phi(4);              // Should return 10 (0+2+4+4)
    int r7 = switch_phi(1);              // Should return 2

    if (r1 == 20 && r2 == 16 && r3 == 10 && r4 == 8 &&
        r5 == 12 && r6 == 10 && r7 == 2) {
        return 0;
    }
    return 1;
}
