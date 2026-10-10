// Struct globals with MORE THAN ONE encryptable array field.
//
// Regression shape for the stale multi-field rebuild in StringEncryption:
// each field is re-encrypted against the ORIGINAL initializer captured at
// collection time, so every set_initializer except the last one restores
// the previously encrypted fields to plaintext, while decryptors are still
// emitted for all of them -> all but the last field get XORed exactly once
// at runtime (double encryption).
//
// Expected (unobfuscated baseline):
//   wide:   2 14 39
//   narrow: 62 78 77
//   mixed:  62 2 78
//
// Known-failure state (see tests/string_encryption_edge_cases.rs):
//   master: narrow/mixed first field corrupted (pre-existing bug)
//   PR #94: wide/mixed ALSO corrupted (wide detection extends the bug)
#include <stdio.h>
#include <uchar.h>

struct TwoWide {
    char16_t a[4];
    char16_t b[4];
};
static const struct TwoWide W = {{41, 2, 3, 0}, {57, 20, 30, 0}};

struct TwoNarrow {
    char a[5];
    char b[5];
};
static const struct TwoNarrow N = {{"abcd"}, {"wxyz"}};

struct Mixed {
    char a[5];
    char16_t w[4];
    char b[5];
};
static const struct Mixed M = {{"abcd"}, {41, 2, 3, 0}, {"wxyz"}};

int main(void)
{
    printf("wide:   %x %x %x\n", (unsigned)W.a[1], (unsigned)W.b[1], (unsigned)W.b[0]);
    printf("narrow: %x %x %x\n", (unsigned char)N.a[1], (unsigned char)N.b[1], (unsigned char)N.b[0]);
    printf("mixed:  %x %x %x\n", (unsigned char)M.a[1], (unsigned)M.w[1], (unsigned char)M.b[1]);
    return 0;
}
