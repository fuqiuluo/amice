#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>

typedef uint64_t (*test_fn)(uint32_t, uint64_t, uint64_t);
#define DECLARE(name) \
    uint64_t name(uint32_t, uint64_t, uint64_t); \
    uint64_t reference_##name(uint32_t, uint64_t, uint64_t)
DECLARE(vmf_switch);
DECLARE(vmf_loop);
DECLARE(vmf_recursive);

int main(void) {
    const test_fn protected[] = {vmf_switch, vmf_loop, vmf_recursive};
    const test_fn reference[] = {reference_vmf_switch, reference_vmf_loop, reference_vmf_recursive};
    const uint64_t values[] = {0, 1, UINT32_MAX, UINT64_C(1) << 32,
                              UINT64_C(1) << 63, UINT64_MAX};
    uint64_t random = UINT64_C(0x9e3779b97f4a7c15);
    unsigned comparisons = 0;
    for (unsigned function = 0; function < 3; ++function) {
        /* 0..31 covers every switch case/default, loop limit and depth. */
        for (uint32_t selector = 0; selector < 32; ++selector) {
            for (unsigned i = 0; i < 48; ++i) {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                uint64_t x = i < 36 ? values[i / 6] : random;
                uint64_t y = i < 36 ? values[i % 6] : random * 7 + selector;
                uint64_t expected = reference[function](selector, x, y);
                uint64_t actual = protected[function](selector, x, y);
                if (actual != expected) {
                    fprintf(stderr, "VMF mismatch fn=%u selector=%u x=%" PRIu64
                            " y=%" PRIu64 " expected=%" PRIu64 " actual=%" PRIu64 "\n",
                            function, selector, x, y, expected, actual);
                    return 1;
                }
                ++comparisons;
            }
        }
    }
    printf("PASS VMF shared %u comparisons\n", comparisons);
    return 0;
}
