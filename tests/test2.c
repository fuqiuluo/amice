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
    // 最终的综合测试
    printf("\n综合测试:\n");
    int final_result = 0;

    for (int i = 0; i < 4; i++) {
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

//综合测试:
//情况0: 加法操作，temp=0
//当前结果: 42
//情况1: 减法操作，temp=1
//当前结果: 41
//情况2: 乘法操作，temp=2
//当前结果: 82
//情况3: 除法/加法操作，temp=3
//当前结果: 27
//情况0: 加法操作，temp=4
//当前结果: 51
//
//最终结果: 51
//测试完成！