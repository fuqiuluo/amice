#include <stdint.h>
#include <stdio.h>
#include <string.h>

static uint32_t rol32(uint32_t value, uint32_t shift) {
    return (value << shift) | (value >> (32u - shift));
}

static uint32_t ror32(uint32_t value, uint32_t shift) {
    return (value >> shift) | (value << (32u - shift));
}

static uint32_t load_be32(const uint8_t *p) {
    return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 8) | (uint32_t)p[3];
}

static uint32_t load_le32(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static void store_be32(uint8_t *p, uint32_t value) {
    p[0] = (uint8_t)(value >> 24);
    p[1] = (uint8_t)(value >> 16);
    p[2] = (uint8_t)(value >> 8);
    p[3] = (uint8_t)value;
}

static void store_le32(uint8_t *p, uint32_t value) {
    p[0] = (uint8_t)value;
    p[1] = (uint8_t)(value >> 8);
    p[2] = (uint8_t)(value >> 16);
    p[3] = (uint8_t)(value >> 24);
}

__attribute__((noinline)) static void sha1_digest(const uint8_t *input, size_t len, uint8_t out[20]) {
    uint8_t block[64];
    uint32_t w[80];
    uint32_t h0 = 0x67452301u;
    uint32_t h1 = 0xefcdab89u;
    uint32_t h2 = 0x98badcfeu;
    uint32_t h3 = 0x10325476u;
    uint32_t h4 = 0xc3d2e1f0u;

    memset(block, 0, sizeof(block));
    memcpy(block, input, len);
    block[len] = 0x80u;
    uint64_t bit_len = (uint64_t)len * 8u;
    for (int i = 0; i < 8; ++i) {
        block[63 - i] = (uint8_t)(bit_len >> (8 * i));
    }

    for (int i = 0; i < 16; ++i) {
        w[i] = load_be32(block + i * 4);
    }
    for (int i = 16; i < 80; ++i) {
        w[i] = rol32(w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16], 1);
    }

    uint32_t a = h0;
    uint32_t b = h1;
    uint32_t c = h2;
    uint32_t d = h3;
    uint32_t e = h4;
    for (int i = 0; i < 80; ++i) {
        uint32_t f;
        uint32_t k;
        if (i < 20) {
            f = (b & c) | ((~b) & d);
            k = 0x5a827999u;
        } else if (i < 40) {
            f = b ^ c ^ d;
            k = 0x6ed9eba1u;
        } else if (i < 60) {
            f = (b & c) | (b & d) | (c & d);
            k = 0x8f1bbcdcu;
        } else {
            f = b ^ c ^ d;
            k = 0xca62c1d6u;
        }
        uint32_t temp = rol32(a, 5) + f + e + k + w[i];
        e = d;
        d = c;
        c = rol32(b, 30);
        b = a;
        a = temp;
    }

    h0 += a;
    h1 += b;
    h2 += c;
    h3 += d;
    h4 += e;
    store_be32(out + 0, h0);
    store_be32(out + 4, h1);
    store_be32(out + 8, h2);
    store_be32(out + 12, h3);
    store_be32(out + 16, h4);
}

__attribute__((noinline)) static void rc4_crypt(
    const uint8_t *key,
    size_t key_len,
    const uint8_t *input,
    uint8_t *output,
    size_t len
) {
    uint8_t s[256];
    for (int i = 0; i < 256; ++i) {
        s[i] = (uint8_t)i;
    }

    uint8_t j = 0;
    for (int i = 0; i < 256; ++i) {
        j = (uint8_t)(j + s[i] + key[(size_t)i % key_len]);
        uint8_t tmp = s[i];
        s[i] = s[j];
        s[j] = tmp;
    }

    uint8_t i = 0;
    j = 0;
    for (size_t n = 0; n < len; ++n) {
        i = (uint8_t)(i + 1);
        j = (uint8_t)(j + s[i]);
        uint8_t tmp = s[i];
        s[i] = s[j];
        s[j] = tmp;
        uint8_t k = s[(uint8_t)(s[i] + s[j])];
        output[n] = input[n] ^ k;
    }
}

static const uint8_t AES_SBOX[256] = {
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
};

static uint8_t aes_xtime(uint8_t value) {
    return (uint8_t)((value << 1) ^ ((value & 0x80u) ? 0x1bu : 0x00u));
}

static void aes_key_expand(const uint8_t key[16], uint8_t round_keys[176]) {
    static const uint8_t rcon[10] = {0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36};
    memcpy(round_keys, key, 16);
    uint8_t temp[4];
    int bytes = 16;
    int rcon_index = 0;
    while (bytes < 176) {
        for (int i = 0; i < 4; ++i) {
            temp[i] = round_keys[bytes - 4 + i];
        }
        if ((bytes % 16) == 0) {
            uint8_t t = temp[0];
            temp[0] = AES_SBOX[temp[1]] ^ rcon[rcon_index++];
            temp[1] = AES_SBOX[temp[2]];
            temp[2] = AES_SBOX[temp[3]];
            temp[3] = AES_SBOX[t];
        }
        for (int i = 0; i < 4; ++i) {
            round_keys[bytes] = round_keys[bytes - 16] ^ temp[i];
            ++bytes;
        }
    }
}

