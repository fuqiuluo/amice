#include "bcf_cpp.h"
#include <cstdio>
#include <cstdlib>

template <class T> static T reference_mix(T x) {
    x += T(0x1357);
    x ^= T(0x2468);
    return x - T(0x369);
}

static std::uint64_t reference(std::uint32_t x) {
    const auto mix32 = std::uint64_t(reference_mix(x));
    const auto wide = std::uint64_t(x);
    return mix32 * 3 + (reference_mix(wide + 7) ^ reference_mix(wide + 9))
        + reference_mix(wide + 1) + wide + 16 + 0xabcdef + 7;
}

int main() {
    bcf_parallel_check();
    std::uint32_t state = 0;
    for (unsigned i = 0; i < 2048; ++i) {
        if (i == 1) state = UINT32_MAX;
        if (i > 1) state = state * 1664525u + 1013904223u;
        const auto actual = bcf_cpp_case(state);
        if (actual != reference(state) || bcf_lifetime_count() != 0
            || bcf_mix(state) != reference_mix(state)) {
            std::fprintf(stderr, "case %u input %u: %llu != %llu, live=%d\n", i, state,
                static_cast<unsigned long long>(actual),
                static_cast<unsigned long long>(reference(state)), bcf_lifetime_count());
            return EXIT_FAILURE;
        }
    }
    std::puts("PASS C++ lifetime, exceptions, coroutine, ABI, COMDAT, TLS and atomics: 2048 inputs");
}
