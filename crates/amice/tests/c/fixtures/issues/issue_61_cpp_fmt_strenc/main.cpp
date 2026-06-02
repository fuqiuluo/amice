#include <stdio.h>
#include "fmt/format.h"

int main() {
    char buf[1024];
    fmt::format_to_n(buf, 1023, "{}", "hello");
    puts(buf);
    return 0;
}
