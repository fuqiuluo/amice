template <typename T>
struct EmptyTable {
    alignas(16) static constexpr unsigned char controls[16] = {
        0xff, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80,
        0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80,
    };
};

// Compile twice so both modules refer to the same table with separate decrypt state
extern "C" const unsigned char* READER_NAME() {
    return EmptyTable<int>::controls;
}
