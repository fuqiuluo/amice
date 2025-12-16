# Amice

Amice 是一个基于 **llvm-plugin-rs** 构建的 LLVM Pass 插件项目，可通过 `clang -fpass-plugin` 方式注入到编译流程中。

---

## 快速上手

1. **构建插件**

   ```bash
   # 如需调试日志请解除注释
   # export RUST_LOG=debug
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
| 字符串加密          |   ✅   |  ⏳   |  ⏳   |
| 间接调用混淆         |   ✅   |  ⏳   |  ❌   |
| 间接跳转混淆         |   ✅   |  ⏳   |  ❌   |
| 切割基本块          |   ✅   |  ⏳   |  ❌   |
| switch 降级      |   ✅   |  ⏳   |  ❌   |
| 扁平化控制流 (VM)    |   ✅   |  ⏳   |  ❌   |
| 控制流平坦化         |   ✅   |  ⏳   |  ❌   |
| MBA 算术混淆       |   ✅   |  ⏳   |  ❌   |
| 虚假控制流混淆        |   ✅   |  ⏳   |  ❌   |
| 函数包装           |   ✅   |  ⏳   |  ❌   |
| 常参特化克隆混淆       |   ✅   |  ⏳   |  ❌   |
| 别名访问混淆         |   ✅   |  ⏳   |  ❌   |
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

### 1. Linux / macOS

> 要求 LLVM 工具链支持 **动态链接** LLVM 库。  
> 推荐使用系统包管理器安装。

- Debian / Ubuntu

  ```bash
  sudo apt install llvm-14
  ```

- Homebrew

  ```bash
  brew install llvm@14
  ```

如使用自编译或自解压版本，请手动配置路径：

```bash
# 假设 LLVM 安装在 ~/llvm
export PATH="$PATH:$HOME/llvm/bin"
# 或者
export LLVM_SYS_140_PREFIX="$HOME/llvm"
```

#### [问题排查](docs/Troubleshooting.md) | [LLVM 安装指南](docs/LLVMSetup.md)

### 2. Windows

官方预编译的 LLVM 无法启用动态插件，需**自行编译**或使用社区版本：  
<https://github.com/jamesmth/llvm-project/releases>

```powershell
# 假设 LLVM 安装在 C:\llvm
setx PATH "%PATH%;C:\llvm\bin"
rem 或者
setx LLVM_SYS_140_PREFIX "C:\llvm"
```

### 3. Android NDK

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

> © 2025 Fuqiuluo & Contributors.  
> 使用遵循本仓库 LICENSE。
