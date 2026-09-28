// No platform headers: compile this fixture with Linux, Windows and Android ABIs.
extern "C" void observe(unsigned);
struct Guard {
    unsigned value;
    ~Guard() { observe(value); }
};
struct Base {
    virtual ~Base();
    virtual unsigned method(unsigned) = 0;
};
Base::~Base() { observe(17); }
struct Object : Base {
    unsigned method(unsigned x) override { return (x + 19) ^ 31; }
};
struct Copy {
    unsigned words[8];
    Copy(const Copy &other) {
        for (int i = 0; i < 8; ++i) words[i] = other.words[i];
        observe(words[0]);
    }
    ~Copy() { observe(words[7]); }
};

#if defined(_WIN32)
#define BCF_EXPORT __declspec(dllexport)
#else
#define BCF_EXPORT __attribute__((visibility("default")))
#endif

extern "C" BCF_EXPORT unsigned bcf_abi_exception(unsigned x) {
    try {
        Guard guard{x};
        Object object;
        if (x & 1) throw x;
        return object.method(x);
    } catch (unsigned value) {
        return (value + 19) ^ 31;
    }
}

BCF_EXPORT unsigned bcf_abi_copy(Copy value, unsigned x) {
    return (value.words[0] + x) ^ 31;
}

extern "C" BCF_EXPORT unsigned bcf_abi_plain(unsigned x) {
    return ((x + 19) ^ 31) - 7;
}
