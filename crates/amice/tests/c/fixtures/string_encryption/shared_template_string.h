#pragma once

#include <cstdint>

struct TemplateMessage {
    char text[64];
    std::uint32_t line;
    char source[32];

    consteval TemplateMessage()
        : text{"AMICE_SHARED_TEMPLATE_MESSAGE {}"},
          line{44},
          source{"AMICE_NTTP_SOURCE_FILE"} {}
};

struct TemplateControls {
    unsigned char bytes[16] = {
        0xff, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80,
        0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80,
    };
};
