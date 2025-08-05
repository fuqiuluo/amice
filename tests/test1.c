// obfuscated_call_demo.c
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

int add(int a, int b) {
    printf("Called: add(%d, %d)\n", a, b);
    return a + b;
}

int mul(int a, int b) {
    printf("Called: mul(%d, %b)\n", a, b);
    return a * b;
}

int sub(int a, int b) {
    printf("Called: sub(%d, %d)\n", a, b);
    return a - b;
}

typedef int (*func_ptr)(int, int);

func_ptr func_table[] = { add, mul, sub };
#define TABLE_SIZE (sizeof(func_table) / sizeof(func_ptr))

int obfuscated_call(int func_id, int a, int b) {
    int decoded_id = func_id ^ 0x55;
    if (decoded_id >= 0 && decoded_id < TABLE_SIZE) {
        volatile int dummy = rand() % 100;
        (void)dummy;
        return func_table[decoded_id](a, b);
    }
    fprintf(stderr, "Invalid function ID!\n");
    return -1;
}

// 主函数测试
int main() {
    srand(time(NULL));

    printf("=== Direct Calls (Clear) ===\n");
    printf("Result: %d\n", add(10, 5));
    printf("Result: %d\n", mul(10, 5));
    printf("Result: %d\n", sub(10, 5));

    printf("=== Obfuscated Indirect Calls ===\n");

    // 注意：func_id 被混淆编码过（原始 ID ^ 0x55）
    printf("Result: %d\n", obfuscated_call(0 ^ 0x55, 20, 8));  // calls add
    printf("Result: %d\n", obfuscated_call(1 ^ 0x55, 20, 8));  // calls mul
    printf("Result: %d\n", obfuscated_call(2 ^ 0x55, 20, 8));  // calls sub

    return 0;
}