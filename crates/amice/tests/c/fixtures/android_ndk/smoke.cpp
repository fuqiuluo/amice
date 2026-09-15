#include <cstdio>
#include <cstdlib>
#include <string>
#include <vector>

__attribute__((noinline))
static unsigned retain_value(unsigned value, unsigned salt, unsigned mirror) {
    return (value + salt) - mirror;
}

__attribute__((noinline, annotate("+flatten,+split_basic_block,+mba")))
static unsigned transform(unsigned value) {
    if (value & 1)
        value = (value * 7) ^ 0x55;
    else
        value = (value + 11) ^ 0x33;
    return retain_value(value, 17, 17) + 17;
}

struct Guard {
    int &count;
    ~Guard() { ++count; }
};

int main(int argc, char **argv) {
    unsigned value = argc > 1 ? std::strtoul(argv[1], nullptr, 10) : 7;
    int destroyed = 0;
    int caught = 0;
    // Exercise nested exception cleanup with both full and ThinLTO (NDK #2073).
    try {
        Guard outer{destroyed};
        try {
            Guard inner{destroyed};
            throw 5;
        } catch (int error) {
            throw error + 1;
        }
    } catch (int error) {
        caught = error;
    }
    std::vector<unsigned> results{transform(value), (unsigned)caught, (unsigned)destroyed};
    std::string marker = "AMICE_NDK_CPP_MARKER";
    std::puts(marker.c_str());
    std::printf("%u:%u:%u\n", results[0], results[1], results[2]);
    return caught == 6 && destroyed == 2 ? 0 : 1;
}
