// 文件：mba_constants_demo.c
// 说明：纯 C、无随机、单文件、常量驱动的表达式集合，覆盖 8/16/32/64 位（含有符号/无符号）。
// 注意点：
// - 使用 <stdint.h> 的精确宽度类型与 INTx_C/UINTx_C 常量宏，避免实现相关差异。
// - 避免未定义行为：不对负数做右移；移位量小于位宽；有符号运算避免溢出；循环移位在无符号类型上实现。
// - 所有表达式均仅由常量组合；输出用于人工/脚本比对。

#include <stdint.h>
#include <stdio.h>

#define U8(x)  UINT8_C(x)
#define S8(x)  INT8_C(x)
#define U16(x) UINT16_C(x)
#define S16(x) INT16_C(x)
#define U32(x) UINT32_C(x)
#define S32(x) INT32_C(x)
#define U64(x) UINT64_C(x)
#define S64(x) INT64_C(x)

// 安全循环移位（仅用于无符号类型）
static inline uint8_t  rotl8 (uint8_t  v, unsigned r){ r&=7;  return (uint8_t)((uint8_t)(v<<r)|(uint8_t)(v>>(8-r))); }
static inline uint8_t  rotr8 (uint8_t  v, unsigned r){ r&=7;  return (uint8_t)((uint8_t)(v>>r)|(uint8_t)(v<<(8-r))); }
static inline uint16_t rotl16(uint16_t v, unsigned r){ r&=15; return (uint16_t)((uint16_t)(v<<r)|(uint16_t)(v>>(16-r))); }
static inline uint16_t rotr16(uint16_t v, unsigned r){ r&=15; return (uint16_t)((uint16_t)(v>>r)|(uint16_t)(v<<(16-r))); }
static inline uint32_t rotl32(uint32_t v, unsigned r){ r&=31; return (uint32_t)((v<<r)|(v>>(32u-r))); }
static inline uint32_t rotr32(uint32_t v, unsigned r){ r&=31; return (uint32_t)((v>>r)|(v<<(32u-r))); }
static inline uint64_t rotl64(uint64_t v, unsigned r){ r&=63; return (uint64_t)((v<<r)|(v>>(64u-r))); }
static inline uint64_t rotr64(uint64_t v, unsigned r){ r&=63; return (uint64_t)((v>>r)|(v<<(64u-r))); }

static void test_u8(void){
    puts("=== uint8_t ===");
    uint8_t a = U8(0x5A);
    uint8_t b = (U8(0x12) + U8(7)) ^ U8(0xF0);
    uint8_t c = (U8(0xFF) & U8(0x3C)) | U8(0x02);
    uint8_t d = (U8(0x81) << 1);                // 0x102 -> 截断为 0x02
    uint8_t e = (U8(0x40) >> 2);
    uint8_t f = (uint8_t)(~U8(0x0F));
    uint8_t g = rotl8(U8(0x3C), 3);
    uint8_t h = rotr8(U8(0xA5), 4);
    uint8_t i = (U8(200) - U8(57));             // 143 -> 0x8F
    uint8_t j = (U8(11) * U8(7));               // 77
    uint8_t k = (U8(0x55) ^ U8(0xAA)) + (U8(1) | U8(2));
    uint8_t m = (U8(0x10) ? U8(0xFE) : U8(0x01));
    uint8_t arr[4] = {U8(0x11),U8(0x22),U8(0x33),U8(0x44)};
    uint8_t n = arr[(U8(6) & U8(3))];           // 6&3=2 -> 0x33
    printf("a=%3u 0x%02X, b=%3u 0x%02X, c=%3u 0x%02X, d=%3u 0x%02X\n", a,a,b,b,c,c,d,d);
    printf("e=%3u 0x%02X, f=%3u 0x%02X, g=%3u 0x%02X, h=%3u 0x%02X\n", e,e,f,f,g,g,h,h);
    printf("i=%3u 0x%02X, j=%3u 0x%02X, k=%3u 0x%02X, m=%3u 0x%02X, n=%3u 0x%02X\n",
           i,i,j,j,k,k,m,m,n,n);
}

