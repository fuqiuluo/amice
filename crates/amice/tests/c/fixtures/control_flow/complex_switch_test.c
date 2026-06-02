// complex_switch_test.c
// 构建：clang -std=c11 -O2 complex_switch_test.c -o complex_switch_test
// 运行示例：./complex_switch_test 123 foo 999 Z

#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <limits.h>

#if defined(__clang__) || defined(__GNUC__)
// 标注有意的 fallthrough，避免告警（在构建时可加 -Wimplicit-fallthrough）
#define FALLTHROUGH __attribute__((fallthrough))
#else
#define FALLTHROUGH
#endif

static volatile int g_volatile_bias = 0;

static inline uint64_t mix64(uint64_t x) {
    // 简单的 64 位混合函数，避免被过度优化
    x ^= x >> 33;
    x *= 0xff51afd7ed558ccdULL;
    x ^= x >> 33;
    x *= 0xc4ceb9fe1a85ec53ULL;
    x ^= x >> 33;
    return x;
}

static uint32_t fnv1a_32(const char* s) {
    // 简单 FNV-1a 变体
    uint32_t h = 2166136261u;
    while (*s) {
        h ^= (unsigned char)*s++;
        h *= 16777619u;
    }
    // 注入全局扰动，避免常量行为
    h ^= (uint32_t)g_volatile_bias;
    return h;
}

static int switch_char_class(int c) {
    int score = 0;
#if defined(__clang__) || defined(__GNUC__)
    // GNU 扩展：case 范围
    switch (c) {
        case '0'...'9': score = 1; break;
        case 'A'...'Z': score = 2; break;
        case 'a'...'z': score = 3; break;
        case '\n': score = -1; break;
        default: score = 0; break;
    }
#else
    // 兼容路径（无范围 case）
    switch (c) {
        case '\n': score = -1; break;
        default:
            if (c >= '0' && c <= '9') score = 1;
            else if (c >= 'A' && c <= 'Z') score = 2;
            else if (c >= 'a' && c <= 'z') score = 3;
            else score = 0;
            break;
    }
#endif
    return score;
}

static int switch_sparse_int(int x) {
    // 稀疏/极值 case 集合
    int acc = 0;
    switch (x) {
        case INT_MIN: acc = -100000; break;
        case -123456: acc = -123; break;
        case -1024: acc = -10; break;
        case -1: acc = -1; break;
        case 0: acc = 0; break;
        case 1: acc = 1; break;
        case 2: acc = 3; FALLTHROUGH;
        case 3: acc += 4; FALLTHROUGH;
        case 7: acc += 8; break; // 有意的 fallthrough 链
        case 42: acc = 420; break;
        case 100: acc = 100; break;
        case 255: acc = 255; break;
        case 256: acc = 256; break;
        case 511: acc = 511; break;
        case 512: acc = 512; break;
        case 1000: acc = 1000; break;
        case 4096: acc = 4096; break;
        case 65535: acc = 65535; break;
        case 65536: acc = 65536; break;
        case 1000000: acc = 1000000; break;
        case INT_MAX: acc = 100000; break;
        default:
            // 嵌套 switch（根据低 3 位进行分派）
            switch (x & 7) {
                case 0: acc = x ^ 0xA5A5; break;
                case 1: acc = x + 17; break;
                case 2: acc = x - 23; break;
                case 3: acc = x * 3; break;
                case 4: acc = (x << 1) ^ (x >> 1); break;
                case 5: acc = ~x; break;
                case 6: acc = x / (x ? 3 : 1); break;
                default: acc = x; break;
            }
            break;
    }
    return acc;
}

