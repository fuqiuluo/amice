#include "shared_template_string.h"

template <TemplateMessage Message>
const TemplateMessage* message() {
    return &Message;
}

template <TemplateControls Controls>
const unsigned char* controls() {
    return Controls.bytes;
}

extern "C" const TemplateMessage* READER_NAME() {
    return message<TemplateMessage{}>();
}

extern "C" const unsigned char* CONTROLS_NAME() {
    return controls<TemplateControls{}>();
}

template <typename T, T... Cs>
consteval auto operator""_sym() {
    return FixedString<sizeof...(Cs), Cs..., T{}>{};
}

template <FixedString Symbol>
const char* symbol(std::size_t* size, std::uint32_t* gnu_hash) {
    *size = Symbol.data.size();
    *gnu_hash = Symbol.gnu_hash();
    return Symbol.data.data();
}

extern "C" const char* SYMBOL_NAME(std::size_t* size, std::uint32_t* gnu_hash) {
    return symbol<"_ZN3art7Runtime9instance_E"_sym>(size, gnu_hash);
}

template <unsigned char First>
const unsigned char* local_controls() {
    static constexpr std::array<unsigned char, 16> bytes = {
        First, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80,
        0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80,
    };
    return bytes.data();
}

extern "C" const unsigned char* LOCAL_CONTROLS_NAME() {
    return local_controls<0xff>();
}
