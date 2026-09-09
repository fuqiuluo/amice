# Android NDK 使用说明

## 先说结论

不要直接拿普通 Android NDK 的 clang 加载随便构建出来的 `libamice.so`。AMICE 是 clang 在宿主机上加载的 LLVM Pass 插件，插件本身需要匹配的 host `libLLVM.so`/`libLLVM.dylib`。官方 NDK 通常不带这个动态库，所以用户最常见的失败是：

```text
error: unable to load plugin 'libamice.so': libLLVM.so: cannot open shared object file
```

推荐方式是下载 AMICE release 里的 Android NDK bundle：

- `amice-android-ndk-r30-linux-x86_64.tar.gz`
- `amice-android-ndk-r30-darwin-x86_64.tar.gz`

这个包里面已经放好了：

- 官方 Android NDK：`android-ndk-r30/`
- AMICE 插件：`amice/lib/libamice.so` 或 `amice/lib/libamice.dylib`
- 与 NDK clang 匹配的 LLVM 动态库：`amice/llvm-lib/`
- 自动设置动态库路径并追加 `-fpass-plugin` 的 wrapper：`amice/bin/`

macOS 的 NDK host tag 仍叫 `darwin-x86_64`，这是 Android NDK 的历史路径名；现代 NDK 里这个目录也用于 Apple Silicon。

## 直接测试

Linux：

```bash
tar xf amice-android-ndk-r30-linux-x86_64.tar.gz
cd amice-android-ndk-r30-linux-x86_64
```

macOS：

```bash
tar xf amice-android-ndk-r30-darwin-x86_64.tar.gz
cd amice-android-ndk-r30-darwin-x86_64
```

编译一个 arm64 Android 可执行文件：

```bash
cat > hello.c <<'SRC'
extern int puts(const char *);
int main(void) { return puts("AMICE_NDK_STRING_TEST_20260603") < 0; }
SRC

AMICE_STRING_ENCRYPTION=true ./amice/bin/aarch64-linux-android-clang hello.c -o hello
file hello
if strings -a hello | grep -q 'AMICE_NDK_STRING_TEST_20260603'; then
  echo "ERROR: string encryption did not hide the marker"
  exit 1
fi
```

默认 API level：

- `aarch64-linux-android-*`：API 23
- `x86_64-linux-android-*`：API 23
- `armv7a-linux-androideabi-*`：API 21
- `i686-linux-android-*`：API 21

需要指定 API level：

```bash
AMICE_ANDROID_API=21 ./amice/bin/aarch64-linux-android-clang hello.c -o hello
```

需要 C++：

```bash
./amice/bin/aarch64-linux-android-clang++ hello.cc -o hello
```

`amice/bin/amice-clang` 和 `amice/bin/amice-clang++` 是通用 wrapper，只负责设置动态库路径并注入插件；它们不会自动选择 Android ABI。使用它们时要显式传 `--target=aarch64-linux-android23` 这类 target。想要默认 Android target，就用 `aarch64-linux-android-clang` 这类带 ABI 名的 wrapper。

所有 Pass 默认关闭。通过环境变量或配置文件开启，完整列表见 [运行时环境变量](EnvConfig_zh_CN.md)。

## 在 CMake/Gradle 里用

先让构建进程能找到 bundle 里的 LLVM 动态库：

```bash
cd /path/to/amice-android-ndk-r30-linux-x86_64
source ./amice/env.sh
```

macOS 下把上面的目录换成 `amice-android-ndk-r30-darwin-x86_64`，插件扩展名也使用 `.dylib`。

CMake 里只需要把插件路径加到编译参数：

```cmake
set(AMICE_PLUGIN "/absolute/path/to/amice/lib/libamice.so")

target_compile_options(your_target PRIVATE
    "-fpass-plugin=${AMICE_PLUGIN}"
)
```

Gradle + externalNativeBuild 通常把插件路径作为 CMake 参数传进去：

```gradle
externalNativeBuild {
    cmake {
        arguments "-DAMICE_PLUGIN=/absolute/path/to/amice/lib/libamice.so"
    }
}
```

然后在 `CMakeLists.txt` 中使用：

```cmake
if(DEFINED AMICE_PLUGIN)
    target_compile_options(your_target PRIVATE
        "-fpass-plugin=${AMICE_PLUGIN}"
    )
endif()
```

如果 Gradle 仍然使用系统里另一个 NDK，确保它使用的是 bundle 里的 NDK：

```properties
ndk.dir=/absolute/path/to/amice-android-ndk-r30-linux-x86_64/android-ndk-r30
```

如果你的 Android Gradle Plugin 版本要求使用 `android.ndkVersion`，注意它要写数字版本号，不是 `r30` 这种 release 名。此时建议把对应 NDK 安装进 Android SDK，再用 `source ./amice/env.sh` 提供 `libLLVM` 和 `libamice`。

## 仓库脚本

在源码仓库里，可以直接让现有脚本使用解压后的 release bundle：

```bash
AMICE_ANDROID_BUNDLE=/absolute/path/to/amice-android-ndk-r30-linux-x86_64 \
  ./scripts/build_android_arm64.sh hello.c hello
```

## 从源码构建 Android NDK 版本

如果你不使用 release bundle，就必须自己准备和 NDK 对应的完整 Android clang，并用它构建 AMICE。

当前 CI 覆盖的对应关系：

