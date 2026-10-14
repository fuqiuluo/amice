// No platform headers: this fixture is compiled to Windows-MSVC IR on any host
// (issue #99 WinEH funclet scenarios). extern "C" printf avoids <cstdio>, and
// catch(...) avoids RTTI typeinfo dependencies.
extern "C" int printf(const char *format, ...);

const char *stored_message = "";

int main() {
    try {
        throw 1;
    } catch (...) {
        stored_message = "SECRET_IN_HANDLER_99";
    }
    printf("%s\n", stored_message);
    return 0;
}
