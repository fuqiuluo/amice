// 示例代码来自 fuqiuluo ！
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int other_func()
{
    char *literal1 = "This is a literal.";
    char *literal2 = "This is a literal.";
    printf("%s %p\n", literal1, literal1);
    printf("%s %p\n", literal2, literal2);
    printf("%s %p\n", literal1, literal1);
    printf("%s %p\n", literal2, literal2);
    printf("%s %p\n", literal2, literal2);
    printf("%s %p\n", literal1, literal1);
    printf("%s %p\n", literal2, literal2);
    printf("%s %p\n", literal2, literal2);
    printf("%s %p\n", literal1, literal1);
    printf("%s %p\n", literal2, literal2);
    return 0;
}

int main()
{
    char *literal1 = "This is a literal.";
    char *literal2 = "This is a literal.";
    printf("%s %p\n", literal1, literal1);
    printf("%s %p\n", literal2, literal2);
    printf("%s %p\n", literal1, literal1);
    printf("%s %p\n", literal2, literal2);
    printf("%s %p\n", literal2, literal2);
    printf("%s %p\n", literal1, literal1);
    printf("%s %p\n", literal2, literal2);
    printf("%s %p\n", literal2, literal2);
    printf("%s %p\n", literal1, literal1);
    printf("%s %p\n", literal2, literal2);
    return 0;
}