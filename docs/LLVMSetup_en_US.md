# Building Amice from Source

English | [简体中文](LLVMSetup_zh_CN.md)

This guide is for developers building Amice itself. If you downloaded a release, use [Download and Setup](Download_en_US.md), then follow [Quick Start](QuickStart_en_US.md) to integrate it into your project.

## Match the Versions First

AMICE is loaded on the compiler host. Its Cargo feature, `LLVM_SYS_*_PREFIX`, the host `clang` / `opt` and LLVM shared libraries must match. Android cross-compilation needs the [NDK toolchain](AndroidNDKSupport_en_US.md). Rust also requires compatibility with rustc's LLVM; see [Rust Usage](RustUsage_en_US.md).

The default feature is `llvm21-1`. Setting an LLVM 22 prefix does not change Cargo features: also pass `--no-default-features --features llvm22-1`.

| LLVM | Cargo feature | Environment variable |
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

These are declared features, not a promise of equal validation across versions, platforms and passes. Linux/macOS CI currently configures LLVM 18–22; Windows build workflows configure LLVM 18–20.

## Prerequisites

- Git and [Rust/Cargo](https://rustup.rs/). Repository CI uses Rust 1.89.0; other versions depend on build compatibility.
- LLVM development files: `llvm-config`, headers and libraries, plus `clang`, `opt` and `llvm-strings` for the examples.
- C++ build tools: GCC/Clang on Linux, Xcode Command Line Tools on macOS, or MSVC Build Tools and the Windows SDK on Windows.

```text
rustc --version
cargo --version
```

If you have not downloaded the source:

```text
git clone https://github.com/fuqiuluo/amice.git
cd amice
```

Run subsequent build commands from the repository root.

## Linux

### Debian / Ubuntu: LLVM 21

If your repositories do not provide `llvm-21`, first configure the appropriate source for your distribution using the [LLVM APT instructions](https://apt.llvm.org/), then install:

```bash
sudo apt update
sudo apt install build-essential llvm-21 llvm-21-dev clang-21 libpolly-21-dev
export LLVM_SYS_211_PREFIX=/usr/lib/llvm-21
"$LLVM_SYS_211_PREFIX/bin/llvm-config" --version
"$LLVM_SYS_211_PREFIX/bin/clang" --version
cargo build --release --no-default-features --features llvm21-1
```

### Fedora / RHEL

Distribution versions differ. Inspect available packages instead of assuming the latest LLVM is version 21:

```bash
dnf search llvm
sudo dnf install gcc-c++ llvm llvm-devel clang
llvm-config --version
clang --version
```

If both tools report LLVM 21:

```bash
export LLVM_SYS_211_PREFIX=$(llvm-config --prefix)
cargo build --release --no-default-features --features llvm21-1
```

For LLVM 22, use the next version-switching section. For other versions, use the feature/prefix table. When using versioned packages, obtain the prefix from that package's `llvm-config`.

## macOS

Install Xcode Command Line Tools if needed (`xcode-select --install`), then Homebrew LLVM:

```bash
brew install llvm@21
export LLVM_SYS_211_PREFIX=$(brew --prefix llvm@21)
"$LLVM_SYS_211_PREFIX/bin/llvm-config" --version
"$LLVM_SYS_211_PREFIX/bin/clang" --version
cargo build --release --no-default-features --features llvm21-1
```

Also use `"$LLVM_SYS_211_PREFIX/bin/clang"` to compile your application, avoiding an accidental switch to system Apple Clang. The plugin is `target/release/libamice.dylib`.

## Switching to LLVM 22 (Linux / macOS)

Install the corresponding LLVM 22 development packages, then choose the prefix for your platform:

```bash
# Debian / Ubuntu
export LLVM_SYS_221_PREFIX=/usr/lib/llvm-22
# macOS instead: brew install llvm@22
# export LLVM_SYS_221_PREFIX=$(brew --prefix llvm@22)
# Fedora/RHEL: use the installed version's llvm-config --prefix

"$LLVM_SYS_221_PREFIX/bin/llvm-config" --version
"$LLVM_SYS_221_PREFIX/bin/clang" --version
cargo build --release --no-default-features --features llvm22-1
```

Use this same LLVM prefix with the verification example in [Quick Start](QuickStart_en_US.md). Do not load this plugin into another clang found on PATH.

## Windows

### Prepare Native Build Tools

Install the Visual Studio Build Tools **Desktop development with C++** workload (MSVC and Windows SDK), and Rust's `x86_64-pc-windows-msvc` toolchain. Run the commands in a new **Developer PowerShell for VS**.

Windows linking needs `LLVM-C.lib` plus either `opt.lib` or `lld.lib`. Standard official LLVM installers generally do not supply this plugin development environment. One option configured in Windows CI is the [LLVM 20.1.1 plugin toolchain](https://github.com/jamesmth/llvm-project/releases/tag/v20.1.1-rust-1.87): download and extract `llvm-lld-20.1.1-rust-1.87-windows-x86_64.7z`.

Replace `C:\llvm20` below with the directory containing `bin`, `lib` and `include`. This example explicitly uses LLVM 20, rather than the repository's default LLVM 21.

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

Both versions should be 20.x and both file checks should return `True`. If the plugin toolchain lacks clang, install a matching Clang separately and set `$AmiceClang` to its absolute path; keep the plugin toolchain's `opt`, headers and libraries. The example also needs its `llvm-strings.exe`.

`$env:...` takes effect in the current terminal. `setx` and User-level environment writes only affect subsequently started processes and do not replace this step.

### Build and Load Through opt

Run from the repository root. `win-link-opt` selects `opt.exe` as the host; do not use this DLL with the Linux clang `-fpass-plugin` command.

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

Success prints `AMICE_STRING_TEST` and `PASS: same output, plaintext marker hidden`. Outputs are in `target/amice-quickstart`. Closing this dedicated terminal discards the process environment variables set by the example.

### When to Use win-link-lld

`win-link-lld` links against LLD's import library for a plugin-capable LLD to load during LTO. It is not interchangeable with `win-link-opt`. See the toolchain's [LTO plugin instructions](https://github.com/jamesmth/llvm-project#lto-pass-plugins-for-optimizing-c) for loader arguments. AMICE passes register at different stages, so loading at LTO does not imply every pass runs. Start with the opt example above.

## Verification and Troubleshooting

After building on Linux/macOS, follow the first-use example in [Quick Start](QuickStart_en_US.md), setting `AMICE_PLUGIN` to the absolute path of your build output and using compilers from the same LLVM installation. For load errors, version mismatches, ineffective configuration and logging, see [Troubleshooting](Troubleshooting_en_US.md).
