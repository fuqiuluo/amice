// Edge: [N x i16]/[N x i32] whose last element is non-zero must be skipped.
#include <stdio.h>
#include <stdint.h>

static const int32_t  nz32[] = {5, 6, 7, 8};
static const uint16_t nz16[] = {0x1, 0x2, 0x3, 0x9};

int main(void) {
    long a = 0; for (int i = 0; i < 4; ++i) a += nz32[i];
    unsigned b = 0; for (int i = 0; i < 4; ++i) b += nz16[i];
    printf("%ld %u\n", a, b);
    return 0;
}
