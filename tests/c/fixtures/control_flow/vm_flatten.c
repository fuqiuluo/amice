#include <stdio.h>

volatile int global_sink = 0;

static int calculate(int a, int b, int op) {
    int result = 0;

    switch (op) {
        case 0:
            result = a + b;
            break;
        case 1:
            result = a - b;
            break;
        case 2:
            result = a * b;
            break;
        case 3:
            result = b != 0 ? a / b : 0;
            break;
        default:
            result = a ^ b;
            break;
    }

    global_sink ^= result;
    return result;
}

static int branch_loop(int limit) {
    int sum = 0;

    for (int i = 0; i < limit; ++i) {
        if ((i & 1) == 0) {
            sum += calculate(i, 3, 0);
        } else if (i % 3 == 0) {
            sum += calculate(i, 2, 2);
        } else {
            sum -= calculate(i, 1, 1);
        }
    }

    return sum;
}

static int state_machine(const char *input) {
    int state = 0;
    int score = 0;

    for (const char *p = input; *p != '\0'; ++p) {
        switch (state) {
            case 0:
                if (*p == 'a') {
                    state = 1;
                    score += 10;
                } else if (*p == 'b') {
                    state = 2;
                    score += 20;
                } else {
                    score += 1;
                }
                break;
            case 1:
                if (*p == 'b') {
                    state = 3;
                    score += 100;
                } else {
                    state = 0;
                    score -= 5;
                }
                break;
            case 2:
                if (*p == 'a') {
                    state = 4;
                    score += 200;
                } else {
                    state = 0;
                    score -= 10;
                }
                break;
            case 3:
                if (*p == 'c') {
                    score += 1000;
                }
                state = 0;
                break;
            case 4:
                if (*p == 'd') {
                    score += 2000;
                }
                state = 0;
                break;
            default:
                state = 0;
                break;
        }
    }

    global_sink += score;
    return score;
}

int main(void) {
    printf("扁平化混淆测试\n");

    int loop_result = branch_loop(12);
    int state_result = state_machine("abcbadxyz");
    int final_result = loop_result + state_result + global_sink;

    printf("loop=%d\n", loop_result);
    printf("state=%d\n", state_result);
    printf("final=%d\n", final_result);
    printf("测试完成\n");

    return final_result == 0 ? 1 : 0;
}
