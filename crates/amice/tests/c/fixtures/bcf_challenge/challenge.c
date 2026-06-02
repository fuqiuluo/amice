#include <stdio.h>
#include <string.h>
#include <stdint.h>

// A simple license check challenge for reversing practice
// Compile with BCF obfuscation and try to recover the key logic

static uint32_t rotate_left(uint32_t v, int n) {
    return (v << n) | (v >> (32 - n));
}

static uint32_t mix(uint32_t a, uint32_t b) {
    a ^= rotate_left(b, 7);
    a -= b;
    a ^= rotate_left(b, 13);
    return a;
}

// The actual check: takes a 4-byte key, returns 1 if correct
__attribute__((annotate("+bcf,bcf_prob=100,bcf_loops=2")))
int check_key(const char *key) {
    if (strlen(key) != 8)
        return 0;

    uint32_t k0 = ((uint8_t)key[0] << 24) | ((uint8_t)key[1] << 16)
                | ((uint8_t)key[2] << 8)  |  (uint8_t)key[3];
    uint32_t k1 = ((uint8_t)key[4] << 24) | ((uint8_t)key[5] << 16)
                | ((uint8_t)key[6] << 8)  |  (uint8_t)key[7];

    uint32_t h = mix(k0, 0xDEADBEEF);
    h = mix(h, k1);
    h = mix(h, 0x13371337);

    // Expected hash of the "correct" key
    // Key is "Am1c3Ke!" -> you can verify: mix(mix(mix(...)))
    return (h == 0x19B0C57FU) ? 1 : 0;
}

__attribute__((annotate("+bcf,bcf_prob=100")))
void print_status(int ok) {
    if (ok) {
        printf("[+] Correct key!\n");
    } else {
        printf("[-] Wrong key.\n");
    }
}

int main(int argc, char *argv[]) {
    if (argc < 2) {
        printf("Usage: %s <key>\n", argv[0]);
        return 1;
    }
    int result = check_key(argv[1]);
    print_status(result);
    return result ? 0 : 1;
}
