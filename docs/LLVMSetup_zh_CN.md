# 从源码构建 Amice

[English](LLVMSetup_en_US.md) | 简体中文

本文面向自行构建 Amice 的开发者。已经下载发布版的用户请看 [下载与环境准备](Download_zh_CN.md)，然后按 [快速上手](QuickStart_zh_CN.md) 接入自己的项目。

## 先匹配版本

AMICE 是宿主机加载的 LLVM 插件。Cargo feature、`LLVM_SYS_*_PREFIX`、加载插件的 `clang` / `opt` 和 LLVM 动态库必须匹配。交叉编译 Android 时使用 [NDK 专用工具链](AndroidNDKSupport_zh_CN.md)。Rust 接入还要匹配 rustc 自带的 LLVM，见 [Rust 使用说明](RustUsage_zh_CN.md)。

默认 feature 为 `llvm21-1`。设置 LLVM 22 的环境变量不会自动切换 Cargo feature；必须同时使用 `--no-default-features --features llvm22-1`。

| LLVM | Cargo feature | 环境变量 |
| --- | --- | --- |
| 22.1 | `llvm22-1` | `LLVM_SYS_221_PREFIX` |
| 21.1 | `llvm21-1` | `LLVM_SYS_211_PREFIX` |
| 20.1 | `llvm20-1` | `LLVM_SYS_201_PREFIX` |
| 19.1 | `llvm19-1` | `LLVM_SYS_191_PREFIX` |
| 18.1 | `llvm18-1` | `LLVM_SYS_181_PREFIX` |
| 17.0 | `llvm17-0` | `LLVM_SYS_170_PREFIX` |
| 16.0 | `llvm16-0` | `LLVM_SYS_160_PREFIX` |
| 15.0 | `llvm15-0` | `LLVM_SYS_150_PREFIX` |
| 14.0 | `llvm14-0` | `LLVM_SYS_140_PREFIX` |
| 13.0 | `llvm13-0` | `LLVM_SYS_130_PREFIX` |
| 12.0 | `llvm12-0` | `LLVM_SYS_120_PREFIX` |
| 11.0 | `llvm11-0` | `LLVM_SYS_110_PREFIX` |

以上是代码声明的 feature，不代表每个版本、平台和 Pass 组合都有相同的验证覆盖。当前 Linux/macOS CI 配置 LLVM 18–22，Windows 构建工作流配置 LLVM 18–20。

## 通用前置条件

