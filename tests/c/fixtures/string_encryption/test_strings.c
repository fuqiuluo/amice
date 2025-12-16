// test_strings.c
// 目标：让字符串打印只在非入口块中发生，覆盖多种控制流情形
// 编译：clang -O0 -g test_strings.c -o test_strings
// 运行：./test_strings [seed]

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#if defined(_MSC_VER)
#  define NOINLINE __declspec(noinline)
#else
#  define NOINLINE __attribute__((noinline))
#endif

// 使用 volatile 防止编译器过度优化，保证分支存在
static volatile int g_flag = 0;
static volatile int g_sink = 0;

// 一些重复/特殊/多字节字符串用来测试去重与编码处理
static const char* S_HELLO       = "hello";
static const char* S_HELLO_DUP   = "hello";              // 与 S_HELLO 重复
static const char* S_FORMAT_1    = "value = %d\n";
static const char* S_FORMAT_2    = "pair = (%d, %d)\n";
static const char* S_ESCAPED     = "line1\\nline2\\tTabbed\\x21!\n";
static const char* S_UTF8_CN     = "中文测试";
static const char* S_UTF8_MIXED  = "混合: café – τ – 😊";
static const char* S_BRANCH_A    = "[IF] Took branch A\n";
static const char* S_BRANCH_B    = "[IF] Took branch B\n";
static const char* S_SWITCH_DFT  = "[SWITCH] default\n";
static const char* S_LOOP_ENTER  = "[LOOP] enter loop\n";
static const char* S_LOOP_BREAK  = "[LOOP] break at i=%d\n";
static const char* S_LOOP_CONT   = "[LOOP] continue at i=%d\n";
static const char* S_LOOP_EXIT   = "[LOOP] exit loop\n";
static const char* S_SHORT_AND   = "[SC] a && b true\n";
static const char* S_SHORT_OR    = "[SC] a || b true\n";
static const char* S_TERN_TRUE   = "[TERNARY] true path\n";
static const char* S_TERN_FALSE  = "[TERNARY] false path\n";
static const char* S_GOTO_HIT    = "[GOTO] jumped label\n";
static const char* S_RECUR_BASE  = "[RECUR] base case\n";
static const char* S_RECUR_STEP  = "[RECUR] step depth=%d\n";
static const char* S_DISPATCH_A  = "[DISPATCH] handler A\n";
static const char* S_DISPATCH_B  = "[DISPATCH] handler B\n";
static const char* S_MAIN_DONE   = "[MAIN] done seed=%d\n";

// 确保每个函数的打印都不在入口块：先做分支或跳转再打印

NOINLINE void demo_if_else(int x) {
    // 入口块里不打印：先做条件分支
    if ((x & 1) == 0) {
        // 非入口块
        printf("%s", S_BRANCH_A);
        printf("%s %s\n", S_HELLO, S_HELLO_DUP); // 重复字符串测试
    } else {
        // 非入口块
        printf("%s", S_BRANCH_B);
        printf("%s\n", S_UTF8_CN);
    }
    // 再在另一个分支中使用格式化字符串
    if (x > 10) {
        printf(S_FORMAT_1, x);
    } else {
        printf("%s\n", S_ESCAPED);
    }
}

NOINLINE void demo_switch(int x) {
    // 入口块做一次变换，仍不打印
    int v = x % 5;
    switch (v) {
        case 0:
            printf("switch: case 0\n");
            break;
        case 1:
            printf("switch: case 1\n");
            // 故意落入下一个 case 以产生更多基本块
            // 注意：标准 C 需要明确的 fallthrough，使用注释说明
            /* fallthrough */
        case 2:
            printf("switch: case 2 or fallthrough from 1\n");
            break;
        case 3:
            printf("switch: case 3\n");
            printf("%s\n", S_UTF8_MIXED);
            break;
        default:
            printf("%s", S_SWITCH_DFT);
            break;
    }
}

