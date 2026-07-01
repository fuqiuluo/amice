#include <stdio.h>

#define VMP __attribute__((noinline, annotate("+vm_virtualize")))

VMP int vm_mix(int a, int b) {
    int c = (a + b) * 3;
    c = (c ^ a) & 1023;
    return c - b;
}

VMP int vm_branch(int x, int y) {
    if (x > y) {
        return x - y;
    }
    return y - x + 1;
}

VMP int vm_loop(int n) {
    int acc = 0;
    int i = 0;
    while (i < n) {
        acc += (i ^ n) & 7;
        i += 1;
    }
    return acc;
}

VMP int vm_switch(int x) {
    switch (x & 31) {
    case 1:
        return x + 3;
    case 7:
        return x * 2;
    case 19:
        return x - 5;
    default:
        return x ^ 13;
    }
}

VMP int vm_memory(int x) {
    int slots[3];
    slots[0] = x + 1;
    slots[1] = slots[0] * 2;
    slots[2] = slots[1] - x;
    return slots[2] + slots[0];
}

VMP void vm_void_pointer(int *slots, int n) {
    int i = 0;
    while (i < n) {
        slots[i] = (slots[i] + i) ^ n;
        i += 1;
    }
}

VMP long vm_ptr_roundtrip(int *slots, int index) {
    int *p = &slots[index];
    long bits = (long)p;
    int *q = (int *)bits;
    return (long)(*q + index);
}

VMP int vm_dynamic_gep2(int matrix[4][5], int i, int j) {
    return matrix[i][j] + matrix[j][i];
}

VMP int vm_reg_reuse_chain(int x, int y) {
    unsigned salt = (unsigned)y | 1u;
    unsigned a = (unsigned)x + salt;
    a = ((a << 1) ^ (salt + 1u)) + 3u;
    a = ((a << 1) ^ (salt + 2u)) + 5u;
    a = ((a << 1) ^ (salt + 3u)) + 7u;
    a = ((a << 1) ^ (salt + 4u)) + 11u;
    a = ((a << 1) ^ (salt + 5u)) + 13u;
    a = ((a << 1) ^ (salt + 6u)) + 17u;
    a = ((a << 1) ^ (salt + 7u)) + 19u;
    a = ((a << 1) ^ (salt + 8u)) + 23u;
    a = ((a << 1) ^ (salt + 9u)) + 29u;
    a = ((a << 1) ^ (salt + 10u)) + 31u;
    a = ((a << 1) ^ (salt + 11u)) + 37u;
    a = ((a << 1) ^ (salt + 12u)) + 41u;
    a = ((a << 1) ^ (salt + 13u)) + 43u;
    a = ((a << 1) ^ (salt + 14u)) + 47u;
    a = ((a << 1) ^ (salt + 15u)) + 53u;
    a = ((a << 1) ^ (salt + 16u)) + 59u;
    a = ((a << 1) ^ (salt + 17u)) + 61u;
    a = ((a << 1) ^ (salt + 18u)) + 67u;
    a = ((a << 1) ^ (salt + 19u)) + 71u;
    a = ((a << 1) ^ (salt + 20u)) + 73u;
    a = ((a << 1) ^ (salt + 21u)) + 79u;
    a = ((a << 1) ^ (salt + 22u)) + 83u;
    a = ((a << 1) ^ (salt + 23u)) + 89u;
    a = ((a << 1) ^ (salt + 24u)) + 97u;
    a = ((a << 1) ^ (salt + 25u)) + 101u;
    a = ((a << 1) ^ (salt + 26u)) + 103u;
    a = ((a << 1) ^ (salt + 27u)) + 107u;
    a = ((a << 1) ^ (salt + 28u)) + 109u;
    a = ((a << 1) ^ (salt + 29u)) + 113u;
    a = ((a << 1) ^ (salt + 30u)) + 127u;
    a = ((a << 1) ^ (salt + 31u)) + 131u;
    a = ((a << 1) ^ (salt + 32u)) + 137u;
    a = ((a << 1) ^ (salt + 33u)) + 139u;
    a = ((a << 1) ^ (salt + 34u)) + 149u;
    a = ((a << 1) ^ (salt + 35u)) + 151u;
    a = ((a << 1) ^ (salt + 36u)) + 157u;
    return (int)(a & 0x7fffffffu);
}

