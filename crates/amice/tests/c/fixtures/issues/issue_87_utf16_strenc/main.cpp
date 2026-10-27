#include <cstdint>
#include <cstdio>

int main()
{
    const char16_t *secret = u"AMICE_ISSUE_87_\u5BBD\u5B57\u7B26";
    std::uint32_t hash = 2166136261u;

    for (const char16_t *code_unit = secret; *code_unit != u'\0'; ++code_unit) {
        hash ^= static_cast<std::uint32_t>(*code_unit);
        hash *= 16777619u;
    }

    std::printf("%u\n", hash);
    return 0;
}
