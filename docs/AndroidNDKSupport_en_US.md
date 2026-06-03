# Android NDK Usage

## Short Version

Do not load a random `libamice.so` with the clang from a plain Android NDK. AMICE is an LLVM Pass plugin loaded by the host clang process, and the plugin needs a matching host `libLLVM.so`/`libLLVM.dylib`. The official NDK usually does not ship that shared library, so the common failure is:

```text
error: unable to load plugin 'libamice.so': libLLVM.so: cannot open shared object file
```

Use the Android NDK bundle from AMICE releases:

- `amice-android-ndk-r29-linux-x86_64.tar.gz`
- `amice-android-ndk-r29-darwin-x86_64.tar.gz`

The bundle contains:

- Official Android NDK: `android-ndk-r29/`
- AMICE plugin: `amice/lib/libamice.so` or `amice/lib/libamice.dylib`
- Matching LLVM runtime libraries: `amice/llvm-lib/`
- Wrapper compilers that set the runtime library path and add `-fpass-plugin`: `amice/bin/`

The macOS NDK host tag is still named `darwin-x86_64`. That is Android NDK's historical path name; modern NDKs use that directory for Apple Silicon too.

## Quick Test

Linux:

```bash
tar xf amice-android-ndk-r29-linux-x86_64.tar.gz
cd amice-android-ndk-r29-linux-x86_64
```

macOS:

```bash
tar xf amice-android-ndk-r29-darwin-x86_64.tar.gz
cd amice-android-ndk-r29-darwin-x86_64
```

Build an arm64 Android executable:

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

Default API levels:

- `aarch64-linux-android-*`: API 23
- `x86_64-linux-android-*`: API 23
- `armv7a-linux-androideabi-*`: API 19
- `i686-linux-android-*`: API 19

Override the API level:

```bash
AMICE_ANDROID_API=21 ./amice/bin/aarch64-linux-android-clang hello.c -o hello
```

For C++:

```bash
./amice/bin/aarch64-linux-android-clang++ hello.cc -o hello
```

`amice/bin/amice-clang` and `amice/bin/amice-clang++` are generic wrappers. They set the runtime library path and inject the plugin, but they do not choose an Android ABI. Use them with an explicit target such as `--target=aarch64-linux-android23`. If you want a default Android target, use an ABI-named wrapper such as `aarch64-linux-android-clang`.

All passes are disabled by default. Enable them with environment variables or a config file. See [Runtime Environment Variables](EnvConfig_en_US.md).

## CMake/Gradle

First, make the bundled LLVM runtime visible to the build process:

```bash
cd /path/to/amice-android-ndk-r29-linux-x86_64
source ./amice/env.sh
```

On macOS, use the `amice-android-ndk-r29-darwin-x86_64` directory and the `.dylib` plugin extension instead.

In CMake, add the plugin path to compile options:

```cmake
set(AMICE_PLUGIN "/absolute/path/to/amice/lib/libamice.so")

target_compile_options(your_target PRIVATE
    "-fpass-plugin=${AMICE_PLUGIN}"
)
```

Gradle + externalNativeBuild usually passes the plugin path into CMake:

```gradle
externalNativeBuild {
    cmake {
        arguments "-DAMICE_PLUGIN=/absolute/path/to/amice/lib/libamice.so"
    }
}
```

Then use it from `CMakeLists.txt`:

```cmake
if(DEFINED AMICE_PLUGIN)
    target_compile_options(your_target PRIVATE
        "-fpass-plugin=${AMICE_PLUGIN}"
    )
endif()
```

If Gradle still uses another NDK, point it at the bundled one:

```properties
ndk.dir=/absolute/path/to/amice-android-ndk-r29-linux-x86_64/android-ndk-r29
```

If your Android Gradle Plugin requires `android.ndkVersion`, remember that it uses the numeric NDK revision, not the `r29` release name. In that setup, install the matching NDK into the Android SDK and use `source ./amice/env.sh` only for `libLLVM` and `libamice`.

