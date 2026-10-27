// Edge: wchar_t literal with the correct header (<wchar.h>), unlike the PR's
// own wide_strings.c which omits it and fails to compile in C mode.
#include <stdio.h>
#include <wchar.h>

int main(void) {
    const wchar_t *w = L"WcharTest";
    for (int i = 0; w[i]; ++i) printf("%04x ", (unsigned)w[i]);
    printf("\n");
    return 0;
}
