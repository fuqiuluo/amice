#include <cstddef>
#include <cstdio>
#include <cstring>

struct Entry {
    const char *text;
    std::size_t size;
};

static constexpr Entry kEntries[] = {
    {"AMICE_AGGREGATE_SECRET_ALPHA", 28},
    {"AMICE_AGGREGATE_SECRET_BETA", 27},
};

int main() {
    for (const Entry &entry : kEntries) {
        if (std::strlen(entry.text) != entry.size) return 1;
        std::puts(entry.text);
    }
    return 0;
}
