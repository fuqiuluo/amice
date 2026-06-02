# Documentation / 文档

## Start Here

| Need | English | 简体中文 |
| --- | --- | --- |
| Build AMICE on Linux/macOS/Windows | [LLVM Setup](LLVMSetup_en_US.md) | [LLVM 环境配置](LLVMSetup_zh_CN.md) |
| Use AMICE with Android NDK | [Android NDK Usage](AndroidNDKSupport_en_US.md) | [Android NDK 使用说明](AndroidNDKSupport_zh_CN.md) |
| Enable passes with env vars | [Runtime Environment Variables](EnvConfig_en_US.md) | [运行时环境变量](EnvConfig_zh_CN.md) |
| Enable/disable passes per function | [Function Annotations](FunctionAnnotations_en_US.md) | [函数注解](FunctionAnnotations_zh_CN.md) |
| Control pass order | [Pass Execution Order](PassOrder_en_US.md) | [Pass 运行顺序](PassOrder_zh_CN.md) |
| Fix build/plugin-load errors | [Troubleshooting](Troubleshooting_en_US.md) | [故障排除](Troubleshooting_zh_CN.md) |

## Android NDK

Most Android failures are not target ABI problems. The usual issue is that the official NDK clang can load plugins, but the NDK package does not include the host `libLLVM.so`/`libLLVM.dylib` required by `libamice`.

Use the release bundle first:

- Linux: `amice-android-ndk-r29-linux-x86_64.tar.gz`
- macOS: `amice-android-ndk-r29-darwin-x86_64.tar.gz`

Then follow [Android NDK Usage](AndroidNDKSupport_en_US.md) / [Android NDK 使用说明](AndroidNDKSupport_zh_CN.md).
