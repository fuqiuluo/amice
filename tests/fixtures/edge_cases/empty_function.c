// Test empty function handling
#define OBFUSCATE __attribute__((annotate("+flatten,+bcf,+mba,+indirect_branch")))

// Empty function - should be skipped by all passes
OBFUSCATE
void empty_function() {
}

// Single return function - should be skipped
OBFUSCATE
int single_return() {
    return 42;
}

// Single basic block function
OBFUSCATE
int single_block(int a, int b) {
    return a + b;
}

// Function with only declarations - no operations
OBFUSCATE
void only_declarations() {
    int x;
    int y;
    int z;
}

// Normal function for comparison
OBFUSCATE
int normal_function(int a, int b) {
    if (a > b) {
        return a;
    } else {
        return b;
    }
}

int main() {
    empty_function();
    int r1 = single_return(); // 42
    int r2 = single_block(10, 20); // 30
    only_declarations();
    int r3 = normal_function(r1, r2); // 42
    return r3 == 42 ? 0 : 1;
}
