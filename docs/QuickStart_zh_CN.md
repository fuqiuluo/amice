# 快速上手

[English](QuickStart_en_US.md) | 简体中文

本文带你在 5 分钟内跑通一次 Amice：下载插件 → 验证字符串加密生效 → 接入你自己的项目。插件与编译环境的准备工作（包括 Windows 说明）见 [下载与环境准备](Download_zh_CN.md)。

## 1. 下载适合你的版本

打开 Release 的 **Assets**，按你用来编译项目的电脑和编译器选择文件。以下以 LLVM 21 和 Android NDK r30 为例：

| 使用场景 | 下载文件 |
| --- | --- |
| Linux x86_64，Clang 21 | `libamice-llvm21-linux-x86_64.so` |
| macOS Apple Silicon，Homebrew LLVM 21 | `libamice-llvm21-macos-arm64.dylib` |
| macOS Intel，Homebrew LLVM 21 | `libamice-llvm21-macos-x86_64.dylib` |
| 在 Linux 上编译 Android 项目，NDK r30 | `amice-android-ndk-r30-linux-x86_64.tar.gz` |
| 在 macOS 上编译 Android 项目，NDK r30 | `amice-android-ndk-r30-darwin-x86_64.tar.gz` |

Linux/macOS 插件文件名中的 `llvm21` 要与你本机 LLVM/Clang 的主版本一致；例如使用 Clang 20 就选 `llvm20`。Android 用户优先下载包含 NDK、插件和运行库的完整 bundle，然后按 [Android 接入指南](AndroidNDKSupport_zh_CN.md) 操作。

`.sha256` 是校验文件；`Source code` 是 Amice 源码。直接使用请选择上表这类插件或 bundle。

## 2. 先试一次字符串加密

以下是 **Linux/macOS 的 C/C++ 示例**，使用刚下载的插件。Rust 项目请直接看 [Rust 接入](RustUsage_zh_CN.md)。

先选择对应平台的设置，并把插件路径改成实际下载位置。

**Linux，Clang 21：**

```bash
AMICE_CLANG=clang-21
AMICE_CXX=clang++-21
AMICE_PLUGIN="$HOME/Downloads/libamice-llvm21-linux-x86_64.so"
```

**macOS，Homebrew LLVM 21：**

```bash
AMICE_CLANG="$(brew --prefix llvm@21)/bin/clang"
AMICE_CXX="$(brew --prefix llvm@21)/bin/clang++"
AMICE_PLUGIN="$HOME/Downloads/libamice-llvm21-macos-arm64.dylib"
# Intel Mac 将文件名改为 libamice-llvm21-macos-x86_64.dylib
```

确认编译器版本和文件路径：

```bash
"$AMICE_CLANG" --version
test -f "$AMICE_PLUGIN" && echo "Plugin found"
```

应看到 Clang 21 和 `Plugin found`。接着在一个空目录创建 `hello.c`：

```bash
cat > hello.c <<'SRC'
extern int puts(const char *);
int main(void) { return puts("AMICE_STRING_TEST") < 0; }
SRC

AMICE_STRING_ENCRYPTION=true \
  "$AMICE_CLANG" -fpass-plugin="$AMICE_PLUGIN" hello.c -o hello &&
  ./hello
```

程序应输出 `AMICE_STRING_TEST`。再检查这个字符串是否已从可执行文件中隐藏：

```bash
strings -a hello > hello.strings && {
  if grep -Fq AMICE_STRING_TEST hello.strings; then
    echo "FAIL: plaintext marker is still present"
  else
    echo "PASS: plaintext marker is hidden"
  fi
}
```

看到正确的程序输出和 `PASS`，就完成了这个示例的验证。若编译失败、`strings` 报错或仍有明文，见 [故障排除](Troubleshooting_zh_CN.md)。

## 3. 接入你的项目

在原来的编译命令中加入插件路径，并打开需要的功能。例如，为 C++ 项目启用字符串加密：

```bash
AMICE_STRING_ENCRYPTION=true \
  "$AMICE_CXX" -fpass-plugin="$AMICE_PLUGIN" main.cpp -o app
```

CMake 项目可以给指定目标添加：

```cmake
set(AMICE_PLUGIN "/absolute/path/to/downloaded/libamice-llvm21-linux-x86_64.so")
target_compile_options(your_target PRIVATE "-fpass-plugin=${AMICE_PLUGIN}")
```

然后在启动构建的终端执行 `export AMICE_STRING_ENCRYPTION=true`。`your_target` 替换为你的目标名；插件路径使用下载文件的绝对路径，CMake 的 C/C++ 编译器也要选同一套 LLVM。更换插件或配置后，重新编译受影响的源码。

只想保护某个 C/C++ 函数时，可以使用注解。下面为 `sensitive` 启用 VMP：

```c
__attribute__((noinline, annotate("+vm_virtualize")))
int sensitive(int x) {
    return (x * 7) ^ 0x55;
}
```

保留编译命令中的 `-fpass-plugin` 即可。VMP 默认使用内置 profile；详细设置见 [函数注解](FunctionAnnotations_zh_CN.md)。先从少量函数开始，并运行项目原有测试；不支持的函数可能被跳过。

## 保存常用配置

单个功能用环境变量开启；多项配置可以保存为 `amice.toml`：

```toml
[string_encryption]
enable = true

[flatten]
enable = true
mode = "basic"
```

编译时指定这个文件：

```bash
AMICE_CONFIG_PATH="$(pwd)/amice.toml" \
  "$AMICE_CLANG" -fpass-plugin="$AMICE_PLUGIN" hello.c -o hello
```

支持函数注解的功能按 **函数注解 > 环境变量 > 配置文件 > 默认值** 应用设置。字符串加密使用全局配置。需要改变多个功能的执行顺序时，见 [Pass 运行顺序](PassOrder_zh_CN.md)。

## 下一步

- 查询全部开关和参数：[运行时环境变量](EnvConfig_zh_CN.md)
- 按函数启用/禁用：[函数注解](FunctionAnnotations_zh_CN.md)
- 遇到问题：[故障排除](Troubleshooting_zh_CN.md)
