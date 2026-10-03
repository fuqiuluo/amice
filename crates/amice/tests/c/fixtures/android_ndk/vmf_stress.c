/* Shared-library VMF regression: multiple targets per shard, loop PHIs and
 * recursive frames. All arithmetic is unsigned with bounded shift counts. */
#include <stdint.h>

#ifdef VMF_REFERENCE
#define NAME_(name) reference_##name
#else
#define NAME_(name) name
#endif
#define EXPORT __attribute__((noinline, visibility("default")))

EXPORT uint64_t NAME_(vmf_switch)(uint32_t selector, uint64_t x, uint64_t y) {
    uint64_t value;
    switch (selector) {
    case 0: value = x + y; break;
    case 1: value = (x ^ y) * 3; break;
    case 2: value = (x << 7) | (y >> 57); break;
    case 3: value = x - y * 5; break;
    case 4: value = (y << 31) ^ (x >> 17); break;
    case 5: value = (x | y) + 17; break;
    case 6: value = (x & y) - 19; break;
    case 7:
    case 13: value = ~x + (y ^ UINT64_C(0xa55a)); break;
    case 8: value = (x >> 63) + y * 23; break;
    case 9: value = x * x + y; break;
    case 10: value = (x + 29) ^ (y - 31); break;
    case 11: value = (x >> 11) | (y << 53); break;
    case 12: value = x ^ (y * 37 + 41); break;
    case 14: value = (x - 43) * (y + 47); break;
    case 15: value = (x ^ (x >> 9)) + y; break;
    case 16: value = (y ^ (y << 5)) - x; break;
    default: value = (x + UINT64_C(0x9e3779b97f4a7c15)) ^ y; break;
    }
    return (value ^ (value >> 23)) + selector;
}

EXPORT uint64_t NAME_(vmf_loop)(uint32_t count, uint64_t a, uint64_t b) {
    uint64_t sum = 0;
    for (uint32_t i = 0; i < (count & 31); ++i) {
        uint64_t previous = a;
        a = b;
        b = previous;
        if ((a ^ i) & 1) {
            sum += a ^ (b >> 3);
        } else {
            sum ^= b + (a << 5);
        }
    }
    return sum ^ a ^ (b << 1);
}

EXPORT uint64_t NAME_(vmf_recursive)(uint32_t depth, uint64_t x, uint64_t y) {
    depth &= 7;
    if (!depth)
        return x ^ y;
    uint64_t inner = NAME_(vmf_recursive)(depth - 1, y + depth, x * 3);
    /* Post-call work keeps independent invocation frames observable. */
    return (inner + x) ^ (y + depth);
}
