# Documentation / 文档

[← 返回项目 README](../README.md) / [← Back to README (English)](../README_en_US.md)

## Using Amice / 使用 Amice

| Need | English | 简体中文 |
| --- | --- | --- |
| Run a first verified example in five minutes | [Quick Start](QuickStart_en_US.md) | [快速上手](QuickStart_zh_CN.md) |
| Choose a package and prepare the runtime | [Download and Setup](Download_en_US.md) | [下载与环境准备](Download_zh_CN.md) |
| Integrate into a Rust/Cargo project | [Rust Usage](RustUsage_en_US.md) | [Rust 接入](RustUsage_zh_CN.md) |
| Use AMICE with Android NDK | [Android NDK Usage](AndroidNDKSupport_en_US.md) | [Android NDK 使用说明](AndroidNDKSupport_zh_CN.md) |
| Enable passes with env vars | [Runtime Environment Variables](EnvConfig_en_US.md) | [运行时环境变量](EnvConfig_zh_CN.md) |
| Enable/disable passes per function | [Function Annotations](FunctionAnnotations_en_US.md) | [函数注解](FunctionAnnotations_zh_CN.md) |
| Control pass order | [Pass Execution Order](PassOrder_en_US.md) | [Pass 运行顺序](PassOrder_zh_CN.md) |
| Fix plugin loading or ineffective settings | [Troubleshooting](Troubleshooting_en_US.md) | [故障排除](Troubleshooting_zh_CN.md) |

## Developing Amice / 开发 Amice

| Need | English | 简体中文 |
| --- | --- | --- |
| Project structure and tests | [Development Guide](Development_en_US.md) | [开发指南](Development_zh_CN.md) |
| Build the plugin from source | [Source Builds](LLVMSetup_en_US.md) | [源码构建](LLVMSetup_zh_CN.md) |
| VMP implementation spec | - | [VMP 虚拟化实现规范](VMPDesign_zh_CN.md) |
| MBA region design | - | [MBA 区域设计](MbaRegions.md) |

## Android NDK note / Android NDK 提示

Most Android failures are not target ABI problems: the official NDK clang can load plugins, but the NDK package does not include the host `libLLVM.so`/`libLLVM.dylib` required by `libamice`. Download the matching host bundle from [AMICE Releases](https://github.com/fuqiuluo/amice/releases) — `amice-android-ndk-r30-linux-x86_64.tar.gz` or `amice-android-ndk-r30-darwin-x86_64.tar.gz`; no Windows host bundle is currently provided — then follow [Android NDK Usage](AndroidNDKSupport_en_US.md) / [Android NDK 使用说明](AndroidNDKSupport_zh_CN.md).
