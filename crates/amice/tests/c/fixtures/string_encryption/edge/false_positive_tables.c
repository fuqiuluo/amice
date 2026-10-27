// Edge: genuine (non-string) int tables ending in 0 are misclassified as
// "wide strings" by the heuristic. Encryption must stay behaviour-preserving.
#include <stdio.h>
#include <stdint.h>

static const int32_t  codes32[] = {1000, -7, 300000, 42, 0};
static const uint16_t codes16[] = {0xBEEF, 0x1234, 0x00FF, 0xABCD, 0};

int main(void) {
    long a = 0; for (int i = 0; i < 5; ++i) a += codes32[i];
    unsigned b = 0; for (int i = 0; i < 5; ++i) b += codes16[i];
    printf("%ld %u\n", a, b);
    return 0;
}
