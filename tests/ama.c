#include <stdio.h>
#include <stdint.h>

struct Inner {
    int16_t x;     // 0
    uint8_t y;     // 2
    uint8_t pad;   // 3 (padding, but may be optimized away in IR)
    int32_t z;     // 4 (due to alignment)
};

struct Outer {
    int a;                 // 0
    struct Inner in;       // 4 ... 11
    int arr[5];            // 12 ... 31
    unsigned char bytes[16]; // 32 ... 47
};

struct S2 {
    int m[2][3];           // row-major: m[1][2] at offset 4 * (1*3 + 2) = 20
};

typedef struct { int a; int b; } POD;

// 1) 典型：多个不同字段/数组常量访问
int sum_outer(struct Outer *o) {
    // 这些都是常量偏移：o->a、o->in.z、o->arr[3]、o->bytes[5]
    return o->a + o->in.z + o->arr[3] + o->bytes[5];
}

// 2) 嵌套二维数组常量访问
int get_s2(struct S2 *s) {
    return s->m[1][2]; // 常量偏移
}

// 3) 指针算术：常量步进（等价于常量偏移）
int ptr_arith(struct Outer *o) {
    int *p = &o->arr[0];
    return *(p + 4); // o->arr[4]
}

// 4) POD 结构体字段
int pod_use(POD *p) {
    return p->b; // 常量偏移
}

// 5) 不应改写：非常量下标
int nonconst_index(struct Outer *o, int i) {
    return o->arr[i]; // 索引非常量，应保持原样（我们的 pass 不改）
}

int main() {
    struct Outer o = {0};
    o.a = 10;
    o.in.z = 7;
    o.arr[3] = 4;
    o.bytes[5] = 1;

    struct S2 s = { .m = {{1,2,3},{4,5,6}} };
    POD pod = { .a = 11, .b = 22 };

    int r = 0;
    r += sum_outer(&o);
    r += get_s2(&s);
    r += ptr_arith(&o);
    r += pod_use(&pod);
    r += nonconst_index(&o, 2);

    printf("%d\n", r);
    return 0;
}
//
//Running test1...
//50