static void aes_add_round_key(uint8_t state[16], const uint8_t *round_key) {
    for (int i = 0; i < 16; ++i) {
        state[i] ^= round_key[i];
    }
}

static void aes_sub_bytes(uint8_t state[16]) {
    for (int i = 0; i < 16; ++i) {
        state[i] = AES_SBOX[state[i]];
    }
}

static void aes_shift_rows(uint8_t state[16]) {
    uint8_t t = state[1];
    state[1] = state[5];
    state[5] = state[9];
    state[9] = state[13];
    state[13] = t;

    t = state[2];
    state[2] = state[10];
    state[10] = t;
    t = state[6];
    state[6] = state[14];
    state[14] = t;

    t = state[3];
    state[3] = state[15];
    state[15] = state[11];
    state[11] = state[7];
    state[7] = t;
}

static void aes_mix_columns(uint8_t state[16]) {
    for (int col = 0; col < 4; ++col) {
        uint8_t *s = state + col * 4;
        uint8_t a0 = s[0];
        uint8_t a1 = s[1];
        uint8_t a2 = s[2];
        uint8_t a3 = s[3];
        uint8_t t = a0 ^ a1 ^ a2 ^ a3;
        s[0] ^= t ^ aes_xtime(a0 ^ a1);
        s[1] ^= t ^ aes_xtime(a1 ^ a2);
        s[2] ^= t ^ aes_xtime(a2 ^ a3);
        s[3] ^= t ^ aes_xtime(a3 ^ a0);
    }
}

static void aes_encrypt_block(uint8_t block[16], const uint8_t round_keys[176]) {
    aes_add_round_key(block, round_keys);
    for (int round = 1; round < 10; ++round) {
        aes_sub_bytes(block);
        aes_shift_rows(block);
        aes_mix_columns(block);
        aes_add_round_key(block, round_keys + round * 16);
    }
    aes_sub_bytes(block);
    aes_shift_rows(block);
    aes_add_round_key(block, round_keys + 160);
}

__attribute__((noinline)) static void aes128_encrypt_blocks(
    const uint8_t key[16],
    const uint8_t input[32],
    uint8_t output[32]
) {
    uint8_t round_keys[176];
    aes_key_expand(key, round_keys);
    memcpy(output, input, 32);
    aes_encrypt_block(output, round_keys);
    aes_encrypt_block(output + 16, round_keys);
}

__attribute__((noinline)) static void md5_digest(const uint8_t *input, size_t len, uint8_t out[16]) {
    static const uint32_t shifts[64] = {
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
        5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
        4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
        6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    };
    static const uint32_t k[64] = {
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
        0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
        0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
        0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
        0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
        0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
        0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
    };

    uint8_t block[64];
    uint32_t m[16];
    memset(block, 0, sizeof(block));
    memcpy(block, input, len);
    block[len] = 0x80u;
    uint64_t bit_len = (uint64_t)len * 8u;
    for (int i = 0; i < 8; ++i) {
        block[56 + i] = (uint8_t)(bit_len >> (8 * i));
    }
    for (int i = 0; i < 16; ++i) {
        m[i] = load_le32(block + i * 4);
    }

    uint32_t a0 = 0x67452301u;
    uint32_t b0 = 0xefcdab89u;
    uint32_t c0 = 0x98badcfeu;
    uint32_t d0 = 0x10325476u;
    uint32_t a = a0;
    uint32_t b = b0;
    uint32_t c = c0;
    uint32_t d = d0;

    for (int i = 0; i < 64; ++i) {
        uint32_t f;
        uint32_t g;
        if (i < 16) {
            f = (b & c) | ((~b) & d);
            g = (uint32_t)i;
        } else if (i < 32) {
            f = (d & b) | ((~d) & c);
            g = (uint32_t)((5 * i + 1) & 15);
        } else if (i < 48) {
            f = b ^ c ^ d;
            g = (uint32_t)((3 * i + 5) & 15);
        } else {
            f = c ^ (b | (~d));
            g = (uint32_t)((7 * i) & 15);
        }
        uint32_t temp = d;
        d = c;
        c = b;
        b = b + rol32(a + f + k[i] + m[g], shifts[i]);
        a = temp;
    }

    store_le32(out + 0, a0 + a);
    store_le32(out + 4, b0 + b);
    store_le32(out + 8, c0 + c);
    store_le32(out + 12, d0 + d);
}

