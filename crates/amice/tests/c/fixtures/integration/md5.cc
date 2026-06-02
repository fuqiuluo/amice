#include <iostream>
#include <string>
#include <iomanip>
#include <sstream>
#include <cstring>
#include <cstdio>
#include <cstdint>

class MD5 {
private:
    // MD5常量
    static const uint32_t S[64];
    static const uint32_t K[64];

    // MD5状态
    uint32_t h[4];
    uint64_t totalLength;
    uint8_t buffer[64];
    size_t bufferLength;

    // 辅助函数
    static uint32_t F(uint32_t x, uint32_t y, uint32_t z) {
        return (x & y) | (~x & z);
    }

    static uint32_t G(uint32_t x, uint32_t y, uint32_t z) {
        return (x & z) | (y & ~z);
    }

    static uint32_t H(uint32_t x, uint32_t y, uint32_t z) {
        return x ^ y ^ z;
    }

    static uint32_t I(uint32_t x, uint32_t y, uint32_t z) {
        return y ^ (x | ~z);
    }

    static uint32_t rotateLeft(uint32_t value, int shift) {
        return (value << shift) | (value >> (32 - shift));
    }

    void processBlock(const uint8_t* block) {
        uint32_t w[16];
        for (int i = 0; i < 16; i++) {
            w[i] = block[i*4] | (block[i*4+1] << 8) | (block[i*4+2] << 16) | (block[i*4+3] << 24);
        }

        uint32_t a = h[0], b = h[1], c = h[2], d = h[3];

        // Round 1
        for (int i = 0; i < 16; i++) {
            uint32_t f = F(b, c, d);
            uint32_t temp = d;
            d = c;
            c = b;
            b = b + rotateLeft(a + f + K[i] + w[i], S[i]);
            a = temp;
        }

        // Round 2
        for (int i = 16; i < 32; i++) {
            uint32_t f = G(b, c, d);
            uint32_t temp = d;
            d = c;
            c = b;
            b = b + rotateLeft(a + f + K[i] + w[(5*i + 1) % 16], S[i]);
            a = temp;
        }

        // Round 3
        for (int i = 32; i < 48; i++) {
            uint32_t f = H(b, c, d);
            uint32_t temp = d;
            d = c;
            c = b;
            b = b + rotateLeft(a + f + K[i] + w[(3*i + 5) % 16], S[i]);
            a = temp;
        }

        // Round 4
        for (int i = 48; i < 64; i++) {
            uint32_t f = I(b, c, d);
            uint32_t temp = d;
            d = c;
            c = b;
            b = b + rotateLeft(a + f + K[i] + w[(7*i) % 16], S[i]);
            a = temp;
        }

        h[0] += a;
        h[1] += b;
        h[2] += c;
        h[3] += d;
    }

public:
    MD5() {
        reset();
    }

    void reset() {
        h[0] = 0x67452301;
        h[1] = 0xEFCDAB89;
        h[2] = 0x98BADCFE;
        h[3] = 0x10325476;
        totalLength = 0;
        bufferLength = 0;
    }

    void update(const uint8_t* data, size_t length) {
        totalLength += length;

        while (length > 0) {
            size_t toCopy = std::min(length, 64 - bufferLength);
            memcpy(buffer + bufferLength, data, toCopy);
            bufferLength += toCopy;
            data += toCopy;
            length -= toCopy;

            if (bufferLength == 64) {
                processBlock(buffer);
                bufferLength = 0;
            }
        }
    }

    void update(const std::string& str) {
        update(reinterpret_cast<const uint8_t*>(str.c_str()), str.length());
    }

    std::string finalize() {
        // 添加填充
        uint8_t padding[64];
        size_t paddingLength = (55 - bufferLength) % 64 + 1;
        padding[0] = 0x80;
        for (size_t i = 1; i < paddingLength; i++) {
            padding[i] = 0;
        }
        update(padding, paddingLength);

        // 添加长度
        uint64_t bitLength = totalLength * 8;
        uint8_t lengthBytes[8];
        for (int i = 0; i < 8; i++) {
            lengthBytes[i] = (bitLength >> (i * 8)) & 0xFF;
        }
        update(lengthBytes, 8);

        // 生成最终哈希值
        std::stringstream ss;
        for (int i = 0; i < 4; i++) {
            for (int j = 0; j < 4; j++) {
                ss << std::hex << std::setw(2) << std::setfill('0')
                   << ((h[i] >> (j * 8)) & 0xFF);
            }
        }

        return ss.str();
    }

    static std::string hash(const std::string& input) {
        MD5 md5;
        md5.update(input);
        return md5.finalize();
    }

    static std::string hash(const uint8_t* data, size_t length) {
        MD5 md5;
        md5.update(data, length);
        return md5.finalize();
    }
};

// C风格接口函数
void md5_hex(const void* data, size_t length, char* hex_output) {
    MD5 md5;
    md5.update(static_cast<const uint8_t*>(data), length);
    std::string result = md5.finalize();
    strcpy(hex_output, result.c_str());
}

// MD5常量定义
const uint32_t MD5::S[64] = {
    7, 12, 17, 22,  7, 12, 17, 22,  7, 12, 17, 22,  7, 12, 17, 22,
    5,  9, 14, 20,  5,  9, 14, 20,  5,  9, 14, 20,  5,  9, 14, 20,
    4, 11, 16, 23,  4, 11, 16, 23,  4, 11, 16, 23,  4, 11, 16, 23,
    6, 10, 15, 21,  6, 10, 15, 21,  6, 10, 15, 21,  6, 10, 15, 21
};

const uint32_t MD5::K[64] = {
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391
};

int main() {
    // 标准测试用例
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

    printf("\n");

    // 二进制数据测试
    const uint8_t data_bin[] = {0x00, 0x01, 0x02, 0xFF};
    char hex_bin[33];
    md5_hex(data_bin, sizeof(data_bin), hex_bin);
    printf("MD5([00 01 02 FF]) = %s\n", hex_bin);

    return 0;
}