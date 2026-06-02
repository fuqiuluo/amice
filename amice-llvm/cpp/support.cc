// Cross-cutting support: LLVM version reporting and C-string release.
#include "amice_ffi.h"

extern "C" {

int amice_version_major() {
    return LLVM_VERSION_MAJOR;
}

int amice_version_minor() {
    return LLVM_VERSION_MINOR;
}

// Release a malloc'd C string handed back by other amice_* helpers.
int amice_free_string(char *err) {
    if (err) {
        free(err);
        return 0;
    }
    return -1;
}

}
