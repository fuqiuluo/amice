# Amice

[English](README_en_US.md) | 简体中文

[![Release](https://img.shields.io/github/v/release/fuqiuluo/amice?include_prereleases)](https://github.com/fuqiuluo/amice/releases)
[![CI](https://github.com/fuqiuluo/amice/actions/workflows/linux-x64-build.yml/badge.svg)](https://github.com/fuqiuluo/amice/actions/workflows/linux-x64-build.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

Amice 是一个以 LLVM Pass 插件形式工作的代码混淆工具，为 C/C++、Rust 和 Android 原生代码提供字符串加密、控制流混淆和指令级 VMP 虚拟化。下载预编译插件后，一条 `-fpass-plugin` 参数即可接入现有构建——不需要重新编译 LLVM，也不需要修改编译器或项目源码。

> **状态**：当前为 beta（v0.1.5-beta.5），配置项和行为可能随版本调整。预编译插件覆盖 Linux/macOS 上的 LLVM/Clang 18–22，以及 Android NDK r27d–r30（其中 r29/r30 提供含 NDK 和运行库的完整 bundle，r27d/r28c 仅提供插件）；Windows 暂无预编译包，可[从源码构建](docs/LLVMSetup_zh_CN.md)。

## 特性

- **即插即用** — 以动态库形式加载进 clang，无需重编 LLVM；使用 Clang 的现有项目只加一个编译参数即可接入
- **字符串加密** — `xor` / `simd_xor` 算法，支持 lazy 与程序启动时解密、栈/堆分配
- **控制流混淆** — 控制流平坦化、VM控制流平坦化、[BCF](docs/EnvConfig_zh_CN.md#虚假控制流混淆)、间接调用/跳转、基本块拆分与重排等
- **指令级 VMP** — 将函数提升为 VM bytecode 并生成解释执行的 runtime
- **MBA 混淆** — 混合布尔算术重写，覆盖整数与 binary64 浮点区域
- **函数级控制** — 通过 `__attribute__((annotate(...)))` 按函数选择混淆策略
- **多语言接入** — C/C++、Rust（nightly `-Zllvm-plugins`）、Android NDK（CMake/Gradle bundle）
- **可组合** — 全部开关、参数与 Pass 执行顺序通过环境变量或 `amice.toml` 配置

完整的 Pass 支持矩阵（语言覆盖 × 环境变量开关）见 [运行时环境变量](docs/EnvConfig_zh_CN.md)。

## 快速开始

从 [Releases](https://github.com/fuqiuluo/amice/releases) 下载与你的 LLVM 主版本匹配的插件后，在原有编译命令上加载插件并打开功能开关：

```bash
AMICE_STRING_ENCRYPTION=true \
  clang-21 -fpass-plugin=/path/to/libamice-llvm21-linux-x86_64.so hello.c -o hello
# 程序行为不变，但二进制中的明文字符串已被加密隐藏
```

第一次使用？跟着 [快速上手](docs/QuickStart_zh_CN.md) 用五分钟跑通一个可验证的示例，含 macOS、Android、Rust 与 CMake 接入。

> **使用注意**：请仅在你有权保护的软件上使用 Amice。混淆会改变代码生成，接入后请运行项目原有测试；混淆产物可能触发杀毒软件或应用商店的误报；VMP 等强混淆会增大体积、影响运行性能，建议按函数启用。

## 文档

| 你想做什么 | 文档 |
| --- | --- |
| 五分钟跑通第一个示例 | [快速上手](docs/QuickStart_zh_CN.md) |
| 选择下载包、准备运行环境 | [下载与环境准备](docs/Download_zh_CN.md) |
| 接入 Android CMake / Gradle 项目 | [Android NDK](docs/AndroidNDKSupport_zh_CN.md) |
| 接入 Rust / Cargo 项目 | [Rust 接入](docs/RustUsage_zh_CN.md) |
| 查询开关、参数、默认值和支持矩阵 | [运行时环境变量](docs/EnvConfig_zh_CN.md) |
| 只处理或排除指定函数 | [函数注解](docs/FunctionAnnotations_zh_CN.md) |
| 控制功能组合与执行顺序 | [Pass 运行顺序](docs/PassOrder_zh_CN.md) |
| 解决加载失败、配置无效等问题 | [故障排除](docs/Troubleshooting_zh_CN.md) |

## 参与贡献

欢迎通过 issue 和 PR 参与。开发环境搭建、测试运行方式和项目结构见 [开发指南](docs/Development_zh_CN.md)，从源码构建见 [LLVM 环境配置](docs/LLVMSetup_zh_CN.md)。

## 鸣谢

- LLVM Project: <https://llvm.org/>
- llvm-plugin-rs
  - <https://github.com/jamesmth/llvm-plugin-rs/tree/feat/llvm-20>
  - <https://github.com/stevefan1999-personal/llvm-plugin-rs>
- Obfuscator-LLVM: <https://github.com/obfuscator-llvm/obfuscator>
- SsagePass: <https://github.com/SsageParuders/SsagePass>
- Polaris-Obfuscator: <https://github.com/za233/Polaris-Obfuscator>
- YANSOllvm: <https://github.com/emc2314/YANSOllvm>
- MBA: <https://plzin.github.io/posts/mba>
- LLVM PassManager 变更及动态注册: <https://bbs.kanxue.com/thread-272801.htm>

## 许可证

Amice 以 [Apache-2.0](LICENSE) 发布。

> © 2025-2026 Fuqiuluo & Contributors.
