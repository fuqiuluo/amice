# Android NDK Support

## Background

Android NDK and upstream LLVM Clang versions may not be consistent. To correctly build and use the AMICE plugin, you need to use a complete (unstripped) Clang that matches the Android NDK version.

## Check Android NDK Information

First, check the LLVM version information used by your current Android NDK:

```bash
cat $ANDROID_HOME/ndk/25.2.9519653/toolchains/llvm/prebuilt/linux-x86_64/AndroidVersion.txt
```

Example output:

```
14.0.7
based on r450784d1
for additional information on LLVM revision and cherry-picks, see clang_source_info.md
```

## Obtain Matching Complete Clang

Based on the `r450784d1` in the version information, visit Google's prebuilt Clang repository to find the corresponding branch:

[https://android.googlesource.com/platform/prebuilts/clang/host/linux-x86/+log/refs/heads/master/clang-r450784d](https://android.googlesource.com/platform/prebuilts/clang/host/linux-x86/+log/refs/heads/master/clang-r450784d)

**Detailed download tutorial**: [https://xtuly.cn/article/ndk-load-llvm-pass-plugin](https://xtuly.cn/article/ndk-load-llvm-pass-plugin)

Download the complete (unstripped) Clang, then use that version to compile AMICE to get an AMICE plugin library compatible with your current Android NDK (Clang).

## Build Script

Here's an example script for building AMICE:

```bash
# r522817 corresponds to llvm18-1
export LLVM_SYS_181_PREFIX=/home/fuqiuluo/Downloads/linux-x86-refs_heads_main-clang-r522817

#cargo clean
export CXX="/home/fuqiuluo/Downloads/linux-x86-refs_heads_main-clang-r522817/bin/clang++"
export CXXFLAGS="-stdlib=libc++ -I/home/fuqiuluo/Downloads/linux-x86-refs_heads_main-clang-r522817/include/c++/v1"
export LDFLAGS="-stdlib=libc++ -L/home/fuqiuluo/Downloads/linux-x86-refs_heads_main-clang-r522817/lib"

cargo b --release --no-default-features --features llvm18-1,android-ndk
```

## Compilation Usage

### Using Complete Clang

After a successful build, you can directly use the complete Clang to compile source files:

```bash
# Set library dependency path because the plugin depends on libLLVM.so
export LD_LIBRARY_PATH="/home/fuqiuluo/Downloads/linux-x86-refs_heads_main-clang-r522817/lib"
/home/fuqiuluo/Downloads/linux-x86-refs_heads_main-clang-r522817/bin/clang \
  -fpass-plugin=../target/release/libamice.so \
  test1.c -o test1
```

### Using Android NDK Toolchain

You can also use the Clang from the Android NDK toolchain directly:

```bash
/home/fuqiuluo/android-kernel/android-ndk-r25c/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android21-clang \
  -fpass-plugin=../target/release/libamice.so \
  test1.c -o test_ndk
```

## Resources

**Android NDK r25c compatible version**: [https://github.com/fuqiuluo/amice/releases/tag/android-ndk-r25c](https://github.com/fuqiuluo/amice/releases/tag/android-ndk-r25c)

Compatible build command:
```bash
cargo b --release --no-default-features --features llvm18-1
```

## Common Issues and Solutions

### Undefined Symbol Error

If you encounter the following error when loading:
```
error: unable to load plugin './target/release/libamice.so': './target/release/libamice.so: undefined symbol: _ZTIN4llvm10CallbackVHE'
```

Try adding the new feature:
```bash
cargo b --release --no-default-features --features llvm18-1,android-ndk
```

### Version Mismatch Error

If you see an error like:
```
error: unable to load plugin '/home/who/amice/target/release/libamice.so': 'Could not load library '/home/who/amice/target/release/libamice.so': /usr/lib/llvm-18/lib/libLLVM-18.so: version `LLVM_18' not found (required by /home/who/amice/target/release/libamice.so)'
```

**Solution steps:**

1. Check Clang version:
   ```bash
   $ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin/clang --version
   ```

2. Set the complete Clang library path:
   ```bash
   export LD_LIBRARY_PATH=/path/to/unstripped-clang/lib:$LD_LIBRARY_PATH
   ```

## Build System Integration

### CMake Integration

Use the plugin in CMake:

```cmake
target_compile_options(${PROJECT_NAME} PRIVATE
    -fpass-plugin=${PLUGIN_PATH}
    -Xclang -load -Xclang ${PLUGIN_PATH}
)
```

### Gradle Integration

Use with Gradle:

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

## Debugging and Logging

After a successful build, you can enable logging for detailed information:

```bash
export RUST_LOG=info
```

## More Information

For more details, please refer to: [https://github.com/fuqiuluo/amice/wiki](https://github.com/fuqiuluo/amice/wiki)

> Thanks to [Android1500](https://github.com/Android1500) for the discussion and research at https://github.com/fuqiuluo/amice/discussions/55.
