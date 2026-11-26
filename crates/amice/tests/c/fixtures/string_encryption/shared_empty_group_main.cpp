extern "C" const unsigned char* read_a();
extern "C" const unsigned char* read_b();

static bool valid(const volatile unsigned char* controls) {
    if (controls[0] != 0xff) return false;
    for (int i = 1; i < 16; ++i) {
        if (controls[i] != 0x80) return false;
    }
    return true;
}

int main() {
    const unsigned char* first = read_a();
    if (!valid(first)) return 1;

    const unsigned char* second = read_b();
    // A private copy per module would break the shared address
    if (first != second) return 2;
    if (!valid(first) || !valid(second)) return 3;

    return valid(read_a()) && valid(read_b()) ? 0 : 4;
}