VMP int vm_multiblock_reuse(int x) {
    unsigned salt = (unsigned)x | 3u;
    if ((x & 1) != 0) {
        unsigned a = salt + 1u;
        a = ((a << 1) ^ (salt + 2u)) + 3u;
        a = ((a << 1) ^ (salt + 4u)) + 5u;
        a = ((a << 1) ^ (salt + 6u)) + 7u;
        a = ((a << 1) ^ (salt + 8u)) + 11u;
        a = ((a << 1) ^ (salt + 10u)) + 13u;
        a = ((a << 1) ^ (salt + 12u)) + 17u;
        a = ((a << 1) ^ (salt + 14u)) + 19u;
        a = ((a << 1) ^ (salt + 16u)) + 23u;
        a = ((a << 1) ^ (salt + 18u)) + 29u;
        a = ((a << 1) ^ (salt + 20u)) + 31u;
        a = ((a << 1) ^ (salt + 22u)) + 37u;
        a = ((a << 1) ^ (salt + 24u)) + 41u;
        a = ((a << 1) ^ (salt + 26u)) + 43u;
        a = ((a << 1) ^ (salt + 28u)) + 47u;
        a = ((a << 1) ^ (salt + 30u)) + 53u;
        a = ((a << 1) ^ (salt + 32u)) + 59u;
        a = ((a << 1) ^ (salt + 34u)) + 61u;
        a = ((a << 1) ^ (salt + 36u)) + 67u;
        return (int)(a & 0x7fffffffu);
    }

    unsigned b = salt ^ 7u;
    b = ((b << 1) + (salt ^ 2u)) ^ 3u;
    b = ((b << 1) + (salt ^ 4u)) ^ 5u;
    b = ((b << 1) + (salt ^ 6u)) ^ 7u;
    b = ((b << 1) + (salt ^ 8u)) ^ 11u;
    b = ((b << 1) + (salt ^ 10u)) ^ 13u;
    b = ((b << 1) + (salt ^ 12u)) ^ 17u;
    b = ((b << 1) + (salt ^ 14u)) ^ 19u;
    b = ((b << 1) + (salt ^ 16u)) ^ 23u;
    b = ((b << 1) + (salt ^ 18u)) ^ 29u;
    b = ((b << 1) + (salt ^ 20u)) ^ 31u;
    b = ((b << 1) + (salt ^ 22u)) ^ 37u;
    b = ((b << 1) + (salt ^ 24u)) ^ 41u;
    b = ((b << 1) + (salt ^ 26u)) ^ 43u;
    b = ((b << 1) + (salt ^ 28u)) ^ 47u;
    b = ((b << 1) + (salt ^ 30u)) ^ 53u;
    b = ((b << 1) + (salt ^ 32u)) ^ 59u;
    b = ((b << 1) + (salt ^ 34u)) ^ 61u;
    b = ((b << 1) + (salt ^ 36u)) ^ 67u;
    return (int)(b & 0x7fffffffu);
}

VMP int vm_const_pool(int x) {
    int salt = x;
    return (salt ^ 0x12345678) + 0x0fedcba9;
}

typedef struct {
    long a;
    long b;
    long c;
} BigResult;

VMP BigResult vm_sret_big(long x) {
    BigResult result;
    result.a = x + 1;
    result.b = x + 2;
    result.c = result.a + result.b;
    return result;
}

typedef struct {
    long a;
    long b;
} SmallPair;

typedef int Vec4 __attribute__((vector_size(16)));

VMP SmallPair vm_pair(long x) {
    SmallPair result;
    result.a = x + 3;
    result.b = x * 2;
    return result;
}

__attribute__((noinline)) static int native_callee(int x) {
    return x + 11;
}

__attribute__((noinline)) static SmallPair native_pair(long x, long y) {
    SmallPair result;
    result.a = x + y;
    result.b = x - y;
    return result;
}