static void test_s8(void){
    puts("=== int8_t ===");
    int8_t a = S8(42);
    int8_t b = (int8_t)(S8(100) - S8(27));      // 73
    int8_t c = (int8_t)(S8(12) * S8(5));        // 60
    int8_t d = (int8_t)(S8(-60) + S8(50));      // -10
    int8_t e = (int8_t)(S8(0x7F) & S8(0x3C));   // 60
    int8_t f = (int8_t)(S8(0x55) ^ S8(0x2A));   // 0x7F
    // 移位在有符号上小心：仅对非负数做右移，避免实现定义
    int8_t g = (int8_t)((uint8_t)S8(0x7C) >> 2); // 0x1F -> 31
    int8_t h = (int8_t)((uint8_t)S8(0x11) << 3); // 0x88 -> -120
    int8_t i = (S8(-1) ? S8(5) : S8(6));        // 5
    printf("a=%4d, b=%4d, c=%4d, d=%4d, e=%4d, f=%4d, g=%4d, h=%4d, i=%4d\n",
           a,b,c,d,e,f,g,h,i);
}

static void test_u16(void){
    puts("=== uint16_t ===");
    uint16_t a = U16(0x1234) + U16(0x0101);
    uint16_t b = (U16(0xFFFF) ^ U16(0x00FF));
    uint16_t c = (U16(0x0F0F) | U16(0x00F0));
    uint16_t d = (U16(0x8001) << 1);            // -> 0x0002 (溢出截断)
    uint16_t e = (U16(0x4000) >> 3);
    uint16_t f = (uint16_t)(~U16(0x00FF));
    uint16_t g = rotl16(U16(0x1337), 7);
    uint16_t h = rotr16(U16(0xBEEF), 9);
    uint16_t i = (U16(5000) - U16(1234));
    uint16_t j = (U16(123) * U16(45));
    printf("a=0x%04X b=0x%04X c=0x%04X d=0x%04X e=0x%04X f=0x%04X g=0x%04X h=0x%04X i=%u j=%u\n",
           a,b,c,d,e,f,g,h,i,j);
}

static void test_s16(void){
    puts("=== int16_t ===");
    int16_t a = S16(30000) - S16(1000);         // 29000 (安全范围)
    int16_t b = (int16_t)(S16(1234) * S16(5));  // 6170
    int16_t c = (int16_t)(S16(0x7FFF) & S16(0x0FF0));
    int16_t d = (int16_t)(S16(-32000) + S16(123)); // -31877
    int16_t e = (int16_t)((uint16_t)S16(0x7F00) >> 4); // 0x07F0 -> 2032
    int16_t f = (S16(0) ? S16(1) : S16(-1));    // -1
    printf("a=%6d b=%6d c=0x%04X d=%6d e=%6d f=%6d\n", a,b,(uint16_t)c,d,e,f);
}

static void test_u32(void){
    puts("=== uint32_t ===");
    uint32_t a = U32(0x89ABCDEF) ^ U32(0x13579BDF);
    uint32_t b = (U32(0xDEADBEEF) & U32(0x00FFFFFF)) | U32(0x11000000);
    uint32_t c = U32(0x01234567) + U32(0x89ABCDEF);
    uint32_t d = U32(0x1) << 31;                // 最高位
    uint32_t e = U32(0x80000000) >> 3;          // 逻辑右移
    uint32_t f = (uint32_t)(~U32(0x0F0F0F0F));
    uint32_t g = rotl32(U32(0x13371337), 13);
    uint32_t h = rotr32(U32(0xC001D00D), 7);
    uint32_t i = (U32(100000) * U32(300)) + (U32(1)<<5);
    uint32_t j = (U32(0xAAAAAAAA) | U32(0x55555555)) ^ U32(0xFFFFFFFF);
    printf("a=0x%08X b=0x%08X c=0x%08X d=0x%08X e=0x%08X f=0x%08X\n", a,b,c,d,e,f);
    printf("g=0x%08X h=0x%08X i=%u (0x%08X) j=0x%08X\n", g,h,i,i,j);
}

static void test_s32(void){
    puts("=== int32_t ===");
    int32_t a = S32(2000000000) - S32(123456789); // 1876543211 (安全)
    int32_t b = (int32_t)(S32(123456) * S32(7));  // 864192
    int32_t c = (int32_t)(S32(0x7FFFFFFF) & S32(0x0FFF0FFF));
    int32_t d = (int32_t)((uint32_t)S32(0x7FFF0000) >> 8); // 逻辑右移后再转回
    int32_t e = (S32(42) ? S32(-3141592) : S32(2718281));
    printf("a=%11d b=%9d c=0x%08X d=%11d e=%11d\n", a,b,(uint32_t)c,d,e);
}

