#include <stdio.h>

int main(void)
{
    int ac = 0;
    for (int i = 0; i < 10; i++)
        __atomic_fetch_add(&ac, i, 5);
    printf("a1:%d\n", ac);

    int n = 8;
    int vla[n];
    for (int i = 0; i < n; i++)
        vla[i] = i * i;
    printf("l1:%d\n", vla[7]);
    fflush(stdout);
    return 0;
}
