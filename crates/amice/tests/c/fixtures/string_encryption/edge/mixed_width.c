// Edge: i8 / i16 / i32 string literals coexisting in one module.
#include <stdio.h>
#include <uchar.h>

int main(void) {
    const char *narrow = "NARROW_i8_secret";
    const char16_t *u16 = u"WIDE16_你";
    const char32_t *u32 = U"WIDE32_你";

    printf("%s\n", narrow);
    for (int i = 0; u16[i]; ++i) printf("%04x ", (unsigned)u16[i]);
    printf("\n");
    for (int i = 0; u32[i]; ++i) printf("%04x ", (unsigned)u32[i]);
    printf("\n");
    return 0;
}
