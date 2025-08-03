# Amice 

# How to use

```shell
# 在此之前，您需要构建 Amice 的 Rust 代码库，生成 `libamice.so` 插件。
# export RUST_LOG=debug 如果需要调试日志

clang \
  -fpass-plugin=libamice.so \
  your_source.c -o your_source
```

# Runtime environment variables

| 变量名                            | 说明                                                                                                                             | 默认值   |
|--------------------------------|--------------------------------------------------------------------------------------------------------------------------------|-------|
| AMICE_STRING_ALGORITHM         | 控制字符串的加密算法：<br/>• `xor` —— 使用异或加密字符串。<br/>• `simd_xor` —— (beta) 使用SIMD指令的异或加密字符串。                                             | aes   |
| AMICE_STRING_DECRYPT_TIMING    | 控制字符串的解密时机：<br/>• `global` —— 程序启动时在全局初始化阶段一次性解密所有受保护字符串；<br/>• `lazy` —— 在每个字符串首次被使用前按需解密（随后可缓存）。 <br/>  备注：解密在栈上的字符串不支持这个配置！ | lazy  |
| AMICE_STRING_STACK_ALLOC       | (beta) 控制解密字符串的内存分配方式：<br/>• `true` —— 将解密的字符串分配到栈上；<br/>• `false` —— 将解密的字符串分配到堆上。<br/>  备注：栈分配模式下仅支持 `lazy` 解密时机！            | false |
| AMICE_STRING_INLINE_DECRYPT_FN | 控制是否内联解密函数：<br/>• `true` ——内联解密函数；<br/>• `false` —— 不内联解密函数。                                                                   | false |

# Build Me

## Linux & MacOS Requirements

您的 LLVM 工具链应能动态链接 LLVM 库！ 目前来说您可以使用`apt` 或者 `homebrew` 安装LLVM，而不需要其它大量额外的配置！

> Fedora的用户请注意，您需要安装的是 `llvm-devel` 包。

<details>
 <summary><em>安装 LLVM-14 通过 apt</em></summary>

 ```shell
 $ apt install llvm-14
 ```

 </details>

<details>
 <summary><em>安装 LLVM-14 通过 homebrew</em></summary>

 ```shell
 $ brew install llvm@14
 ```

 </details>

如果您不使用这些包管理器中的任何一个，您可以从自己编译或者下载编译好了的LLVM工具包。
在这种情况下，不要忘记配置LLVM 工具链路径。

例如，如果您的 LLVM-14 工具链位于`~/llvm`，则应设置以下任一选项：
- `PATH=$PATH;$HOME/llvm/bin`
- `LLVM_SYS_140_PREFIX=$HOME/llvm`

## Windows Requirements

适用于 Windows 的官方 LLVM 工具链迄今为止不支持使用动态加载的LLVM插件，您可能需要手动编译一个`clang/llvm`。也许，这里可以找到兼容的工具链：[点我](https://github.com/jamesmth/llvm-project/releases).

不要忘记使用 LLVM 工具链路径更新您的`PATH`环境变量，或更新`LLVM_SYS_XXX_PREFIX`环境变量来定位您的工具链。

例如，如果您的 LLVM-14 工具链位于`C:\llvm`，则应设置以下任一选项：
- `PATH=$PATH;C:\llvm\bin`
- `LLVM_SYS_140_PREFIX=C:\llvm`

# Thanks

- [jamesmth/llvm-plugin-rs](https://github.com/jamesmth/llvm-plugin-rs/tree/feat/llvm-20#)
- [stevefan1999-personal/llvm-plugin-rs](https://github.com/stevefan1999-personal/llvm-plugin-rs)
- [llvm PassManager的变更及动态注册Pass的加载过程](https://bbs.kanxue.com/thread-272801.htm)
- [SsagePass](https://github.com/SsageParuders/SsagePass)