static uint64_t switch_u64(uint64_t v) {
    // 覆盖 64 位 case，含极值
    uint64_t r = 0;
    switch (v) {
        case 0ULL: r = 0; break;
        case 1ULL: r = 10; break;
        case 2ULL: r = 20; break;
        case 3ULL: r = 30; break;
        case 10ULL: r = 100; break;
        case 100ULL: r = 1000; break;
        case (1ULL<<32): r = 0xDEADBEEFDEADBEEFULL; break;
        case (1ULL<<32) + 1ULL: r = 0xABCDEF0123456789ULL; break;
        case (1ULL<<48): r = 0x123456789ABCDEF0ULL; break;
        case (1ULL<<63): r = 0x8000000000000000ULL; break;
        case 18446744073709551615ULL: r = 0xFFFFFFFFFFFFFFFFULL; break; // UINT64_MAX
        default:
            // 嵌套：再根据高位 bit 数量进行辅助分派
            {
                int high = (v >> 60) & 0xF;
                switch (high) {
                    case 0: r = v ^ 0xC0FFEEULL; break;
                    case 1: r = mix64(v); break;
                    case 2: r = v * 7 + 13; break;
                    default: r = mix64(v ^ 0x9E3779B97F4A7C15ULL); break;
                }
            }
            break;
    }
    return r;
}

typedef int (*op_fn)(int, int);

static int op_add(int a, int b) { return a + b; }
static int op_sub(int a, int b) { return a - b; }
static int op_mul(int a, int b) { return a * b; }
static int op_xor(int a, int b) { return a ^ b; }
static int op_max(int a, int b) { return a > b ? a : b; }

static int small_vm_run(const int* code, size_t n, int init) {
    // 一个小 VM：使用 switch 实现 opcode 分派（稠密+稀疏混合）
    static op_fn fns[8] = { op_add, op_sub, op_mul, op_xor, op_max, op_add, op_xor, op_sub };
    int acc = init;
    for (size_t i = 0; i < n; ++i) {
        int op = code[i] ^ g_volatile_bias; // 注入扰动
        switch (op) {
            case 0: // NOP
                FALLTHROUGH;
            case 100: // 别名到 NOP
                acc += 0;
                break;
            case 1: // ADD
                acc = fns[0](acc, (int)i);
                break;
            case 2: // SUB
                acc = fns[1](acc, (int)(i * 3));
                break;
            case 3: // MUL
                acc = fns[2](acc, (int)(i % 7 + 1));
                break;
            case 4: // XOR
                acc = fns[3](acc, (int)(0x55AA00FF ^ (int)i));
                break;
            case 5: // MAX
                acc = fns[4](acc, (int)(i * i));
                break;
            case 7: // 稀疏
                acc ^= 0x77777777;
                break;
            case 13: // 稀疏
                acc += 1337;
                FALLTHROUGH;
            case 14: // 紧邻
                acc ^= 0x31415926;
                break;
            case 255: // 稀疏高值
                acc -= 999;
                break;
            default:
                // default-only 子 switch，用于进一步稀释分派
                switch (op & 3) {
                    default: acc = (acc << 1) ^ (acc >> 1) ^ op; break;
                }
                break;
        }
    }
    return acc;
}

static int string_switch_like(const char* s) {
    // 使用哈希做“字符串 switch”，先 switch 哈希，再进行字符串确认以消歧
    uint32_t h = fnv1a_32(s);
    int r = -1;
    switch (h) {
        case 0x811C9DC5u ^ 0x00000000u: // 不会命中（示例值）
            r = -2;
            break;
        case 0xE8A3F197u: // 可能与 "foo" 或 "oof" 冲突（示例）
        case 0xB5FA0C3Du:
        case 0xC9F53E3Cu:
            if (strcmp(s, "foo") == 0) { r = 10; break; }
            if (strcmp(s, "bar") == 0) { r = 20; break; }
            if (strcmp(s, "baz") == 0) { r = 30; break; }
            // 冲突但不匹配，继续 default
            FALLTHROUGH;
        default:
            // 再次进行基于长度的嵌套 switch
            switch ((int)strlen(s)) {
                case 0: r = 0; break;
                case 1: r = (unsigned char)s[0]; break;
                case 2: r = s[0] + s[1]; break;
                case 3: r = s[0] * 3 + s[1] * 5 + s[2] * 7; break;
                default: r = (int)(h ^ 0xA5A5A5A5u); break;
            }
            break;
    }
    return r;
}

