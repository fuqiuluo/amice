# Android NDK 支持

## 背景说明

Android NDK 和上游的 LLVM Clang 版本存在不一致的问题。为了正确构建和使用 AMICE 插件，需要使用与 Android NDK 版本匹配的完整版 Clang。

## 查看 Android NDK 信息

首先，查看当前 Android NDK 使用的 LLVM 版本信息：

```bash
cat $ANDROID_HOME/ndk/25.2.9519653/toolchains/llvm/prebuilt/linux-x86_64/AndroidVersion.txt
```

输出内容示例：

```
14.0.7
based on r450784d1
for additional information on LLVM revision and cherry-picks, see clang_source_info.md
```

## 获取匹配的完整版 Clang

根据版本信息中的 `r450784d1`，访问 Google 的预构建 Clang 仓库找到对应分支：

🔗 [https://android.googlesource.com/platform/prebuilts/clang/host/linux-x86/+log/refs/heads/master/clang-r450784d](https://android.googlesource.com/platform/prebuilts/clang/host/linux-x86/+log/refs/heads/master/clang-r450784d)

**详细下载教程**: [https://xtuly.cn/article/ndk-load-llvm-pass-plugin](https://xtuly.cn/article/ndk-load-llvm-pass-plugin)

下载完整版（未精简）的 Clang，然后使用该版本编译 AMICE，以获得与当前 Android NDK (Clang) 兼容的 AMICE 插件库文件。

## 构建脚本

以下是构建 AMICE 的示例脚本：

```bash
# r522817是llvm18-1
export LLVM_SYS_181_PREFIX=/home/fuqiuluo/下载/linux-x86-refs_heads_main-clang-r522817

#cargo clean
export CXX="/home/fuqiuluo/下载/linux-x86-refs_heads_main-clang-r522817/bin/clang++"
export CXXFLAGS="-stdlib=libc++ -I/home/fuqiuluo/下载/linux-x86-refs_heads_main-clang-r522817/include/c++/v1"
export LDFLAGS="-stdlib=libc++ -L/home/fuqiuluo/下载/linux-x86-refs_heads_main-clang-r522817/lib"

cargo b --release --no-default-features --features llvm18-1,android-ndk
```

## 编译使用方式

### 使用完整版 Clang 编译

构建成功后，可以直接使用完整版 Clang 编译源文件：

```bash
# 设置库依赖路径，因为插件依赖 libLLVM.so
export LD_LIBRARY_PATH="/home/fuqiuluo/下载/linux-x86-refs_heads_main-clang-r522817/lib"
/home/fuqiuluo/下载/linux-x86-refs_heads_main-clang-r522817/bin/clang \
  -fpass-plugin=../target/release/libamice.so \
  test1.c -o test1
```

### 使用 Android NDK Toolchain 编译

也可以直接使用 Android NDK toolchain 中的 Clang 进行编译：

```bash
/home/fuqiuluo/android-kernel/android-ndk-r25c/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android21-clang \
  -fpass-plugin=../target/release/libamice.so \
  test1.c -o test_ndk
```

## 配套资源

**Android NDK r25c 配套版本**: [https://github.com/fuqiuluo/amice/releases/tag/android-ndk-r25c](https://github.com/fuqiuluo/amice/releases/tag/android-ndk-r25c)

配套构建命令：
```bash
cargo b --release --no-default-features --features llvm18-1
```

## 常见问题及解决方案

### 符号未定义错误

如果在载入时出现以下错误：
```
error: unable to load plugin './target/release/libamice.so': './target/release/libamice.so: undefined symbol: _ZTIN4llvm10CallbackVHE'
```

尝试添加新的 feature：
```bash
cargo b --release --no-default-features --features llvm18-1,android-ndk
```

### 版本不匹配错误

如果出现类似错误：
```
error: unable to load plugin '/home/who/amice/target/release/libamice.so': 'Could not load library '/home/who/amice/target/release/libamice.so': /usr/lib/llvm-18/lib/libLLVM-18.so: version `LLVM_18' not found (required by /home/who/amice/target/release/libamice.so)'
```

**解决步骤：**

1. 检查 Clang 版本：
   ```bash
   $ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin/clang --version
   ```

2. 设置完整版 Clang 库路径：
   ```bash
   export LD_LIBRARY_PATH=/path/to/unstripped-clang/lib:$LD_LIBRARY_PATH
   ```

## 集成到构建系统

### CMake 集成

在 CMake 中使用插件：

```cmake
target_compile_options(${PROJECT_NAME} PRIVATE
    -fpass-plugin=${PLUGIN_PATH}
    -Xclang -load -Xclang ${PLUGIN_PATH}
)
```

### Gradle 集成

配合 Gradle 使用：

```gradle
externalNativeBuild {
    cmake {
        arguments(
            "-DCMAKE_VERBOSE_MAKEFILE=ON",
            "-DPLUGIN_PATH=/home/who/amice/target/release/libamice.so"
        )
        targets += "[your target name]"
    }
}
```

## 调试和日志

构建成功运行后，可以启用日志查看详细信息：

```bash
export RUST_LOG=info
```

## 更多信息

更多详细信息请参考：[https://github.com/fuqiuluo/amice/wiki](https://github.com/fuqiuluo/amice/wiki)

> 感谢 [Android1500](https://github.com/Android1500) 在 https://github.com/fuqiuluo/amice/discussions/55 的讨论与研究。