// C
/*
 * md5.c - 自包含的 MD5 实现与示例
 * 编译:
 *   gcc -std=c11 -O2 -Wall -Wextra -o md5 md5.c
 * 运行:
 *   ./md5
 */

#include <stdio.h>
#include <stdint.h>
#include <string.h>

typedef struct {
    uint32_t state[4];   // A, B, C, D
    uint64_t bitlen;     // 总比特长度
    uint8_t  buffer[64]; // 512-bit 缓冲
    size_t   buflen;     // 缓冲中已有字节数
} MD5_CTX;

/* 左循环 */
static inline uint32_t ROTL(uint32_t x, uint32_t n) {
    return (x << n) | (x >> (32 - n));
}

/* 基本函数 */
#define F(x,y,z) (((x) & (y)) | ((~(x)) & (z)))
#define G(x,y,z) (((x) & (z)) | ((y) & (~(z))))
#define H(x,y,z) ((x) ^ (y) ^ (z))
#define I(x,y,z) ((y) ^ ((x) | (~(z))))

/* 每轮操作 */
#define STEP(f, a, b, c, d, x, t, s) \
    (a) += f((b), (c), (d)) + (x) + (t); \
    (a) = (b) + ROTL((a), (s))

/* 小端读取 32 位 */
static inline uint32_t le32load(const uint8_t p[4]) {
    return (uint32_t)p[0]
         | ((uint32_t)p[1] << 8)
         | ((uint32_t)p[2] << 16)
         | ((uint32_t)p[3] << 24);
}

/* 小端写出 32 位 */
static inline void le32store(uint8_t p[4], uint32_t v) {
    p[0] = (uint8_t)(v);
    p[1] = (uint8_t)(v >> 8);
    p[2] = (uint8_t)(v >> 16);
    p[3] = (uint8_t)(v >> 24);
}

static void md5_transform(uint32_t s[4], const uint8_t block[64]) {
    uint32_t a = s[0], b = s[1], c = s[2], d = s[3];
    uint32_t x[16];
    for (int i = 0; i < 16; ++i) {
        x[i] = le32load(block + i * 4);
    }

    // T 常量 (前 64 个正弦值 * 2^32 的整数部分)
    static const uint32_t T[64] = {
        0xd76aa478,0xe8c7b756,0x242070db,0xc1bdceee,0xf57c0faf,0x4787c62a,0xa8304613,0xfd469501,
        0x698098d8,0x8b44f7af,0xffff5bb1,0x895cd7be,0x6b901122,0xfd987193,0xa679438e,0x49b40821,
        0xf61e2562,0xc040b340,0x265e5a51,0xe9b6c7aa,0xd62f105d,0x02441453,0xd8a1e681,0xe7d3fbc8,
        0x21e1cde6,0xc33707d6,0xf4d50d87,0x455a14ed,0xa9e3e905,0xfcefa3f8,0x676f02d9,0x8d2a4c8a,
        0xfffa3942,0x8771f681,0x6d9d6122,0xfde5380c,0xa4beea44,0x4bdecfa9,0xf6bb4b60,0xbebfbc70,
        0x289b7ec6,0xeaa127fa,0xd4ef3085,0x04881d05,0xd9d4d039,0xe6db99e5,0x1fa27cf8,0xc4ac5665,
        0xf4292244,0x432aff97,0xab9423a7,0xfc93a039,0x655b59c3,0x8f0ccc92,0xffeff47d,0x85845dd1,
        0x6fa87e4f,0xfe2ce6e0,0xa3014314,0x4e0811a1,0xf7537e82,0xbd3af235,0x2ad7d2bb,0xeb86d391
    };

    // 第 1 轮
    STEP(F,a,b,c,d,x[ 0],T[ 0], 7); STEP(F,d,a,b,c,x[ 1],T[ 1],12);
    STEP(F,c,d,a,b,x[ 2],T[ 2],17); STEP(F,b,c,d,a,x[ 3],T[ 3],22);
    STEP(F,a,b,c,d,x[ 4],T[ 4], 7); STEP(F,d,a,b,c,x[ 5],T[ 5],12);
    STEP(F,c,d,a,b,x[ 6],T[ 6],17); STEP(F,b,c,d,a,x[ 7],T[ 7],22);
    STEP(F,a,b,c,d,x[ 8],T[ 8], 7); STEP(F,d,a,b,c,x[ 9],T[ 9],12);
    STEP(F,c,d,a,b,x[10],T[10],17); STEP(F,b,c,d,a,x[11],T[11],22);
    STEP(F,a,b,c,d,x[12],T[12], 7); STEP(F,d,a,b,c,x[13],T[13],12);
    STEP(F,c,d,a,b,x[14],T[14],17); STEP(F,b,c,d,a,x[15],T[15],22);

    // 第 2 轮
    STEP(G,a,b,c,d,x[ 1],T[16], 5); STEP(G,d,a,b,c,x[ 6],T[17], 9);
    STEP(G,c,d,a,b,x[11],T[18],14); STEP(G,b,c,d,a,x[ 0],T[19],20);
    STEP(G,a,b,c,d,x[ 5],T[20], 5); STEP(G,d,a,b,c,x[10],T[21], 9);
    STEP(G,c,d,a,b,x[15],T[22],14); STEP(G,b,c,d,a,x[ 4],T[23],20);
    STEP(G,a,b,c,d,x[ 9],T[24], 5); STEP(G,d,a,b,c,x[14],T[25], 9);
    STEP(G,c,d,a,b,x[ 3],T[26],14); STEP(G,b,c,d,a,x[ 8],T[27],20);
    STEP(G,a,b,c,d,x[13],T[28], 5); STEP(G,d,a,b,c,x[ 2],T[29], 9);
    STEP(G,c,d,a,b,x[ 7],T[30],14); STEP(G,b,c,d,a,x[12],T[31],20);

    // 第 3 轮
    STEP(H,a,b,c,d,x[ 5],T[32], 4); STEP(H,d,a,b,c,x[ 8],T[33],11);
    STEP(H,c,d,a,b,x[11],T[34],16); STEP(H,b,c,d,a,x[14],T[35],23);
    STEP(H,a,b,c,d,x[ 1],T[36], 4); STEP(H,d,a,b,c,x[ 4],T[37],11);
    STEP(H,c,d,a,b,x[ 7],T[38],16); STEP(H,b,c,d,a,x[10],T[39],23);
    STEP(H,a,b,c,d,x[13],T[40], 4); STEP(H,d,a,b,c,x[ 0],T[41],11);
    STEP(H,c,d,a,b,x[ 3],T[42],16); STEP(H,b,c,d,a,x[ 6],T[43],23);
    STEP(H,a,b,c,d,x[ 9],T[44], 4); STEP(H,d,a,b,c,x[12],T[45],11);
    STEP(H,c,d,a,b,x[15],T[46],16); STEP(H,b,c,d,a,x[ 2],T[47],23);

    // 第 4 轮
    STEP(I,a,b,c,d,x[ 0],T[48], 6); STEP(I,d,a,b,c,x[ 7],T[49],10);
    STEP(I,c,d,a,b,x[14],T[50],15); STEP(I,b,c,d,a,x[ 5],T[51],21);
    STEP(I,a,b,c,d,x[12],T[52], 6); STEP(I,d,a,b,c,x[ 3],T[53],10);
    STEP(I,c,d,a,b,x[10],T[54],15); STEP(I,b,c,d,a,x[ 1],T[55],21);
    STEP(I,a,b,c,d,x[ 8],T[56], 6); STEP(I,d,a,b,c,x[15],T[57],10);
    STEP(I,c,d,a,b,x[ 6],T[58],15); STEP(I,b,c,d,a,x[13],T[59],21);
    STEP(I,a,b,c,d,x[ 4],T[60], 6); STEP(I,d,a,b,c,x[11],T[61],10);
    STEP(I,c,d,a,b,x[ 2],T[62],15); STEP(I,b,c,d,a,x[ 9],T[63],21);

    s[0] += a; s[1] += b; s[2] += c; s[3] += d;
}

