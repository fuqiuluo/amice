// Test inline assembly handling with obfuscation
#define OBFUSCATE __attribute__((annotate("+flatten,+bcf")))

#if defined(__x86_64__) || defined(__i386__)

// Function with inline assembly - should be skipped by obfuscation
OBFUSCATE
int function_with_inline_asm(int x, int y) {
    int result;

    #ifdef __x86_64__
    __asm__ volatile (
        "movl %1, %%eax\n\t"
        "addl %2, %%eax\n\t"
        "movl %%eax, %0\n\t"
        : "=r" (result)
        : "r" (x), "r" (y)
        : "%eax"
    );
    #else
    result = x + y;  // Fallback
    #endif

    return result;
}

// Function with inline asm and control flow
OBFUSCATE
int inline_asm_with_branches(int x) {
    int result;

    if (x > 0) {
        #ifdef __x86_64__
        __asm__ volatile (
            "movl %1, %%eax\n\t"
            "imull $2, %%eax\n\t"
            "movl %%eax, %0\n\t"
            : "=r" (result)
            : "r" (x)
            : "%eax"
        );
        #else
        result = x * 2;
        #endif
    } else {
        result = 0;
    }

    return result;
}

#else

// Non-x86 fallback
int function_with_inline_asm(int x, int y) {
    return x + y;
}

int inline_asm_with_branches(int x) {
    return (x > 0) ? (x * 2) : 0;
}

#endif

// Function without inline asm for comparison
OBFUSCATE
int normal_function(int x, int y) {
    if (x > y) {
        return x * 2;
    } else {
        return y * 2;
    }
}

int main() {
    int r1 = function_with_inline_asm(10, 20);   // Should return 30
    int r2 = inline_asm_with_branches(5);        // Should return 10
    int r3 = inline_asm_with_branches(-3);       // Should return 0
    int r4 = normal_function(10, 5);             // Should return 20

    if (r1 == 30 && r2 == 10 && r3 == 0 && r4 == 20) {
        return 0;
    }
    return 1;
}