__attribute__((noinline)) static BigResult native_big(long x) {
    BigResult result;
    result.a = x + 7;
    result.b = x * 3;
    result.c = result.a ^ result.b;
    return result;
}

__attribute__((noinline)) static double native_float_callee(float x, double y) {
    return (double)x * 1.25 + y / 2.0;
}

VMP int vm_safe_skip_call(int x) {
    return native_callee(x) * 2;
}

VMP long vm_native_pair(long x) {
    SmallPair result = native_pair(x, x + 5);
    return result.a * 3 + result.b;
}

VMP long vm_native_sret(long x) {
    BigResult result = native_big(x);
    return result.a + result.b + result.c;
}

VMP double vm_native_float(double x) {
    float narrowed = (float)(x + 1.5);
    double mixed = native_float_callee(narrowed, x - 0.25);
    return mixed + (double)narrowed;
}

VMP float vm_float32_mix(float x) {
    float nx = -x;
    float a = nx - 0.25f;
    float b = a * -1.5f;
    float c = b / 2.0f;
    if (c > x) {
        return -(c - x);
    }
    return -(x - c);
}

VMP double vm_float64_mix(double x) {
    double nx = -x;
    double a = nx - 0.25;
    double b = a * -1.75;
    double c = b / 3.0;
    if (c <= x) {
        return -(x - c);
    }
    return -(c - x);
}

VMP double vm_float_cast_mix(int x, unsigned y, float z, double w) {
    float sx = (float)x;
    double uy = (double)y;
    int zi = (int)z;
    unsigned wu = (unsigned)w;
    float wt = (float)w;
    double ze = (double)z;
    return (double)sx + uy + (double)zi + (double)wu + (double)wt + ze;
}

VMP int vm_vector_skip(Vec4 value) {
    Vec4 mixed = value + (Vec4){ 1, 2, 3, 4 };
    return mixed[0] ^ mixed[3];
}

int main(int argc, char **argv) {
    (void)argv;
    int seed = argc + 4;
    int a = vm_mix(seed, 7);
    int b = vm_branch(a, seed * 3);
    int c = vm_loop(seed + 5);
    int d = vm_switch(seed + 2);
    int e = vm_memory(seed + 6);
    int slots[4] = { seed, seed + 1, seed + 2, seed + 3 };
    int matrix[4][5] = {
        { seed, seed + 1, seed + 2, seed + 3, seed + 4 },
        { seed + 5, seed + 6, seed + 7, seed + 8, seed + 9 },
        { seed + 10, seed + 11, seed + 12, seed + 13, seed + 14 },
        { seed + 15, seed + 16, seed + 17, seed + 18, seed + 19 },
    };
    vm_void_pointer(slots, 4);
    long g = vm_ptr_roundtrip(slots, 2);
    int gg = vm_dynamic_gep2(matrix, 2, 1);
    int gr = vm_reg_reuse_chain(gg, seed);
    int gc = vm_const_pool(gr);
    BigResult h = vm_sret_big(g);
    SmallPair p = vm_pair(h.a);
    int f = vm_safe_skip_call(seed);
    long np = vm_native_pair(h.b);
    long ns = vm_native_sret(h.c);
    double nf = vm_native_float((double)seed + 0.5);
    float u = vm_float32_mix((float)seed);
    double v = vm_float64_mix((double)seed);
    double t = vm_float_cast_mix(seed, (unsigned)(seed + 3), (float)seed + 0.75f, (double)seed + 1.25);
    Vec4 vec = { seed, seed + 1, seed + 2, seed + 3 };
    int vk = vm_vector_skip(vec);
    int mr = vm_multiblock_reuse(seed + gr);
    printf(
        "%d %d %d %d %d %d %ld %d %d %d %d %ld %ld %ld %ld %ld %ld %ld %.2f %d %.2f %.2f %.2f %d\n",
        a,
        b,
        c,
        d,
        e,
        f,
        g,
        gg,
        gr,
        gc,
        mr,
        h.a,
        h.b,
        h.c,
        p.a,
        p.b,
        np,
        ns,
        nf,
        slots[3],
        u,
        v,
        t,
        vk
    );
    return 0;
}
