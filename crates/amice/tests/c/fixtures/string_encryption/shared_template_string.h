#pragma once

#include <cstdint>

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
