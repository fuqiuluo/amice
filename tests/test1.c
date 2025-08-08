#include <stdio.h>
#include <stdlib.h>
#include <time.h>

// 简单的计算函数
int calculate(int a, int b, int op) {
    switch(op) {
        case 0: return a + b;
        case 1: return a - b;
        case 2: return a * b;
        case 3: return (b != 0) ? a / b : 0;
        default: return a;
    }
}

// 包含嵌套控制流的函数
int complex_function(int input) {
    int result = 0;
    int i, j;

    // 嵌套的if-else和循环
    if (input > 0) {
        if (input < 10) {
            // 嵌套for循环
            for (i = 0; i < input; i++) {
                for (j = 0; j < 3; j++) {
                    result += calculate(i, j, j % 4);
                }

                // 嵌套的while循环
                int temp = input;
                while (temp > 0) {
                    result += temp % 2;
                    temp /= 2;
                }
            }
        } else {
            // 另一个分支
            i = input;
            while (i > 10) {
                if (i % 2 == 0) {
                    result += i / 2;
                    i -= 3;
                } else {
                    result += i * 2;
                    i -= 5;
                }
            }
        }
    } else if (input < 0) {
        // 负数处理
        input = -input;
        for (i = input; i > 0; i--) {
            if (i % 3 == 0) {
                result -= i;
            } else if (i % 3 == 1) {
                result += i * 2;
            } else {
                result += i / 2;
            }
        }
    } else {
        // input == 0的情况
        result = 42;
    }

    return result;
}

// 包含多层嵌套的数据处理函数
void process_array(int* arr, int size) {
    int i, j, k;

    // 三层嵌套循环
    for (i = 0; i < size; i++) {
        if (arr[i] > 0) {
            for (j = 0; j < arr[i] % 5 + 1; j++) {
                for (k = 0; k < 3; k++) {
                    if (j * k > 0) {
                        arr[i] += calculate(j, k, k % 3);
                    } else {
                        arr[i] -= j + k;
                    }
                }

                // 内层的条件分支
                if (arr[i] % 2 == 0) {
                    arr[i] /= 2;
                } else {
                    arr[i] = arr[i] * 3 + 1;
                }
            }
        } else {
            // 处理负数或零
            int temp = arr[i];
            while (temp != 0) {
                if (temp > 0) {
                    temp--;
                    arr[i]++;
                } else {
                    temp++;
                    arr[i]--;
                }
            }
        }
    }
}

// 递归函数（测试函数调用的扁平化）
int fibonacci(int n) {
    if (n <= 1) {
        return n;
    } else if (n == 2) {
        return 1;
    } else {
        // 迭代版本避免太深的递归
        int a = 0, b = 1, c;
        int i;
        for (i = 2; i <= n; i++) {
            c = a + b;
            a = b;
            b = c;

            // 添加一些条件分支
            if (c % 3 == 0) {
                c += 1;
            } else if (c % 5 == 0) {
                c -= 1;
            }
            b = c;
        }
        return b;
    }
}

// 主函数包含多种控制流
int main() {
    printf("=== 扁平化混淆测试Demo ===\n");

    int test_values[] = {-5, -1, 0, 3, 7, 15, 25};
    int num_tests = sizeof(test_values) / sizeof(test_values[0]);
    int i, j;

    printf("测试复杂函数:\n");
    for (i = 0; i < num_tests; i++) {
        int input = test_values[i];
        int result = complex_function(input);
        printf("complex_function(%d) = %d\n", input, result);

        // 根据结果进行不同处理
        if (result > 100) {
            printf("  结果较大，进行额外处理\n");
            for (j = 0; j < 3; j++) {
                result = calculate(result, j + 1, j % 4);
                printf("  处理步骤%d: %d\n", j + 1, result);
            }
        } else if (result < 0) {
            printf("  负结果，转换为正数: %d\n", -result);
        } else {
            printf("  结果正常\n");
        }
    }

    // 测试数组处理
    printf("\n测试数组处理:\n");
    int test_array[] = {5, -3, 0, 12, 8, -7, 15, 2};
    int array_size = sizeof(test_array) / sizeof(test_array[0]);

    printf("原始数组: ");
    for (i = 0; i < array_size; i++) {
        printf("%d ", test_array[i]);
    }
    printf("\n");

    process_array(test_array, array_size);

    printf("处理后数组: ");
    for (i = 0; i < array_size; i++) {
        printf("%d ", test_array[i]);
    }
    printf("\n");

    // 测试斐波那契数列
    printf("\n测试斐波那契数列:\n");
    for (i = 0; i <= 10; i++) {
        int fib = fibonacci(i);
        printf("fib(%d) = %d", i, fib);

        // 添加一些额外的控制流
        if (fib % 2 == 0) {
            printf(" (偶数)");
        } else {
            printf(" (奇数)");
        }

        if (fib > 20) {
            printf(" - 较大的数");
        }
        printf("\n");
    }

    // 最终的综合测试
    printf("\n综合测试:\n");
    int final_result = 0;

    for (i = 0; i < 5; i++) {
        int temp = i;

        switch (temp % 4) {
            case 0:
                final_result += complex_function(temp);
                printf("情况0: 加法操作，temp=%d\n", temp);
                break;
            case 1:
                final_result -= fibonacci(abs(temp) % 8);
                printf("情况1: 减法操作，temp=%d\n", temp);
                break;
            case 2:
                final_result *= (temp == 0) ? 1 : temp;
                printf("情况2: 乘法操作，temp=%d\n", temp);
                break;
            default:
                if (temp != 0) {
                    final_result /= temp;
                } else {
                    final_result += 10;
                }
                printf("情况3: 除法/加法操作，temp=%d\n", temp);
                break;
        }

        printf("当前结果: %d\n", final_result);
    }

    printf("\n最终结果: %d\n", final_result);
    printf("测试完成！\n");

    return 0;
}