static void md5_init(MD5_CTX* ctx) {
    ctx->state[0] = 0x67452301u;
    ctx->state[1] = 0xefcdab89u;
    ctx->state[2] = 0x98badcfeu;
    ctx->state[3] = 0x10325476u;
    ctx->bitlen   = 0;
    ctx->buflen   = 0;
}

static void md5_update(MD5_CTX* ctx, const void* data, size_t len) {
    const uint8_t* p = (const uint8_t*)data;
    ctx->bitlen += (uint64_t)len * 8;

    while (len > 0) {
        size_t space = 64 - ctx->buflen;
        size_t take = (len < space) ? len : space;
        memcpy(ctx->buffer + ctx->buflen, p, take);
        ctx->buflen += take;
        p += take;
        len -= take;

        if (ctx->buflen == 64) {
            md5_transform(ctx->state, ctx->buffer);
            ctx->buflen = 0;
        }
    }
}

static void md5_final(MD5_CTX* ctx, uint8_t out[16]) {
    // 填充：0x80 后跟 0x00，直到剩余 8 字节放长度
    uint8_t pad = 0x80;
    md5_update(ctx, &pad, 1);
    uint8_t zero = 0x00;
    while (ctx->buflen != 56) {
        if (ctx->buflen == 64) {
            md5_transform(ctx->state, ctx->buffer);
            ctx->buflen = 0;
        }
        md5_update(ctx, &zero, 1);
    }

    // 附加长度（小端 64 位）
    uint8_t lenle[8];
    uint64_t bitlen = ctx->bitlen;
    for (int i = 0; i < 8; ++i) {
        lenle[i] = (uint8_t)(bitlen >> (8 * i));
    }
    md5_update(ctx, lenle, 8);

    // 导出摘要（A,B,C,D 小端）
    for (int i = 0; i < 4; ++i) {
        le32store(out + 4*i, ctx->state[i]);
    }
}

static void md5(const void* data, size_t len, uint8_t out[16]) {
    MD5_CTX ctx;
    md5_init(&ctx);
    md5_update(&ctx, data, len);
    md5_final(&ctx, out);
}

static void md5_hex(const void* data, size_t len, char hex[33]) {
    static const char* digits = "0123456789abcdef";
    uint8_t digest[16];
    md5(data, len, digest);
    for (int i = 0; i < 16; ++i) {
        hex[2*i]   = digits[(digest[i] >> 4) & 0xF];
        hex[2*i+1] = digits[digest[i] & 0xF];
    }
    hex[32] = '\0';
}

int __attribute__((annotate("+vmf"))) main(void) {
    const char* tests[] = {
        "", "a", "abc", "message digest",
        "abcdefghijklmnopqrstuvwxyz",
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
        "1234567890"
    };
    const size_t n = sizeof(tests)/sizeof(tests[0]);

    for (size_t i = 0; i < n; ++i) {
        char hex[33];
        md5_hex(tests[i], strlen(tests[i]), hex);
        printf("MD5(\"%s\") = %s\n", tests[i], hex);
    }

    const uint8_t data_bin[] = {0x00, 0x01, 0x02, 0xFF};
    char hex_bin[33];
    md5_hex(data_bin, sizeof(data_bin), hex_bin);
    printf("MD5([00 01 02 FF]) = %s\n", hex_bin);

    return 0;
}