# Quick Start

English | [简体中文](QuickStart_zh_CN.md)

This guide gets Amice running in about five minutes: download the plugin → verify string encryption → integrate it into your own project. For plugin and toolchain preparation, including Windows notes, see [Download and Setup](Download_en_US.md).

## 1. Download the Right File

Open a release's **Assets** and choose a file for the computer and compiler you use to build your project. These examples use LLVM 21 and Android NDK r30:

| Use case | Download |
| --- | --- |
| Linux x86_64, Clang 21 | `libamice-llvm21-linux-x86_64.so` |
| macOS Apple Silicon, Homebrew LLVM 21 | `libamice-llvm21-macos-arm64.dylib` |
| macOS Intel, Homebrew LLVM 21 | `libamice-llvm21-macos-x86_64.dylib` |
| Building Android on Linux, NDK r30 | `amice-android-ndk-r30-linux-x86_64.tar.gz` |
| Building Android on macOS, NDK r30 | `amice-android-ndk-r30-darwin-x86_64.tar.gz` |

For the Linux/macOS plugins, the `llvm21` in the file name must match your local LLVM/Clang major: for example, choose `llvm20` for Clang 20. Android users should download a complete bundle containing the NDK, plugin and runtime libraries, then follow [Android Usage](AndroidNDKSupport_en_US.md).

`.sha256` files are checksums; `Source code` contains Amice's source.

## 2. Try String Encryption

This **Linux/macOS C/C++ example** uses your downloaded plugin. For a Rust project, go to [Rust Usage](RustUsage_en_US.md).

Choose the settings for your platform and adjust the plugin path to its download location.

**Linux, Clang 21:**

```bash
AMICE_CLANG=clang-21
AMICE_CXX=clang++-21
AMICE_PLUGIN="$HOME/Downloads/libamice-llvm21-linux-x86_64.so"
```

**macOS, Homebrew LLVM 21:**

```bash
AMICE_CLANG="$(brew --prefix llvm@21)/bin/clang"
AMICE_CXX="$(brew --prefix llvm@21)/bin/clang++"
AMICE_PLUGIN="$HOME/Downloads/libamice-llvm21-macos-arm64.dylib"
# On Intel Macs use libamice-llvm21-macos-x86_64.dylib instead
```

Check the compiler version and file path:

```bash
"$AMICE_CLANG" --version
test -f "$AMICE_PLUGIN" && echo "Plugin found"
```

You should see Clang 21 and `Plugin found`. In an empty directory, create `hello.c`:

```bash
cat > hello.c <<'SRC'
extern int puts(const char *);
int main(void) { return puts("AMICE_STRING_TEST") < 0; }
SRC

AMICE_STRING_ENCRYPTION=true \
  "$AMICE_CLANG" -fpass-plugin="$AMICE_PLUGIN" hello.c -o hello &&
  ./hello
```

The program should print `AMICE_STRING_TEST`. Now check that the string is hidden in the executable:

```bash
strings -a hello > hello.strings && {
  if grep -Fq AMICE_STRING_TEST hello.strings; then
    echo "FAIL: plaintext marker is still present"
  else
    echo "PASS: plaintext marker is hidden"
  fi
}
```

Correct program output and `PASS` verify this example. For compilation errors, `strings` errors or remaining plaintext, see [Troubleshooting](Troubleshooting_en_US.md).

## 3. Add It to Your Project

Add the plugin path to your existing compile command and enable the features you need. For example, enable string encryption in C++:

```bash
AMICE_STRING_ENCRYPTION=true \
  "$AMICE_CXX" -fpass-plugin="$AMICE_PLUGIN" main.cpp -o app
```

In CMake, add the option to your chosen target:

```cmake
set(AMICE_PLUGIN "/absolute/path/to/downloaded/libamice-llvm21-linux-x86_64.so")
target_compile_options(your_target PRIVATE "-fpass-plugin=${AMICE_PLUGIN}")
```

Run `export AMICE_STRING_ENCRYPTION=true` in the terminal launching the build. Replace `your_target` with your target's name, use the absolute path of the downloaded plugin, and select C/C++ compilers from the same LLVM installation in CMake. Recompile affected source files after changing the plugin or configuration.

To protect a particular C/C++ function, use an annotation. This enables VMP for `sensitive`:

```c
__attribute__((noinline, annotate("+vm_virtualize")))
int sensitive(int x) {
    return (x * 7) ^ 0x55;
}
```

Keep `-fpass-plugin` in your compile command. VMP uses its built-in profile by default; see [Function Annotations](FunctionAnnotations_en_US.md) for options. Start with a few functions and run your project's existing tests; unsupported functions may be skipped.

## Save Common Settings

Enable individual features with environment variables, or save multiple settings in `amice.toml`:

```toml
[string_encryption]
enable = true

[flatten]
enable = true
mode = "basic"
```

Select the file when compiling:

```bash
AMICE_CONFIG_PATH="$(pwd)/amice.toml" \
  "$AMICE_CLANG" -fpass-plugin="$AMICE_PLUGIN" hello.c -o hello
```

For features supporting annotations, precedence is **function annotation > environment > config file > default**. String encryption uses global settings. To change how multiple features are ordered, see [Pass Execution Order](PassOrder_en_US.md).

## Next Steps

- All switches and parameters: [Runtime Environment Variables](EnvConfig_en_US.md)
- Per-function enable/disable: [Function Annotations](FunctionAnnotations_en_US.md)
- Something went wrong: [Troubleshooting](Troubleshooting_en_US.md)
