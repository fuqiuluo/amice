// Plain i8 string only, short enough to sit within GlobalOpt's static
// constructor evaluation budget. Used to document a PRE-EXISTING
// simd_xor+global+O2 weakness (unrelated to wide-char support / PR #94):
// at O2 the decrypt constructor is statically evaluated away and the
// plaintext is restored. A longer string would exceed the budget and survive.
#include <stdio.h>
int main(void) {
    const char *s = "SIMD_O2_BUG_MARKER";
    printf("%s\n", s);
    return 0;
}
