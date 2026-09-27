#include <cstdint>
#include <cstdio>
#include <stdexcept>

static volatile uint32_t destroyed;
struct Guard {
    uint32_t value;
    ~Guard() { destroyed = (destroyed + value) ^ 0xa5a5a5a5u; }
};

__attribute__((noinline)) uint32_t work(uint32_t x, uint32_t y) {
    Guard guard{x ^ y};
    uint32_t sum = x + y;
    if (x & 1) throw sum;
    if (x & 2) throw std::runtime_error("mba");
    return sum ^ (x - y);
}

int main() {
    uint32_t sink = 0;
    for (uint32_t x = 0; x < 1024; ++x) {
        try { sink += work(x, ~x + 7u); }
        catch (uint32_t value) { sink ^= value + x; }
        catch (const std::exception&) { sink -= x ^ 0x31415926u; }
    }
    std::printf("%08x %08x\n", sink, static_cast<uint32_t>(destroyed));
}
