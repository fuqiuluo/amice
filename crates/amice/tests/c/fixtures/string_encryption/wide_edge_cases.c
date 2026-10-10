// Wide-character string encryption edge cases (PR #94 boundary probe).
//
// Shapes that must round-trip or be skipped safely:
//  - struct with a SINGLE [N x i16] field            -> encrypt + round-trip
//  - [N x i16] zeroinitializer                       -> skipped (LLVMIsNull)
//  - [N x i16] data array without trailing NUL       -> skipped, plaintext OK
//  - [N x i64] array ending in 0                     -> skipped (width filter)
//  - [2 x i16] minimal wide string                   -> encrypt + round-trip
#include <stdio.h>
#include <uchar.h>

struct Key16 {
    char16_t u[8];
};
static const struct Key16 K = {{0x4E62, 0x4F60, u'A', u'B', u'C', u'D', u'E', 0}};

static char16_t Z[8]; // zeroinitializer: must be skipped untouched

static const unsigned short D[6] = {1, 3, 5, 7, 9, 11}; // no trailing NUL: skipped

static const unsigned long long Q[4] = {1, 2, 3, 0}; // i64: width rejected

static const char16_t M2[2] = {0x4F60, 0}; // minimal wide string

int main(void)
{
    if (K.u[0] != 0x4E62 || K.u[1] != 0x4F60 || K.u[2] != u'A' || K.u[7] != 0) {
        puts("K corrupt");
        return 1;
    }
    if (Z[3] != 0 || Z[7] != 0) {
        puts("Z corrupt");
        return 2;
    }
    if (D[1] != 3 || D[5] != 11) {
        puts("D corrupt");
        return 3;
    }
    if (Q[1] != 2 || Q[3] != 0) {
        puts("Q corrupt");
        return 4;
    }
    if (M2[0] != 0x4F60) {
        puts("M2 corrupt");
        return 5;
    }
    puts("WIDE_EDGE_OK");
    return 0;
}
