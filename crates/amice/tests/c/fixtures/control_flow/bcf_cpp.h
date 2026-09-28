#pragma once
#include <cstdint>

#if defined(__clang__)
#define BCF_NOINLINE __attribute__((noinline))
#else
#define BCF_NOINLINE
#endif

// Both translation units instantiate this template to exercise COMDAT linking.
template <class T> BCF_NOINLINE T bcf_mix(T x) {
    return ((x + T(0x1357)) ^ T(0x2468)) - T(0x369);
}

struct BcfPacket {
    std::uint64_t words[8];
    BCF_NOINLINE BcfPacket transformed() const;
};

BCF_NOINLINE std::uint64_t bcf_cpp_case(std::uint32_t x);
BCF_NOINLINE int bcf_lifetime_count();
BCF_NOINLINE void bcf_parallel_check();
