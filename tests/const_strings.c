// 示例代码来自 NapXIN ！

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

void print_bytes(const char *label, const char *data, size_t len)
{
    printf("%s (bytes): ", label);
    for (size_t i = 0; i < len; ++i)
        printf("%02X ", (unsigned char)data[i]);
    printf("\n");
}

void change(char **b)
{
    char *bb = *b;
    bb[0] = 'c';
}

void pp(char *n)
{
    printf("1pu: %s\n", n);
}

static char *p = NULL;
int main()
{
    const char *test1 = "hello\0\0\x39\x05\0\0";
    print_bytes("test1", test1, 5 + 2 + 2);
    printf("test1 string: %s\n", test1);
    int val = *(int *)(test1 + 7);
    printf("test1 int: %d\n", val);

    char test2[] =
    {
        'h', 'e', 'l', 'l', 'o',
        '\0', '\0',
        0x39, 0x05, 0x00, 0x00
    };
    print_bytes("test2", test2, sizeof(test2));
    printf("test2 string: %s\n", test2);
    val = *(int *)(test2 + 7);
    printf("test2 int: %d\n", val);

    printf("p1: %p\n", p);
    char name[] = "World";
    p = name;
    char *name2 = "World";
    printf("p2: %p\n", p);
    change(&p);
    pp(p);
    pp(name2);

    char *str = strdup("Hello world1");
    *str = 'X';
    printf("%s\n", str);
    free(str);

    char array[] = "Hello world2";
    array[0] = 'X';
    printf("%s\n", array);

    char *mallocStr = (char *)malloc(256);
    mallocStr = "Hello world3";
    printf("%s\n", mallocStr);

    char *literal1 = "This is a literal.";
    char *literal2 = "This is a literal.";
    printf("%s %p\n", literal1, literal1);
    printf("%s %p\n", literal2, literal2);

    return 0;
}