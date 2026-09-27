# 开发 Amice

[English](Development_en_US.md) | 简体中文

本文面向修改、构建或测试 Amice 本身的贡献者。直接使用发布版请从 [快速上手](QuickStart_zh_CN.md) 开始。

## 源码构建

Amice 是基于 Rust、`llvm-plugin` 和 `inkwell` 的 Cargo workspace。主插件 crate 位于 `crates/amice`；release 构建产物为 Linux 的 `target/release/libamice.so`、macOS 的 `target/release/libamice.dylib` 或 Windows 的 `target/release/amice.dll`。

依赖安装、LLVM feature 对应和各平台命令见 [从源码构建](LLVMSetup_zh_CN.md)。默认 LLVM feature 为 `llvm21-1`。

## Rust 集成测试的边界

测试工具在 rustc 与插件的 LLVM 主版本不同时，可以使用专门的“输出 IR → opt → llc → 链接”流程；其中包含特定 IR 兼容处理，不应把这个测试能力当作任意 Rust 项目的接入保证。`AMICE_RUST_TOOLCHAIN` 选择测试工具链，不改变插件的 LLVM feature。

## 测试

集成测试会调用 clang 加载 release 版插件，因此请使用 `--release`。测试脚本会自动探测 `llvm-config`，也支持 `LLVM_SYS_*_PREFIX`。

```bash
# Linux/macOS：构建并运行全部测试
./crates/amice/tests/scripts/run_tests.sh --build

# 只运行名称匹配的测试
./crates/amice/tests/scripts/run_tests.sh -v string

# 直接使用 cargo
cargo test --release --no-default-features --features llvm21-1
cargo test --release --no-default-features --features llvm21-1 --test string_encryption
cargo test --release --no-default-features --features llvm21-1 test_md5

# LLVM 22 示例
LLVM_SYS_221_PREFIX=/usr/lib64/llvm22 cargo test --release --no-default-features --features llvm22-1
```

Windows 请先完成 [原生 opt 验证](LLVMSetup_zh_CN.md#windows)。完整测试脚本的适用范围见 [crates/amice/tests/README.md](../crates/amice/tests/README.md)。

---

## 项目结构

| 路径 | 说明 |
|:---|:---|
| `crates/amice` | 主 clang pass 插件，注册和实现各类混淆 pass |
| `crates/amice-llvm` | LLVM/inkwell 扩展层和 C++ FFI glue |
| `crates/amice-macro` | `#[amice(...)]` pass 注册宏和配置宏 |
| `crates/amice-plugin` | pass manager / pass builder 适配层 |
| `crates/amice-plugin-macros` | plugin 适配层宏 |
| `crates/amice-build-support` | 构建期 LLVM 探测辅助 |
| `docs` | 构建、环境变量、函数注解、Android NDK 和排障文档 |
| `scripts` | Android NDK bundle 打包和辅助构建脚本 |

---
