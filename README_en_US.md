# Amice

English | [简体中文](README.md)

Amice is an LLVM Pass plugin project built on **llvm-plugin-rs**, injectable into the compilation pipeline via `clang -fpass-plugin`.

---

## Quick Start

1. **Build the Plugin**

   ```bash
   export LLVM_SYS_211_PREFIX=/usr/lib64/llvm21  # Fedora; adjust for your distro
   # export RUST_LOG=debug  # uncomment for debug logs
   cargo build --release
   # Output: target/release/libamice.so
   ```

2. **Compile with Pass Injection**

   ```bash
   clang -fpass-plugin=libamice.so your_source.c -o your_source
   ```
---

## Supported Obfuscations

| Obfuscation                           | C/C++ | Rust | ObjC |
|:--------------------------------------|:-----:|:----:|:----:|
| String Encryption                     |   ✅   |  ✅   |  ⏳   |
| Indirect Call Obfuscation             |   ✅   |  ✅   |  ❌   |
| Indirect Branch Obfuscation           |   ✅   |  ✅   |  ❌   |
| Split Basic Block                     |   ✅   |  ✅   |  ❌   |
| Switch Lowering                       |   ✅   |  ✅   |  ❌   |
| VM Flatten                            |   ✅   |  ✅   |  ❌   |
| Control Flow Flattening               |   ✅   |  ✅   |  ❌   |
| MBA Arithmetic Obfuscation            |   ✅   |  ✅   |  ❌   |
| Bogus Control Flow                    |   ✅   |  ✅   |  ❌   |
| Function Wrapper                      |   ✅   |  ✅   |  ❌   |
| Clone Function (Const Specialization) |   ✅   |  ✅   |  ❌   |
| Alias Access Obfuscation              |   ✅   |  ✅   |  ❌   |
| Custom Calling Convention             |   ⏳   |  ⏳   |  ❌   |
| Delayed Offset Loading (AMA)          |   ✅   |  ⏳   |  ❌   |
| Anti-Class Export                     |   ❌   |  ❌   |  ⏳   |
| Parameter Aggregation (PAO)           |   ✅   |  ⏳   |  ❌   |
| Instruction Virtualization            |   ⏳   |  ⏳   |  ❌   |
| Function Outlining (BB2FUNC)          |   ✅   |  ⏳   |  ❌   |

> Legend:
> - ✅ Supported
> - ⏳ In Progress / Planned / Untested
> - ❌ Not Planned

## Runtime Environment Variables

For detailed documentation, please refer to:
<https://github.com/fuqiuluo/amice/blob/master/docs>

---

## Build Guide

### 1. Linux

> The default feature is `llvm21-1`. LLVM 21 development package is required. You can also disable the default feature and use another LLVM version via `--no-default-features --features llvm<version>` (supported range: llvm11-0 ~ llvm21-1).

- Fedora / RHEL

  ```bash
  sudo dnf install llvm llvm-devel
  export LLVM_SYS_211_PREFIX=/usr/lib64/llvm21
  cargo build --release
  ```

- Debian / Ubuntu (via [apt.llvm.org](https://apt.llvm.org/))

  ```bash
  sudo apt install llvm-21 llvm-21-dev
  export LLVM_SYS_211_PREFIX=/usr/lib/llvm-21
  cargo build --release
  ```

- Custom path

  ```bash
  export LLVM_SYS_211_PREFIX=/path/to/llvm21
  cargo build --release
  ```

#### [Troubleshooting](docs/Troubleshooting_en_US.md) | [LLVM Setup Guide](docs/LLVMSetup_en_US.md)

### 2. macOS

```bash
brew install llvm@21
export LLVM_SYS_211_PREFIX=$(brew --prefix llvm@21)
cargo build --release
```

### 3. Windows

Official pre-built LLVM does not support dynamic plugins. Compile yourself or use community builds:
<https://github.com/jamesmth/llvm-project/releases>

```powershell
setx LLVM_SYS_211_PREFIX "C:\llvm21"
cargo build --release
```

### 4. Android NDK

Android's bundled clang supports dynamic Pass loading but lacks `opt`. Use the "unstripped clang" approach, refer to:
[Ylarod: NDK Load LLVM Pass](https://xtuly.cn/article/ndk-load-llvm-pass-plugin)

```bash
# Example based on r522817 (NDK 25c)
export CXX="/path/to/unstripped-clang/bin/clang++"
export CXXFLAGS="-stdlib=libc++ -I/path/to/unstripped-clang/include/c++/v1"
export LDFLAGS="-stdlib=libc++ -L/path/to/unstripped-clang/lib"

# llvm-plugin-rs 18.1, corresponding to NDK clang 18.0
export LLVM_SYS_181_PREFIX=/path/to/unstripped-clang

# cargo build --release
# ndk 25c is llvm-18-1
cargo b --release --no-default-features --features llvm-18-1

# If libLLVM.so is not found, specify LD_LIBRARY_PATH
export LD_LIBRARY_PATH=/path/to/unstripped-clang/lib

/path/to/ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/clang \
  -fpass-plugin=../target/release/libamice.so \
  -Xclang -load -Xclang ../target/release/libamice.so \
  luo.c -o luo
```

Download: [android-ndk-r25c Linux X64](https://github.com/fuqiuluo/amice/releases/tag/android-ndk-r25c)

---

## TODO

- [ ] Inline mode support
- [ ] More Pass examples
- [ ] CI / CD

---

## Acknowledgements

- LLVM Project – <https://llvm.org/>
- llvm-plugin-rs
    - <https://github.com/jamesmth/llvm-plugin-rs/tree/feat/llvm-20>
    - <https://github.com/stevefan1999-personal/llvm-plugin-rs>
- Obfuscator-LLVM - <https://github.com/obfuscator-llvm/obfuscator>
- SsagePass – <https://github.com/SsageParuders/SsagePass>
- Polaris-Obfuscator – <https://github.com/za233/Polaris-Obfuscator>
- YANSOllvm - <https://github.com/emc2314/YANSOllvm>
- Related Articles
    - MBA – <https://plzin.github.io/posts/mba>
    - LLVM PassManager Changes and Dynamic Registration – <https://bbs.kanxue.com/thread-272801.htm>

---

> © 2025-2026 Fuqiuluo & Contributors.
> Licensed under this repository's LICENSE.
