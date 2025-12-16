// Test large function handling (near the 4096 basic block limit)
#define OBFUSCATE __attribute__((annotate("+flatten")))

// Function with many basic blocks
OBFUSCATE
int large_function(int x) {
    int result = 0;

    // Generate many if-else chains to create many basic blocks
    if (x == 0) result += 1;
    else if (x == 1) result += 2;
    else if (x == 2) result += 3;
    else if (x == 3) result += 4;
    else if (x == 4) result += 5;
    else if (x == 5) result += 6;
    else if (x == 6) result += 7;
    else if (x == 7) result += 8;
    else if (x == 8) result += 9;
    else if (x == 9) result += 10;
    else if (x == 10) result += 11;
    else if (x == 11) result += 12;
    else if (x == 12) result += 13;
    else if (x == 13) result += 14;
    else if (x == 14) result += 15;
    else if (x == 15) result += 16;
    else if (x == 16) result += 17;
    else if (x == 17) result += 18;
    else if (x == 18) result += 19;
    else if (x == 19) result += 20;
    else result += 100;

    // Add more branches
    for (int i = 0; i < 10; i++) {
        if (i % 2 == 0) {
            result += i;
        } else {
            result -= i;
        }
    }

    return result;
}

int main() {
    int r = large_function(5);
    return r == 1 ? 0 : 1;
}