__attribute__((noinline)) static void sha256_digest(const uint8_t *input, size_t len, uint8_t out[32]) {
    static const uint32_t k[64] = {
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    };
    uint8_t block[64];
    uint32_t w[64];
    uint32_t h[8] = {
        0x6a09e667u, 0xbb67ae85u, 0x3c6ef372u, 0xa54ff53au,
        0x510e527fu, 0x9b05688cu, 0x1f83d9abu, 0x5be0cd19u,
    };

    memset(block, 0, sizeof(block));
    memcpy(block, input, len);
    block[len] = 0x80u;
    uint64_t bit_len = (uint64_t)len * 8u;
    for (int i = 0; i < 8; ++i) {
        block[63 - i] = (uint8_t)(bit_len >> (8 * i));
    }
    for (int i = 0; i < 16; ++i) {
        w[i] = load_be32(block + i * 4);
    }
    for (int i = 16; i < 64; ++i) {
        uint32_t s0 = ror32(w[i - 15], 7) ^ ror32(w[i - 15], 18) ^ (w[i - 15] >> 3);
        uint32_t s1 = ror32(w[i - 2], 17) ^ ror32(w[i - 2], 19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16] + s0 + w[i - 7] + s1;
    }

    uint32_t a = h[0];
    uint32_t b = h[1];
    uint32_t c = h[2];
    uint32_t d = h[3];
    uint32_t e = h[4];
    uint32_t f = h[5];
    uint32_t g = h[6];
    uint32_t hh = h[7];
    for (int i = 0; i < 64; ++i) {
        uint32_t s1 = ror32(e, 6) ^ ror32(e, 11) ^ ror32(e, 25);
        uint32_t ch = (e & f) ^ ((~e) & g);
        uint32_t temp1 = hh + s1 + ch + k[i] + w[i];
        uint32_t s0 = ror32(a, 2) ^ ror32(a, 13) ^ ror32(a, 22);
        uint32_t maj = (a & b) ^ (a & c) ^ (b & c);
        uint32_t temp2 = s0 + maj;
        hh = g;
        g = f;
        f = e;
        e = d + temp1;
        d = c;
        c = b;
        b = a;
        a = temp1 + temp2;
    }

    h[0] += a;
    h[1] += b;
    h[2] += c;
    h[3] += d;
    h[4] += e;
    h[5] += f;
    h[6] += g;
    h[7] += hh;
    for (int i = 0; i < 8; ++i) {
        store_be32(out + i * 4, h[i]);
    }
}

__attribute__((noinline)) static uint32_t vm_guard(const uint8_t *buf, size_t len) {
    uint32_t acc = 0x9e3779b9u;
    for (size_t i = 0; i < len; ++i) {
        acc ^= (uint32_t)buf[i] + (uint32_t)(i * 17u);
        acc = rol32(acc, 5) + 0x7f4a7c15u;
    }
    return acc;
}

__attribute__((noinline, annotate("+vm_virtualize,-flatten,-indirect_branch,-indirect_call"))) static uint32_t
vm_guard_scalar(uint32_t a, uint32_t b, uint32_t c, uint32_t d) {
    uint32_t x = a ^ (b << 5) ^ (b >> 3);
    uint32_t y = (c + 0x45d9f3bu) ^ (d >> 7) ^ (d << 3);
    uint32_t z = (x * 33u) + (y * 17u);
    return (z ^ (z >> 16)) + 0x7f4a7c15u;
}

static void print_hex(const uint8_t *buf, size_t len) {
    static const char hex[] = "0123456789abcdef";
    for (size_t i = 0; i < len; ++i) {
        putchar(hex[buf[i] >> 4]);
        putchar(hex[buf[i] & 15u]);
    }
}

static void print_u32_hex(uint32_t value) {
    static const char hex[] = "0123456789abcdef";
    for (int i = 7; i >= 0; --i) {
        putchar(hex[(value >> (i * 4)) & 15u]);
    }
}

__attribute__((annotate("-indirect_call"))) int main(void) {
    const char *message = "AMICE VMP crypto pipeline fixture";
    const uint8_t rc4_key[] = "amice-simple-vmp-rc4";
    const uint8_t aes_key[16] = {
        0x61, 0x6d, 0x69, 0x63, 0x65, 0x2d, 0x76, 0x6d,
        0x70, 0x2d, 0x61, 0x65, 0x73, 0x2d, 0x31, 0x36,
    };

    uint8_t sha1_out[20];
    uint8_t rc4_out[20];
    uint8_t aes_in[32];
    uint8_t aes_out[32];
    uint8_t md5_out[16];
    uint8_t sha256_out[32];

    sha1_digest((const uint8_t *)message, strlen(message), sha1_out);
    rc4_crypt(rc4_key, sizeof(rc4_key) - 1, sha1_out, rc4_out, sizeof(rc4_out));

    memset(aes_in, 0, sizeof(aes_in));
    memcpy(aes_in, rc4_out, sizeof(rc4_out));
    for (size_t i = sizeof(rc4_out); i < sizeof(aes_in); ++i) {
        aes_in[i] = (uint8_t)(0xa0u + i);
    }
    aes128_encrypt_blocks(aes_key, aes_in, aes_out);
    md5_digest(aes_out, sizeof(aes_out), md5_out);
    sha256_digest(md5_out, sizeof(md5_out), sha256_out);

    print_hex(sha256_out, sizeof(sha256_out));
    putchar(':');
    uint32_t folded = vm_guard(sha256_out, sizeof(sha256_out));
    folded ^= vm_guard_scalar(
        load_be32(sha256_out + 0),
        load_be32(sha256_out + 4),
        load_be32(sha256_out + 8),
        load_be32(sha256_out + 12)
    );
    print_u32_hex(folded);
    putchar('\n');
    return 0;
}
