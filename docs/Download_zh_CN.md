# 下载与环境准备

[English](Download_en_US.md) | 简体中文

从 [Amice Releases](https://github.com/fuqiuluo/amice/releases) 下载插件或 Android NDK bundle。使用发布版不需要克隆 Amice，也不需要安装用于构建 Amice 的 Rust 工具链。

## Linux / macOS：按编译器选择插件

先检查用于编译项目的 Clang 和当前终端的架构：

```bash
clang --version
uname -m
```

如果项目使用 `clang-21` 或指定路径的编译器，就用那个命令查看版本。文件名中的 LLVM 主版本、系统和 CPU 架构都要匹配。例如，Linux x86_64 + Clang 21 对应 `libamice-llvm21-linux-x86_64.so`。

macOS 示例使用 Homebrew LLVM。系统 Apple Clang 的版本号不能直接当作下载文件中的 LLVM 版本；请检查并使用 Homebrew 安装的 clang。

### 已经有兼容的 LLVM

下载匹配的 `.so` / `.dylib` 文件，记下它的绝对路径，然后执行 [快速上手](QuickStart_zh_CN.md) 的首次使用示例。Linux/macOS 插件仍依赖本机的 LLVM 运行库；遇到加载错误时，见 [故障排除](Troubleshooting_zh_CN.md)。

### 还没有 LLVM：以 LLVM 21 为例

Debian / Ubuntu：如果当前软件源没有 LLVM 21，先按 [LLVM APT 软件源说明](https://apt.llvm.org/) 配置对应发行版的软件源，再安装编译器和工具：

```bash
sudo apt update
sudo apt install clang-21 llvm-21 binutils
clang-21 --version
```

macOS：先准备 Xcode Command Line Tools（尚未安装时运行 `xcode-select --install`），再通过 Homebrew 安装：

```bash
brew install llvm@21
"$(brew --prefix llvm@21)/bin/clang" --version
```

Fedora/RHEL：使用发行版提供的 LLVM，并根据安装后的实际版本选择下载文件：

```bash
sudo dnf install clang llvm binutils
clang --version
```

确认所选 Release 提供该 LLVM 版本的插件后，再执行 [快速上手](QuickStart_zh_CN.md) 的首次使用示例。

## Android：优先下载完整 bundle

选择 `amice-android-ndk-<NDK版本>-<电脑系统>.tar.gz`，它包含 NDK、插件、所需运行库和编译入口。比如在 Linux 上使用 NDK r30，下载 `amice-android-ndk-r30-linux-x86_64.tar.gz`。

文件名中的 `linux` / `darwin` 表示编译项目的电脑系统；手机的 arm64 等 ABI 在编译时选择。单独的 `libamice-android-ndk-*.so` / `.dylib` 只含插件，适合已经准备好匹配运行库的用户。

下载后按 [Android NDK 使用说明](AndroidNDKSupport_zh_CN.md) 解压并接入 CMake/Gradle。

## Rust：先检查 rustc 的 LLVM

执行 `rustc -vV`，查看其中的 `LLVM version`，再选择对应版本的插件。直接加载还需要兼容的 nightly 工具链；详细步骤见 [Rust 接入](RustUsage_zh_CN.md)。

## Windows 的下载情况

[v0.1.5-beta.4](https://github.com/fuqiuluo/amice/releases/tag/v0.1.5-beta.4) 没有提供 Windows DLL 或 Windows NDK bundle，因此这个版本没有 Windows 下载即用的路径。其他版本请查看其 Assets；Linux `.so` 和 macOS `.dylib` 不能供 Windows 编译器加载。

需要在 Windows 上自行准备插件时，见 [Windows 源码构建](LLVMSetup_zh_CN.md#windows)。这是单独的进阶流程。
