// 宽字符字符串加密测试（issue #87）：
// LLVMIsConstantString 只认 [N x i8] 数组，UTF-16 / UTF-32 / wchar_t 字面量
// 之前会被 StringEncryption 直接跳过，保持明文。
#include <stdio.h>
#include <uchar.h>

int main(void)
{
    const char16_t *utf16 = u"Amice16\u4F60";
    const char32_t *utf32 = U"Amice32\u4F60";
    const wchar_t *wide = L"AmiceWide";

    for (int i = 0; utf16[i]; ++i)
        printf("%04x ", (unsigned)utf16[i]);
    printf("\n");
    for (int i = 0; utf32[i]; ++i)
        printf("%04x ", (unsigned)utf32[i]);
    printf("\n");
    for (int i = 0; wide[i]; ++i)
        printf("%04x ", (unsigned)wide[i]);
    printf("\n");
    return 0;
}
