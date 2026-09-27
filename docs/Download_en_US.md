# Download and Setup

English | [简体中文](Download_zh_CN.md)

Download a plugin or Android NDK bundle from [Amice Releases](https://github.com/fuqiuluo/amice/releases). Using a release does not require cloning Amice or installing Rust to build Amice itself.

## Linux / macOS: Match Your Compiler

Check the Clang used by your project and your current terminal's architecture:

```bash
clang --version
uname -m
```

If your project uses `clang-21` or a compiler at a specific path, query that command instead. Match the LLVM major, operating system and CPU architecture in the filename. For example, Linux x86_64 + Clang 21 uses `libamice-llvm21-linux-x86_64.so`.

The macOS examples use Homebrew LLVM. Apple's Clang version number is not a direct match for the LLVM number in download names; inspect and use the Homebrew clang.

### You Already Have Compatible LLVM

Download the matching `.so` / `.dylib`, note its absolute path, and follow the first-use example in [Quick Start](QuickStart_en_US.md). The Linux/macOS plugins still require local LLVM runtime libraries. See [Troubleshooting](Troubleshooting_en_US.md) for load errors.

### Install LLVM If Needed: LLVM 21 Example

Debian / Ubuntu: if LLVM 21 is not available in your repositories, follow the [LLVM APT instructions](https://apt.llvm.org/) for your distribution, then install the compiler and tools:

```bash
sudo apt update
sudo apt install clang-21 llvm-21 binutils
clang-21 --version
```

macOS: install Xcode Command Line Tools if needed (`xcode-select --install`), then use Homebrew:

```bash
brew install llvm@21
"$(brew --prefix llvm@21)/bin/clang" --version
```

Fedora/RHEL: use the distribution's LLVM and choose a download based on the installed version:

```bash
sudo dnf install clang llvm binutils
clang --version
```

Confirm that your chosen release provides a plugin for that LLVM version, then follow the first-use example in [Quick Start](QuickStart_en_US.md).

## Android: Start with a Complete Bundle

Choose `amice-android-ndk-<NDK-version>-<host-system>.tar.gz`. It includes the NDK, plugin, required libraries and compiler wrappers. For example, NDK r30 on Linux uses `amice-android-ndk-r30-linux-x86_64.tar.gz`.

`linux` / `darwin` identifies the computer building your project; the phone's ABI, such as arm64, is selected when compiling. Standalone `libamice-android-ndk-*.so` / `.dylib` files only contain the plugin and are for users who already have the matching runtime libraries.

After downloading, follow [Android NDK Usage](AndroidNDKSupport_en_US.md) to extract and integrate with CMake/Gradle.

## Rust: Check rustc's LLVM First

Run `rustc -vV` and inspect `LLVM version`, then choose the corresponding plugin. Direct loading also requires a compatible nightly toolchain; see [Rust Usage](RustUsage_en_US.md).

## Windows Downloads

[v0.1.5-beta.4](https://github.com/fuqiuluo/amice/releases/tag/v0.1.5-beta.4) does not provide a Windows DLL or Windows NDK bundle, so that version has no ready-to-use Windows download. Check Assets for other releases. Linux `.so` and macOS `.dylib` files cannot be loaded by Windows compilers.

To prepare your own Windows plugin, see [Windows Source Builds](LLVMSetup_en_US.md#windows). This is a separate advanced workflow.