static int only_default_switch(int x) {
    // 只有 default 的 switch：有些实现会直接降级为 if-goto，检验你的 pass 路径
    int r = 0;
    switch (x) {
        default:
            r = (x ^ 0x5A5A5A5A) + 1;
            break;
    }
    return r;
}

int main(int argc, char** argv) {
    // 用命令行扰动，避免被完全常量折叠
    int bias = (argc > 1) ? atoi(argv[1]) : 12345;
    g_volatile_bias = bias;

    const char* s = (argc > 2) ? argv[2] : "foo";
    int x_in = (argc > 3) ? atoi(argv[3]) : 999999;
    int ch = (argc > 4) ? (unsigned char)argv[4][0] : 'Z';

    // 1) 字符分类
    int cls = switch_char_class(ch);

    // 2) 稀疏 int
    int si = switch_sparse_int(x_in ^ g_volatile_bias);

    // 3) 64 位
    uint64_t big = (uint64_t)( (uint64_t)(unsigned)x_in << 32 ) ^ (uint64_t)(unsigned)bias;
    uint64_t su = switch_u64(big ^ 0xDEADBEEFCAFEBABEULL);

    // 4) 小 VM
    int program[] = {0,1,2,3,4,5,7,13,14,255,100,42,6,9};
    int vm = small_vm_run(program, sizeof(program)/sizeof(program[0]), 17);

    // 5) 字符串“switch”
    int ss = string_switch_like(s);

    // 6) 只有 default 的 switch
    int od = only_default_switch(x_in);

    // 7) 循环中的嵌套 switch（达夫设备风格混合）
    int n = (x_in & 63) + 37;
    int acc = 0;
    int i = 0;
    switch (n & 7) {
        case 0: do { acc += switch_sparse_int(i + bias); FALLTHROUGH;
        case 7:      acc ^= switch_char_class((i + ch) & 0xFF); FALLTHROUGH;
        case 6:      acc += (int)(switch_u64(mix64((uint64_t)i)) & 0xFFFF); FALLTHROUGH;
        case 5:      acc ^= only_default_switch(i ^ x_in); FALLTHROUGH;
        case 4:      acc += string_switch_like(s); FALLTHROUGH;
        case 3:      acc ^= small_vm_run(program, 5, i); FALLTHROUGH;
        case 2:      acc += (i * 3) ^ 0x1234; FALLTHROUGH;
        case 1:      acc ^= (i << 2) + 7;
                    i++;
            } while (i < n);
    }

    // 输出结果，防止被完全 DCE
    printf("bias=%d ch=%c x_in=%d\n", bias, (char)ch, x_in);
    printf("char_class=%d sparse_int=%d u64=%llu\n", cls, si, (unsigned long long)su);
    printf("vm=%d str_switch=%d only_default=%d loop_acc=%d\n", vm, ss, od, acc);

    // 返回一个组合值
    unsigned long long rc = (unsigned long long)
        ( (cls & 0xFF)
        ^ ((si & 0xFFFF) << 1)
        ^ ((unsigned)(su & 0xFFFFFF) << 3)
        ^ ((vm & 0xFFFF) << 5)
        ^ ((ss & 0xFFFF) << 7)
        ^ ((od & 0xFFFF) << 11)
        ^ ((acc & 0xFFFF) << 13) );

    return (int)(rc & 0x7FFFFFFF);
}

//bias=12345 ch=Z x_in=999999
//char_class=2 sparse_int=337410 u64=17189095738507770792
//vm=99101413 str_switch=1638 only_default=1515526246 loop_acc=1513856150