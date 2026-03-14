# Amice

[English](README_en_US.md) | 简体中文

Amice 是一个基于 **llvm-plugin-rs** 构建的 LLVM Pass 插件项目，可通过 `clang -fpass-plugin` 方式注入到编译流程中。

---

## 快速上手

1. **构建插件**

   ```bash
   export LLVM_SYS_211_PREFIX=/usr/lib64/llvm21  # Fedora，其他发行版按实际路径调整
   # export RUST_LOG=debug  # 需要调试日志时取消注释
   cargo build --release
   # 生成的动态库位于 target/release/libamice.so
   ```

2. **编译并注入 Pass**

   ```bash
   clang -fpass-plugin=libamice.so your_source.c -o your_source
   ```

---

## 支持的混淆

| 混淆             | C/C++ | Rust | ObjC |
|:---------------|:-----:|:----:|:----:|
| 字符串加密          |   ✅   |  ✅   |  ⏳   |
| 间接调用混淆         |   ✅   |  ✅   |  ❌   |
| 间接跳转混淆         |   ✅   |  ✅   |  ❌   |
| 切割基本块          |   ✅   |  ✅   |  ❌   |
| switch 降级      |   ✅   |  ✅   |  ❌   |
| 扁平化控制流 (VM)    |   ✅   |  ✅   |  ❌   |
| 控制流平坦化         |   ✅   |  ✅   |  ❌   |
| MBA 算术混淆       |   ✅   |  ✅   |  ❌   |
| 虚假控制流混淆        |   ✅   |  ✅   |  ❌   |
| 函数包装           |   ✅   |  ✅   |  ❌   |
| 常参特化克隆混淆       |   ✅   |  ✅   |  ❌   |
| 别名访问混淆         |   ✅   |  ✅   |  ❌   |
| 自定义调用约定        |   ⏳   |  ⏳   |  ❌   |
| 延时偏移加载 (AMA)   |   ✅   |  ⏳   |  ❌   |
| 反Class导出       |   ❌   |  ❌   |  ⏳   |
| 参数结构化混淆 (PAO)  |   ✅   |  ⏳   |  ❌   |
| 指令虚拟化          |   ⏳   |  ⏳   |  ❌   |
| 函数分片 (BB2FUNC) |   ✅   |  ⏳   |  ❌   |

> 说明：
> - ✅ 已支持
> - ⏳ 进行中 / 计划中 / 未测试
> - ❌ 暂未规划

## 运行时环境变量

详细说明请参阅：  
<https://github.com/fuqiuluo/amice/blob/master/docs>

---

## 构建指南

### 1. Linux

> 当前默认 feature 为 `llvm21-1`，需要 LLVM 21 开发包。也可通过 `--no-default-features --features llvm<版本>` 使用其他 LLVM 版本（支持范围：llvm11-0 ~ llvm21-1）。

- Fedora / RHEL

  ```bash
  sudo dnf install llvm llvm-devel
  export LLVM_SYS_211_PREFIX=/usr/lib64/llvm21
  cargo build --release
  ```

- Debian / Ubuntu（LLVM 官方源）

  ```bash
  # 安装 LLVM 21（https://apt.llvm.org/）
  sudo apt install llvm-21 llvm-21-dev
  export LLVM_SYS_211_PREFIX=/usr/lib/llvm-21
  cargo build --release
  ```

- 自定义路径

  ```bash
  export LLVM_SYS_211_PREFIX=/path/to/llvm21
  cargo build --release
  ```

#### [问题排查](docs/Troubleshooting_zh_CN.md) | [LLVM 安装指南](docs/LLVMSetup_zh_CN.md)

### 2. macOS

```bash
brew install llvm@21
export LLVM_SYS_211_PREFIX=$(brew --prefix llvm@21)
cargo build --release
```

### 3. Windows

官方预编译的 LLVM 无法启用动态插件，需**自行编译**或使用社区版本：
<https://github.com/jamesmth/llvm-project/releases>

```powershell
setx LLVM_SYS_211_PREFIX “C:\llvm21”
cargo build --release
```

### 4. Android NDK

Android 自带 clang 支持动态加载 Pass，但缺少 `opt`。可采用“未精简版 clang”方案，参考：  
[Ylarod：NDK 加载 LLVM Pass](https://xtuly.cn/article/ndk-load-llvm-pass-plugin)

```bash
# 以下示例基于 r522817 (NDK 25c)
export CXX="/path/to/unstripped-clang/bin/clang++"
export CXXFLAGS="-stdlib=libc++ -I/path/to/unstripped-clang/include/c++/v1"
export LDFLAGS="-stdlib=libc++ -L/path/to/unstripped-clang/lib"

# llvm-plugin-rs 18.1，对应 NDK clang 18.0
export LLVM_SYS_181_PREFIX=/path/to/unstripped-clang

# cargo build --release 
# ndk 25c is llvm-18-1
cargo b --release --no-default-features --features llvm-18-1

# 如遇找不到 libLLVM.so，可指定 LD_LIBRARY_PATH
export LD_LIBRARY_PATH=/path/to/unstripped-clang/lib

/path/to/ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/clang \
  -fpass-plugin=../target/release/libamice.so \
  -Xclang -load -Xclang ../target/release/libamice.so \
  luo.c -o luo
```

Download: [android-ndk-r25c Linux X64](https://github.com/fuqiuluo/amice/releases/tag/android-ndk-r25c)

---

## TODO

- [ ] 内联模式（Inline）支持
- [ ] 更多 Pass 示例
- [ ] CI / CD

---

## 鸣谢

- LLVM Project – <https://llvm.org/>
- llvm-plugin-rs
    - <https://github.com/jamesmth/llvm-plugin-rs/tree/feat/llvm-20>
    - <https://github.com/stevefan1999-personal/llvm-plugin-rs>
- Obfuscator-LLVM - <https://github.com/obfuscator-llvm/obfuscator>
- SsagePass – <https://github.com/SsageParuders/SsagePass>
- Polaris-Obfuscator – <https://github.com/za233/Polaris-Obfuscator>
- YANSOllvm - <https://github.com/emc2314/YANSOllvm>
- 相关文章
    - MBA – <https://plzin.github.io/posts/mba>
    - LLVM PassManager 变更及动态注册 – <https://bbs.kanxue.com/thread-272801.htm>

---

> © 2025-2026 Fuqiuluo & Contributors.  
> 使用遵循本仓库 LICENSE。
