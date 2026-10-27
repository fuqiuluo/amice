// Edge: interior code unit 0xAAAA XORs to 0x0000 under the xor key, so the
// encrypted initializer contains an embedded null. The byte-length decryptor
// must not stop early. Also covers 0xFFFF and a lone surrogate.
#include <stdio.h>
#include <uchar.h>

static const char16_t edge[] = {0x0041, 0xAAAA, 0xFFFF, 0xD800, 0x4F60, 0};

int main(void) {
    for (int i = 0; edge[i]; ++i) printf("%04x ", (unsigned)edge[i]);
    printf("\n");
    return 0;
}