static void test_u64(void){
    puts("=== uint64_t ===");
    uint64_t a = U64(0x0123456789ABCDEF) ^ U64(0xF0E1D2C3B4A59687);
    uint64_t b = (U64(0xDEADBEEFCAFEBABE) & U64(0x00000000FFFFFFFF)) | U64(0x1234567800000000);
    uint64_t c = U64(0x0000FFFF0000FFFF) + U64(0x1111111100000001);
    uint64_t d = U64(1) << 63;                   // 最高位
    uint64_t e = U64(0x8000000000000000) >> 4;   // 逻辑右移
    uint64_t f = (uint64_t)(~U64(0x00FF00FF00FF00FF));
    uint64_t g = rotl64(U64(0x0123456789ABCDEF), 29);
    uint64_t h = rotr64(U64(0xF00DBABEDEADC0DE), 17);
    uint64_t i = (U64(123456789) * U64(1000000)) + U64(0x1234);
    uint64_t j = ((U64(0xAAAAAAAAAAAAAAAA) | U64(0x5555555555555555)) ^ U64(0xFFFFFFFFFFFFFFFF));
    printf("a=0x%016llX\nb=0x%016llX\nc=0x%016llX\nd=0x%016llX\ne=0x%016llX\n",
           (unsigned long long)a,(unsigned long long)b,(unsigned long long)c,
           (unsigned long long)d,(unsigned long long)e);
    printf("f=0x%016llX\ng=0x%016llX\nh=0x%016llX\ni=%llu (0x%016llX)\nj=0x%016llX\n",
           (unsigned long long)f,(unsigned long long)g,(unsigned long long)h,
           (unsigned long long)i,(unsigned long long)i,(unsigned long long)j);
}

static void test_s64(void){
    puts("=== int64_t ===");
    int64_t a = S64(4000000000000000000) - S64(1234567890123456789); // 安全
    int64_t b = (int64_t)(S64(123456789) * S64(9876));               // 安全
    int64_t c = (int64_t)(S64(0x7FFFFFFFFFFFFFFF) & S64(0x0000FFFF0000FFFF));
    // 逻辑右移在无符号上进行再转回
    int64_t d = (int64_t)((uint64_t)S64(0x7FFF000000000000) >> 12);
    int64_t e = (S64(-1) ? S64(-9223372036854775807) : S64(123));   // 避免用 INT64_MIN 直接字面量相加减
    printf("a=%20lld\nb=%20lld\nc=0x%016llX\nd=%20lld\ne=%20lld\n",
           (long long)a,(long long)b,(unsigned long long)c,(long long)d,(long long)e);
}

static void test_mixed_width(void){
    puts("=== Mixed width ===");
    // 不同宽度的常量混合与显式收缩
    uint16_t a = (uint16_t)(U8(200) + U16(1000));          // 1200
    uint8_t  b = (uint8_t)((U16(0x1234) + U32(0x89)) & U16(0x00FF)); // 0xBD -> 189
    uint32_t c = (uint32_t)(U64(0x1FFFF) * U32(3));        // 0x5FFFD
    uint64_t d = (uint64_t)(U32(0xDEADBEEF) | U64(0xF000000000000000));
    uint8_t  e = (uint8_t)((U32(0x0000ABCD) >> 2) & U32(0xFF));
    int16_t  f = (int16_t)((S32(30000) - S32(123)) & S32(0x7FFF));   // 保证非负
    uint32_t g = (uint32_t)((U8(0xF0) << 20) | (U16(0x0AAA) << 4) | (U8(0x0F)));
    printf("a=%u b=%u c=%u d=0x%016llX e=%u f=%d g=0x%08X\n",
           a,b,c,(unsigned long long)d,e,f,g);
}

int main(void){
    test_u8();
    test_s8();
    test_u16();
    test_s16();
    test_u32();
    test_s32();
    test_u64();
    test_s64();
    test_mixed_width();
    return 0;
}