#pragma once

#include <array>
#include <cstddef>
#include <cstdint>
#include <string_view>

struct TemplateMessage {
    char text[64];
    std::uint32_t line;
    char source[32];
    char exact[21];

    consteval TemplateMessage()
        : text{"AMICE_SHARED_TEMPLATE_MESSAGE {}"},
          line{44},
          source{"AMICE_NTTP_SOURCE_FILE"},
          exact{"AMICE_EXACT_TEMPLATE"} {}
};

struct TemplateControls {
    unsigned char bytes[16] = {
        0xff, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80,
        0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80,
    };
};

template <std::size_t N, char... Cs>
struct FixedString {
    static constexpr auto data = [] consteval {
        static constexpr auto kString = std::array{Cs...};
        return std::string_view{kString.data(), N};
    }();

    static consteval std::uint32_t gnu_hash() {
        std::uint32_t value = 5381;
        for (auto ch : data) {
            value = value * 33 + static_cast<unsigned char>(ch);
        }
        return value;
    }
};
