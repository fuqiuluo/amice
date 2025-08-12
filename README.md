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

[点击这里](https://github.com/fuqiuluo/amice/wiki)

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

## Android NDK

安卓的 LLVM 工具链可以动态加载插件，虽然安卓的clang没有提供`opt`工具，但您可以使用`clang`来编译您的代码，包括但不限于基于LLVM插件，或者将混淆器内敛进您的clang中。

### Plugin

首先我们需要下载ndk对应的未精简的clang，详细教程这里面有：[Ylarod：NDK加载 LLVM Pass 方案](https://xtuly.cn/article/ndk-load-llvm-pass-plugin)!

```shell
# r522817 对应 NDK 25c
export CXX="/linux-x86-refs_heads_main-clang-r522817/bin/clang++"
export CXXFLAGS="-stdlib=libc++ -I/linux-x86-refs_heads_main-clang-r522817/include/c++/v1"
export LDFLAGS="-stdlib=libc++ -L/linux-x86-refs_heads_main-clang-r522817/lib"

# 例如这里我的ndk是llvm-18.0的
# 但是llvm-plugin-rs只有18.1的版本，只能设置LLVM_SYS_181_PREFIX来指向未精简的clang
# (注意：llvm的API在这两个版本没有大的变化，这样改不会影响什么)
export LLVM_SYS_181_PREFIX=/您的NDK对应的未精简的clang

cargo build --release

# export LD_LIBRARY_PATH=/您的NDK对应的未精简的clang/lib
# 由于一些奇怪的问题，可能需要设置 LD_LIBRARY_PATH 来指向未精简的clang的lib目录
# 因为他依赖了`libLLVM.so`，这在android ndk里面是没有的！
/home/fuqiuluo/android-ndk-r25c/toolchains/llvm/prebuilt/linux-x86_64/bin/clang \
  -fpass-plugin=../target/release/libamice.so \
  -Xclang -load -Xclang ../target/release/libamice.so \
   luo.c -o luo
```

### Inline 

> TODO

# Thanks

- [Project: LLVM](https://llvm.org/)
- [Project: jamesmth/llvm-plugin-rs](https://github.com/jamesmth/llvm-plugin-rs/tree/feat/llvm-20#)
- [Project: stevefan1999-personal/llvm-plugin-rs](https://github.com/stevefan1999-personal/llvm-plugin-rs)
- [Project: SsagePass](https://github.com/SsageParuders/SsagePass)
- [Article: MBA](https://plzin.github.io/posts/mba)
- [Article: llvm PassManager的变更及动态注册Pass的加载过程](https://bbs.kanxue.com/thread-272801.htm)
- [Person: Ylarod](https://github.com/Ylarod)