| NDK | LLVM feature | `LLVM_SYS_*_PREFIX` | Android clang revision |
| --- | --- | --- | --- |
| r27d | `llvm18-1` | `LLVM_SYS_181_PREFIX` | `r522817d` |
| r28c | `llvm19-1` | `LLVM_SYS_191_PREFIX` | `r530567e` |
| r29 | `llvm21-1` | `LLVM_SYS_211_PREFIX` | `r563880c` |
| r30 | `llvm21-1` | `LLVM_SYS_211_PREFIX` | `r574158c` |

r30 正式版的数字版本号为 `30.0.16248370`，其 Android Clang 仍报告 `21.0.0`，所以使用 `llvm21-1`。r29 和 r30 的 Android clang revision 不同，插件与宿主 LLVM 库不能混用。r25c/r26d 的旧构建配置目前已在 CI 中注释。

Linux 上的 r30 还有一个宿主库兼容性要求：官方 Clang 和 AOSP `libLLVM.so` 的四个全局容器会因 ELF 符号抢占而被重复销毁，导致退出时崩溃。AMICE bundle 会自动为复制的共享库修正这四个符号的可见性；手动准备工具链时，先在仓库根目录运行：

```bash
python3 scripts/prepare_android_llvm.py /path/to/unstripped-android-clang/lib/libLLVM.so
```

该操作仅适用于 Linux r30 的 `r574158c` 共享库，可重复执行。它不修改官方 NDK Clang，也不改变 LLVM 分析键或函数符号。macOS 不需要此 ELF 修正。

以 r30 为例：

```bash
export LLVM_SYS_211_PREFIX=/path/to/unstripped-android-clang
export CXX="/path/to/unstripped-android-clang/bin/clang++"
export CXXFLAGS="-stdlib=libc++ -I/path/to/unstripped-android-clang/include/c++/v1"
export LDFLAGS="-stdlib=libc++ -L/path/to/unstripped-android-clang/lib"

cargo build --release --no-default-features --features llvm21-1,android-ndk
```

注意 feature 名是 `llvm21-1`、`llvm18-1` 这种格式，中间没有额外的横线。

## 常见错误

### 找不到 `libLLVM.so`

原因：普通 NDK 里没有 AMICE 插件需要的 host LLVM 动态库。

解决：

```bash
source /path/to/amice-android-ndk-r30-linux-x86_64/amice/env.sh
```

或者手动设置：

```bash
export LD_LIBRARY_PATH=/path/to/amice/llvm-lib:$LD_LIBRARY_PATH
```

macOS：

```bash
export DYLD_LIBRARY_PATH=/path/to/amice/llvm-lib:$DYLD_LIBRARY_PATH
```

### macOS 上 Team ID 不一致

如果看到类似错误：

```text
code signature ... not valid for use in process: mapping process and mapped file (non-platform) have different Team IDs
```

原因：官方 NDK 的 `clang` 可能带 hardened runtime 签名，默认不允许加载另一个 Team ID 或 ad-hoc 签名的 pass 插件。新的 AMICE NDK bundle 会在打包时对复制出来的 NDK clang driver 做 ad-hoc 重签，避免这个问题。

如果你用的是旧 bundle 或普通 NDK，可以在解压目录里手动重签本地副本：

```bash
codesign --force --sign - android-ndk-r30/toolchains/llvm/prebuilt/darwin-x86_64/bin/clang-21
```

这会修改解压后的本地 NDK 副本；不要对系统里共享的 NDK 做这个操作，除非你明确接受该改动。

### `undefined symbol`

原因通常是 `libamice`、`libLLVM` 和 NDK clang 不是同一套 Android clang 构建出来的。

解决：换成匹配同一个 NDK release 的 bundle，或者按上面的版本表重新构建。

### Pass 没有效果

AMICE 的 Pass 默认关闭。先用环境变量开一个最容易观察的 Pass：

```bash
AMICE_STRING_ENCRYPTION=true ./amice/bin/aarch64-linux-android-clang hello.c -o hello
strings -a hello | grep AMICE_NDK_STRING_TEST_20260603
```

如果上面的字符串还能搜到，说明字符串加密没有生效；如果 `grep` 没有输出，说明这个 marker 已经被隐藏。更多开关见 [运行时环境变量](EnvConfig_zh_CN.md)。

## 回归测试

NDK CI 在 Linux/macOS 上为 r29、r30 打包并解压到带空格的路径，验证四种 ABI（arm64-v8a、armeabi-v7a、x86_64、x86）的 C/C++ 编译、共享库链接、字符串加密、O0/O2、ThinLTO 和完整 LTO。r30 还会在 Android x86_64 模拟器上比较混淆前后的输出，检查分支算术和 C++ 嵌套异常清理。

从仓库根目录运行：

```bash
python3 -m unittest discover -s scripts/tests -v
python3 scripts/test_android_ndk_bundle.py \
  --bundle /path/to/amice-android-ndk-r30-linux-x86_64 \
  --output-dir target/android-ndk-tests

# 可选：在已连接的设备或模拟器上运行兼容 ABI 的测试产物
python3 scripts/test_android_ndk_bundle.py --runtime-only \
  --output-dir target/android-ndk-tests --adb-serial SERIAL
```

## 参考

- Android NDK host tag 和命令行用法：<https://developer.android.google.cn/ndk/guides/other_build_systems?hl=zh-cn>
- 旧的手动方案说明：<https://xtuly.cn/article/ndk-load-llvm-pass-plugin>
