#include <stdint.h>

typedef unsigned __int128 u128;

__attribute__((noinline)) u128 mix_u128(u128 a, u128 b) {
    u128 mask = ((u128)0x123456789ABCDEF0ULL << 64) | (u128)0x0FEDCBA987654321ULL;
    u128 salt = ((u128)0xA5A5A5A5A5A5A5A5ULL << 64) | (u128)0x5A5A5A5A5A5A5A5AULL;
    return (((a + b) ^ mask) - (b | (u128)42)) ^ salt;
}

int main(void) {
    u128 a = ((u128)1 << 100) | (u128)12345;
    u128 b = ((u128)1 << 65) | (u128)67890;
    volatile u128 result = mix_u128(a, b);

    u128 mask = ((u128)0x123456789ABCDEF0ULL << 64) | (u128)0x0FEDCBA987654321ULL;
    u128 salt = ((u128)0xA5A5A5A5A5A5A5A5ULL << 64) | (u128)0x5A5A5A5A5A5A5A5AULL;
    u128 expected = (((a + b) ^ mask) - (b | (u128)42)) ^ salt;

    return result == expected ? 0 : 1;
}
