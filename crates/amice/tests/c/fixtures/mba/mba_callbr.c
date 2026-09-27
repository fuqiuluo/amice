#include <stdint.h>
#include <stdio.h>
__attribute__((noinline)) uint32_t callbr_value(uint32_t x, uint32_t y) {
    uint32_t v = x + y;
    __asm__ goto("" : "+r"(v) : : : out);
    return (v ^ y) + 3;
out:
    return v - x;
}
int main(void) {
    uint32_t sum = 0;
    for (uint32_t x = 0; x < 4096; ++x) {
        uint32_t y = ~x * 17;
        uint32_t result = callbr_value(x, y);
        if (result != (((x + y) ^ y) + 3)) return 1;
        sum ^= result + x;
    }
    printf("%08x\n", sum);
    return 0;
}