## Repository Helper Script

From the source repository, the existing helper can use an unpacked release bundle:

```bash
AMICE_ANDROID_BUNDLE=/absolute/path/to/amice-android-ndk-r29-linux-x86_64 \
  ./scripts/build_android_arm64.sh hello.c hello
```

## Build from Source

If you do not use the release bundle, you must prepare the complete Android clang matching your NDK and build AMICE with it.

CI currently covers this mapping:

| NDK | LLVM feature | `LLVM_SYS_*_PREFIX` | Android clang revision |
| --- | --- | --- | --- |
| r25c | `llvm14-0` | `LLVM_SYS_140_PREFIX` | `r450784d1` |
| r26d | `llvm17-0` | `LLVM_SYS_170_PREFIX` | `r487747e` |
| r27d | `llvm18-1` | `LLVM_SYS_181_PREFIX` | `r522817d` |
| r28c | `llvm19-1` | `LLVM_SYS_191_PREFIX` | `r530567e` |
| r29 | `llvm21-1` | `LLVM_SYS_211_PREFIX` | `r563880c` |

Example for r29:

```bash
export LLVM_SYS_211_PREFIX=/path/to/unstripped-android-clang
export CXX="/path/to/unstripped-android-clang/bin/clang++"
export CXXFLAGS="-stdlib=libc++ -I/path/to/unstripped-android-clang/include/c++/v1"
export LDFLAGS="-stdlib=libc++ -L/path/to/unstripped-android-clang/lib"

cargo build --release --no-default-features --features llvm21-1,android-ndk
```

Feature names are `llvm21-1`, `llvm18-1`, etc. There is no extra dash after `llvm`.

## Common Errors

### `libLLVM.so` Not Found

Cause: a plain NDK does not include the host LLVM shared library required by the AMICE plugin.

Fix:

```bash
source /path/to/amice-android-ndk-r29-linux-x86_64/amice/env.sh
```

or set it manually:

```bash
export LD_LIBRARY_PATH=/path/to/amice/llvm-lib:$LD_LIBRARY_PATH
```

macOS:

```bash
export DYLD_LIBRARY_PATH=/path/to/amice/llvm-lib:$DYLD_LIBRARY_PATH
```

### Team ID Mismatch on macOS

If you see an error like:

```text
code signature ... not valid for use in process: mapping process and mapped file (non-platform) have different Team IDs
```

Cause: the official NDK `clang` may be signed with hardened runtime, which can reject pass plugins signed by another Team ID or with an ad-hoc signature. New AMICE NDK bundles ad-hoc sign the copied NDK clang driver during packaging to avoid this.

If you are using an older bundle or a plain NDK, re-sign the local extracted copy:

```bash
codesign --force --sign - android-ndk-r29/toolchains/llvm/prebuilt/darwin-x86_64/bin/clang-21
```

This modifies the extracted local NDK copy. Do not do this to a shared system NDK unless you explicitly accept that change.

### `undefined symbol`

The usual cause is a mismatch between `libamice`, `libLLVM`, and the NDK clang process.

Fix: use the bundle for the same NDK release, or rebuild using the version table above.

### The Pass Does Nothing

AMICE passes are disabled by default. Start with an easy-to-check pass:

```bash
AMICE_STRING_ENCRYPTION=true ./amice/bin/aarch64-linux-android-clang hello.c -o hello
strings -a hello | grep AMICE_NDK_STRING_TEST_20260603
```

If the string is still printed, string encryption did not run. If `grep` prints nothing, the marker was hidden. More switches are documented in [Runtime Environment Variables](EnvConfig_en_US.md).

## References

- Android NDK host tags and command-line usage: <https://developer.android.com/ndk/guides/other_build_systems>
- Older manual setup article: <https://xtuly.cn/article/ndk-load-llvm-pass-plugin>
