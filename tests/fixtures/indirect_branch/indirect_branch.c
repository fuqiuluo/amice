#include <stdio.h>

// 防优化：防止编译器常量折叠或删除代码
volatile int sink;

void test_unconditional_br() {
    int a = 1;
    goto label1;
    a = 2;  // dead code, but br will skip
label1:
    a = 3;
    sink = a;
}

void test_conditional_br(int x) {
    int result;
    if (x > 0) {
        result = 10;
    } else if (x < 0) {
        result = -10;
    } else {
        result = 0;
    }
    sink = result;
}

void test_switch_br(int choice) {
    int value = 0;
    switch (choice) {
        case 1:
            value = 100;
            break;
        case 2:
            value = 200;
            break;
        case 3:
            value = 300;
            break;
        default:
            value = -1;
            break;
    }
    sink = value;
}

void test_loop_while(int n) {
    int sum = 0;
    while (n > 0) {
        sum += n;
        n--;
    }
    sink = sum;
}

void test_loop_for(int start, int end) {
    int count = 0;
    for (int i = start; i < end; i++) {
        if (i % 2 == 0) {
            count++;
        }
    }
    sink = count;
}

void test_nested_if_else(int a, int b, int c) {
    int result;
    if (a > 0) {
        if (b > 0) {
            result = 1;
        } else {
            if (c > 0) {
                result = 2;
            } else {
                result = 3;
            }
        }
    } else {
        result = 4;
    }
    sink = result;
}

void test_goto_based_control_flow(int flag) {
    int x = 0;

    if (flag == 1) goto branch1;
    if (flag == 2) goto branch2;
    goto default_branch;

branch1:
    x = 11;
    goto end;

branch2:
    x = 22;
    goto end;

default_branch:
    x = 99;

end:
    sink = x;
}

void test_function_call_and_return(int sel) {
    if (sel) {
        test_conditional_br(5);
    } else {
        test_switch_br(2);
    }
    sink = sel + 1;
}

int main() {
    printf("Running control flow test suite...\n");

    test_unconditional_br();

    test_conditional_br(1);
    test_conditional_br(-1);
    test_conditional_br(0);

    test_switch_br(1);
    test_switch_br(2);
    test_switch_br(3);
    test_switch_br(99);

    test_loop_while(5);
    test_loop_for(1, 10);

    test_nested_if_else(1, 1, 1);
    test_nested_if_else(1, 0, 1);
    test_nested_if_else(0, 0, 0);

    test_goto_based_control_flow(1);
    test_goto_based_control_flow(2);
    test_goto_based_control_flow(0);

    test_function_call_and_return(1);
    test_function_call_and_return(0);

    printf("All tests completed. sink = %d\n", sink);
    return 0;
}