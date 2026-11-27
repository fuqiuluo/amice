#include "shared_template_string.h"

#include <cstddef>

extern "C" const TemplateMessage* read_a();
extern "C" const TemplateMessage* read_b();
extern "C" const unsigned char* read_a_controls();
extern "C" const unsigned char* read_b_controls();
extern "C" const char* read_a_symbol(std::size_t*, std::uint32_t*);
extern "C" const char* read_b_symbol(std::size_t*, std::uint32_t*);
extern "C" const unsigned char* read_a_local_controls();
extern "C" const unsigned char* read_b_local_controls();
#ifdef WITH_PLAIN_READER
extern "C" const TemplateMessage* read_plain();
extern "C" const unsigned char* read_plain_controls();
extern "C" const char* read_plain_symbol(std::size_t*, std::uint32_t*);
extern "C" const unsigned char* read_plain_local_controls();
#endif

static std::uint32_t hash(const volatile char* data, std::size_t size) {
    std::uint32_t value = 2166136261u;
    for (std::size_t i = 0; i < size; ++i) {
        value = (value ^ static_cast<unsigned char>(data[i])) * 16777619u;
    }
    return value;
}

static bool valid(const volatile TemplateMessage* value) {
    return value->line == 44 && hash(value->text, sizeof(value->text)) == 0x4630230bu &&
           hash(value->source, sizeof(value->source)) == 0x41e61b3au &&
           hash(value->exact, sizeof(value->exact)) == 0x3847bbbfu;
}

static bool valid_controls(const volatile unsigned char* value) {
    if (value[0] != 0xff) return false;
    for (int i = 1; i < 16; ++i) {
        if (value[i] != 0x80) return false;
    }
    return true;
}

using SymbolReader = const char* (*)(std::size_t*, std::uint32_t*);

static const char* checked_symbol(SymbolReader reader) {
    std::size_t size = 0;
    std::uint32_t gnu_hash = 0;
    auto* data = reader(&size, &gnu_hash);
    if (size != 26 || gnu_hash != 0xa19e64b3u || hash(data, size + 1) != 0x4617ba5bu) {
        return nullptr;
    }
    return data;
}

static int check() {
    auto* first = read_a();
    auto* second = read_b();
    if (first != second || !valid(first) || !valid(second)) return 1;
    auto* controls = read_a_controls();
    if (controls != read_b_controls() || !valid_controls(controls)) return 2;
    auto* symbol = checked_symbol(read_a_symbol);
    if (!symbol || symbol != checked_symbol(read_b_symbol)) return 5;
    auto* local_controls = read_a_local_controls();
    if (local_controls != read_b_local_controls() || !valid_controls(local_controls)) return 6;
#ifdef WITH_PLAIN_READER
    if (first != read_plain() || !valid(read_plain())) return 3;
    if (controls != read_plain_controls() || !valid_controls(read_plain_controls())) return 4;
    if (symbol != checked_symbol(read_plain_symbol)) return 7;
    if (local_controls != read_plain_local_controls() || !valid_controls(read_plain_local_controls())) return 8;
#endif
    return 0;
}

// Readers from a plain translation unit must work before main, even in lazy mode
static int before_main = check();

int main() {
    if (before_main != 0) return before_main;
    for (int i = 0; i < 8; ++i) {
        if (int result = check()) return result;
    }
    return 0;
}
