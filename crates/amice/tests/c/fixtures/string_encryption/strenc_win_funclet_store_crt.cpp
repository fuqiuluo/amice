// Tier-B companion to strenc_win_funclet_store.cpp: identical WinEH funclet
// store pattern, but includes <cstdio> so printf links through the inline
// header wrapper (__stdio_common_vfprintf) — the raw `printf` symbol does not
// exist in every MSVC CRT (missing from the aarch64 libcmt).
#include <cstdio>

const char *stored_message = "";

int main() {
    try {
        throw 1;
    } catch (...) {
        stored_message = "SECRET_IN_HANDLER_99";
    }
    std::printf("%s\n", stored_message);
    return 0;
}
