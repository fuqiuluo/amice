#include "stdio.h"

//static void* ppp;
//static char* aa;
//static char* bb = "bbb";
//
//int global_var = 42;           // 初始化的全局变量
//int uninitialized_global;      // 未初始化的全局变量
//const int global_const = 100;  // 全局常量
//static int static_global = 10; // 静态全局变量
//
//int add(int a, int b) {
//    return a + b - global_var + uninitialized_global + global_const + static_global;
//}
//
//void print_hello(char* name) {
//    printf("Hello %s\n", name);
//    char* a = "aaa";
//    char* b = "bbb";
//    int global_array[10] = {1, 2, 3};
//    char buffer[256];
//    buffer[0] = name[0];
//    global_array[0] = 10 + name[1];
//
//    printf(ppp ? a : b);
//    printf(aa ? aa : bb);
//    printf("Buffer first element: %d\n", buffer[0]);
//    printf("Global variable: %p\n", global_array);
//}

void change(char** b) {
    char *bb = *b;
    bb[0] = 'c'; // 修改传入的字符串
}

void pp(char* n)  {
    printf("1pu: %s\n", n);
}

int main() {
    char* name = "World";
    char* name2 = "World";
    change(&name);
    pp(name);
    pp(name2);
    return 0;
}