- Git 和 [Rust/Cargo](https://rustup.rs/)。仓库 CI 使用 Rust 1.89.0；使用其他版本时以实际构建结果为准。
- LLVM 开发文件：`llvm-config`、头文件、库文件，以及用于运行示例的 `clang`、`opt`、`llvm-strings`。
- C++ 构建工具：Linux 的 GCC/Clang、macOS 的 Xcode Command Line Tools，或 Windows 的 MSVC Build Tools 和 Windows SDK。

```text
rustc --version
cargo --version
```

尚未下载源码时：

```text
git clone https://github.com/fuqiuluo/amice.git
cd amice
```

后续构建命令均在仓库根目录执行。

## Linux

### Debian / Ubuntu：LLVM 21

如果系统软件源没有 `llvm-21`，先按 [LLVM APT 软件源说明](https://apt.llvm.org/) 为当前发行版配置对应源，再安装：

```bash
sudo apt update
sudo apt install build-essential llvm-21 llvm-21-dev clang-21 libpolly-21-dev
export LLVM_SYS_211_PREFIX=/usr/lib/llvm-21
"$LLVM_SYS_211_PREFIX/bin/llvm-config" --version
"$LLVM_SYS_211_PREFIX/bin/clang" --version
cargo build --release --no-default-features --features llvm21-1
```

### Fedora / RHEL

发行版提供的版本会不同，先查询可用包；不要把“最新 LLVM”当作固定的 LLVM 21：

```bash
dnf search llvm
sudo dnf install gcc-c++ llvm llvm-devel clang
llvm-config --version
clang --version
```

若上述工具均为 LLVM 21，使用：

```bash
export LLVM_SYS_211_PREFIX=$(llvm-config --prefix)
cargo build --release --no-default-features --features llvm21-1
```

若安装的是 LLVM 22，改用下一节的 LLVM 22 命令。其他版本按对应表选择 feature 和变量；使用版本化软件包时，以那个包的 `llvm-config` 返回的 prefix 为准。

## macOS

先安装 Xcode Command Line Tools（尚未安装时执行 `xcode-select --install`），再安装 Homebrew LLVM：

```bash
brew install llvm@21
export LLVM_SYS_211_PREFIX=$(brew --prefix llvm@21)
"$LLVM_SYS_211_PREFIX/bin/llvm-config" --version
"$LLVM_SYS_211_PREFIX/bin/clang" --version
cargo build --release --no-default-features --features llvm21-1
```

编译目标程序时也使用 `"$LLVM_SYS_211_PREFIX/bin/clang"`，避免误用系统 Apple Clang。产物为 `target/release/libamice.dylib`。

## 切换到 LLVM 22（Linux / macOS）

先安装对应的 LLVM 22 开发包，然后只执行适合当前系统的 prefix 设置：

```bash
# Debian / Ubuntu
export LLVM_SYS_221_PREFIX=/usr/lib/llvm-22
# macOS 改用：brew install llvm@22
# export LLVM_SYS_221_PREFIX=$(brew --prefix llvm@22)
# Fedora/RHEL 按所安装版本的 llvm-config --prefix 设置

"$LLVM_SYS_221_PREFIX/bin/llvm-config" --version
"$LLVM_SYS_221_PREFIX/bin/clang" --version
cargo build --release --no-default-features --features llvm22-1
```

使用相同 LLVM prefix 运行 [快速上手中的编译与验证示例](QuickStart_zh_CN.md)。不要让 PATH 中另一个版本的 clang 加载这个插件。

## Windows

### 准备原生工具链

安装 Visual Studio Build Tools 的“使用 C++ 的桌面开发”组件（含 MSVC 和 Windows SDK）及 Rust 的 `x86_64-pc-windows-msvc` 工具链，在新的 **Developer PowerShell for VS** 中执行下面的命令。

AMICE 的 Windows 链接需要 `LLVM-C.lib`，以及 `opt.lib` 或 `lld.lib`。普通官方 LLVM 安装包通常不提供这套插件开发环境。可选择 Windows CI 使用的 [LLVM 20.1.1 插件工具链](https://github.com/jamesmth/llvm-project/releases/tag/v20.1.1-rust-1.87)，下载 `llvm-lld-20.1.1-rust-1.87-windows-x86_64.7z` 并解压。

将下面的 `C:\llvm20` 改成包含 `bin`、`lib`、`include` 的真实目录。示例显式使用 LLVM 20，不使用仓库默认的 LLVM 21。

```powershell
$ErrorActionPreference = 'Stop'
$env:LLVM_SYS_201_PREFIX = 'C:\llvm20'
$env:PATH = "$env:LLVM_SYS_201_PREFIX\bin;$env:PATH"
$AmiceClang = Join-Path $env:LLVM_SYS_201_PREFIX 'bin\clang.exe'

& "$env:LLVM_SYS_201_PREFIX\bin\llvm-config.exe" --version
if ($LASTEXITCODE -ne 0) { throw 'llvm-config failed' }
& $AmiceClang --version
if ($LASTEXITCODE -ne 0) { throw 'clang failed' }
Test-Path "$env:LLVM_SYS_201_PREFIX\lib\LLVM-C.lib"
Test-Path "$env:LLVM_SYS_201_PREFIX\lib\opt.lib"
```

版本输出应为 20.x，两个文件检查都应为 `True`。如果所选工具链没有 clang，可另装匹配版本的 Clang，并将 `$AmiceClang` 设为其绝对路径；仍保留插件工具链的 `opt`、头文件和库文件。示例还需要该 LLVM 的 `llvm-strings.exe`。

这里使用 `$env:...`，立即对当前终端生效。`setx` 或写入 User 环境变量只影响之后启动的进程，不能替代本步骤。

### 构建并通过 opt 加载

在仓库根目录执行。`win-link-opt` 对应 `opt.exe` 宿主；选择它后，不要拿这个 DLL 直接套用 Linux 的 clang `-fpass-plugin` 命令。

```powershell
cargo build --release --no-default-features --features llvm20-1,win-link-opt
if ($LASTEXITCODE -ne 0) { throw 'AMICE build failed' }
$AmicePlugin = (Resolve-Path .\target\release\amice.dll).Path
$AmiceOutput = Join-Path (Get-Location) 'target\amice-quickstart'
New-Item -ItemType Directory -Force -Path $AmiceOutput | Out-Null
@"
extern int puts(const char *);
int main(void) { return puts("AMICE_STRING_TEST") < 0; }
"@ | Set-Content -Encoding ascii "$AmiceOutput\hello.c"

& $AmiceClang "$AmiceOutput\hello.c" -o "$AmiceOutput\plain.exe"
if ($LASTEXITCODE -ne 0) { throw 'Baseline compile failed' }
& $AmiceClang -S -emit-llvm "$AmiceOutput\hello.c" -o "$AmiceOutput\input.ll"
if ($LASTEXITCODE -ne 0) { throw 'IR generation failed' }

$env:AMICE_STRING_ENCRYPTION = 'true'
$env:RUST_LOG = 'amice=info'
& "$env:LLVM_SYS_201_PREFIX\bin\opt.exe" "--load-pass-plugin=$AmicePlugin" `
    '-passes=default<O0>' -S "$AmiceOutput\input.ll" -o "$AmiceOutput\obfuscated.ll"
if ($LASTEXITCODE -ne 0) { throw 'Plugin execution failed' }
& $AmiceClang "$AmiceOutput\obfuscated.ll" -o "$AmiceOutput\obfuscated.exe"
if ($LASTEXITCODE -ne 0) { throw 'Obfuscated compile failed' }

$AmicePlain = & "$AmiceOutput\plain.exe"
if ($LASTEXITCODE -ne 0) { throw 'Baseline run failed' }
$AmiceObfuscated = & "$AmiceOutput\obfuscated.exe"
if ($LASTEXITCODE -ne 0) { throw 'Obfuscated run failed' }
if ($AmicePlain -ne 'AMICE_STRING_TEST' -or $AmiceObfuscated -ne $AmicePlain) {
    throw 'Output mismatch'
}
$AmicePlainStrings = & "$env:LLVM_SYS_201_PREFIX\bin\llvm-strings.exe" "$AmiceOutput\plain.exe"
if ($LASTEXITCODE -ne 0) { throw 'Baseline strings check failed' }
$AmiceHiddenStrings = & "$env:LLVM_SYS_201_PREFIX\bin\llvm-strings.exe" "$AmiceOutput\obfuscated.exe"
if ($LASTEXITCODE -ne 0) { throw 'Obfuscated strings check failed' }
if (-not ($AmicePlainStrings -match 'AMICE_STRING_TEST') -or ($AmiceHiddenStrings -match 'AMICE_STRING_TEST')) {
    throw 'String encryption verification failed'
}
$AmiceObfuscated
'PASS: same output, plaintext marker hidden'
```

成功时输出 `AMICE_STRING_TEST` 和 `PASS: same output, plaintext marker hidden`。文件保存在 `target/amice-quickstart`。关闭这个专用终端即可丢弃示例设置的进程环境变量。

### win-link-lld 的适用范围

`win-link-lld` 将插件链接到 LLD 的导入库，用于支持插件的 LLD 在 LTO 链接阶段加载；它不是 `win-link-opt` 的通用替代品。上游工具链的 [LTO 插件说明](https://github.com/jamesmth/llvm-project#lto-pass-plugins-for-optimizing-c) 介绍了加载参数。AMICE 各 Pass 的注册阶段不同，不能假设 LTO 加载会运行所有 Pass；首次使用请先完成上面的 opt 示例。

## 检查效果与排障

Linux/macOS 构建后可执行 [快速上手的首次使用示例](QuickStart_zh_CN.md)，将其中的 `AMICE_PLUGIN` 改为本次构建产物的绝对路径，并使用同一套 LLVM 的编译器。加载失败、版本冲突、配置不生效和日志诊断见 [故障排除](Troubleshooting_zh_CN.md)。
