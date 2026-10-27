// Edge: [N x i64] ending in 0 must NOT be touched (width 64 not handled).
#include <stdio.h>
#include <stdint.h>

static const int64_t big[] = {100000000000LL, -2, 0};

int main(void) {
    long long s = 0; for (int i = 0; i < 3; ++i) s += big[i];
    printf("%lld\n", s);
    return 0;
}