NOINLINE void demo_loops(int n) {
    // 非入口块打印：先判断
    if (n <= 0) {
        // 不打印，直接返回
        return;
    } else {
        printf("%s", S_LOOP_ENTER);
    }

    for (int i = 0; i < n; i++) {
        // 制造 continue 分支
        if (i % 2 == 0) {
            printf(S_LOOP_CONT, i);
            continue;
        }
        // 制造 break 分支
        if (i == 5) {
            printf(S_LOOP_BREAK, i);
            break;
        }
        // 普通路径
        printf(S_FORMAT_1, i);
    }

    // 循环结束后的块
    printf("%s", S_LOOP_EXIT);
}

NOINLINE void demo_short_circuit(int a, int b) {
    // 先计算，后打印
    int cond_and = (a != 0) && (b != 0);
    if (cond_and) {
        printf("%s", S_SHORT_AND);
    }

    int cond_or = (a != 0) || (b != 0);
    if (cond_or) {
        printf("%s", S_SHORT_OR);
    }
}

NOINLINE void demo_ternary(int x) {
    // 入口只做条件与赋值，不打印
    const char* msg = (x > 0) ? S_TERN_TRUE : S_TERN_FALSE;
    // 把打印放到后续块
    if (msg == S_TERN_TRUE) {
        printf("%s", S_TERN_TRUE);
    } else {
        printf("%s", S_TERN_FALSE);
    }
}

NOINLINE void demo_goto(int x) {
    // 入口判定，不打印
    if (x == 42) {
        goto hit;
    } else {
        // 再次分支以形成更多块
        if (x < 0) {
            printf("goto: negative path\n");
        } else {
            printf("goto: non-negative path\n");
        }
        return;
    }
hit:
    // 只有跳转后才打印
    printf("%s", S_GOTO_HIT);
}

NOINLINE void demo_recursion(int depth) {
    // 入口块：先判断，不打印
    if (depth <= 0) {
        printf("%s", S_RECUR_BASE);
        return;
    } else {
        printf(S_RECUR_STEP, depth);
        // 使用 volatile 防止尾递归优化
        g_sink = depth;
        demo_recursion(depth - 1);
    }
}

typedef void (*handler_t)(void);

NOINLINE void handler_a(void) {
    // 入口先通过全局标志决定打印
    if (g_flag == 0) {
        printf("%s", S_DISPATCH_A);
    } else {
        printf("handler A alt path\n");
    }
}

NOINLINE void handler_b(void) {
    if (g_flag != 0) {
        printf("%s", S_DISPATCH_B);
    } else {
        printf("handler B alt path\n");
    }
}

NOINLINE void demo_dispatch(int key) {
    // 入口块：先选择函数指针，不打印
    handler_t h = (key % 2 == 0) ? handler_a : handler_b;
    // 非入口：间接调用，内部才打印
    h();
}

int main(int argc, char** argv) {
    // main 的入口块不打印：只做参数解析与分支
    int seed = 0;
    demo_if_else(seed);
    demo_switch(seed);
    demo_loops((seed % 10) + 3);
    demo_short_circuit(seed & 2, seed & 4);

    demo_ternary(seed - 5);
    demo_goto(seed % 50);
    g_flag = (seed >> 3) & 1;
    demo_dispatch(seed);

    // 最后的打印也放在分支中，避免位于入口基本块
    if (seed != 0xdeadbeef) {
        printf(S_FORMAT_2, seed, seed ^ 0x5a5a5a5a);
        printf(S_MAIN_DONE, seed);
    } else {
        printf("Unlikely seed matched sentinel\n");
    }

    // 使用未优化的全局读写，避免过度 DCE
    g_sink ^= seed;
    return (g_sink & 1);
}

//[IF] Took branch A
//hello hello
//line1\nline2\tTabbed\x21!
//
//switch: case 0
//[LOOP] enter loop
//[LOOP] continue at i=0
//value = 1
//[LOOP] continue at i=2
//[LOOP] exit loop
//[TERNARY] false path
//goto: non-negative path
//[DISPATCH] handler A
//pair = (0, 1515870810)
//[MAIN] done seed=0
