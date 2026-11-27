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
