#include <stdio.h>
#include <stdlib.h>
#include <time.h>

__attribute__((noinline))
int32_t test_array(int32_t n) {
    int32_t arr[4] = {0, 0, 0, 0};

    for (size_t i = 0; i < 4; i++) {
        arr[i] = (int32_t)i * n;
    }

    int32_t sum = 0;
    for (size_t i = 0; i < 4; i++) {
        sum += arr[i];
    }

    return sum;
}

__attribute__((noinline))
int32_t test_array_big_stack(int32_t n) {
    int32_t arr[0x1000] = {0};

    for (size_t i = 0; i < 0x1000; i++) {
        arr[i] = (int32_t)i * n;
    }

    int32_t sum = 0;
    for (size_t i = 0; i < 10; i++) {
        sum += arr[i];
    }

    return sum;
}

// 主函数测试
int main() {
    srand(time(NULL));

    int test[16];

    for(int i = 0; i<16;i++)
        test[i] = i;

    for(int i = 0; i<16;i++) {
        printf("test[%d] = %d\n", i, test[i]);
        printf("test_array(test[%d]) = %d\n", i, test_array(test[i]));
        printf("test_array_big_stack(test[%d]) = %d\n", i, test_array_big_stack(test[i]));
    }



    return 0;
}