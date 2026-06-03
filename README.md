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

> 当前默认 feature 为 `llvm21-1`，需要 LLVM 21 开发包。
> 也可通过 `--no-default-features --features llvm<版本>` 切换其他 LLVM 版本（支持范围：llvm11-0 ~ llvm21-1），对应调整 `LLVM_SYS_<大版本号>_PREFIX`。
>
> `amice` 构建时会自动在 `$PATH` 中搜索 `llvm-config`、`llvm-config-<N>` 等，若系统包管理器安装的 `llvm-config` 已在 PATH 中，则无需手动设置 `LLVM_SYS_*_PREFIX`。

- Fedora / RHEL

  ```bash
  sudo dnf install llvm llvm-devel
  cargo build --release
  ```

- Debian / Ubuntu（LLVM 官方源）

  ```bash
  # 安装 LLVM 21（https://apt.llvm.org/）
  sudo apt install llvm-21 llvm-21-dev clang-21
  # Ubuntu 的 llvm-config-21 不一定在 PATH，可显式指定
  # export LLVM_SYS_211_PREFIX=/usr/lib/llvm-21
  cargo build --release
  ```

- 非默认版本（以 LLVM 18 为例）

  ```bash
  # feature 名 llvm18-1，对应 LLVM_SYS_181_PREFIX
  export LLVM_SYS_181_PREFIX=/path/to/llvm18
  cargo build --release --no-default-features --features llvm18-1
  ```

- 自定义路径（通用）

  ```bash
  # LLVM_SYS_<大版本号>_PREFIX 指向 llvm-config 所在目录的上级
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
setx LLVM_SYS_211_PREFIX "C:\llvm21"
cargo build --release
```

### 4. Android NDK

普通 Android NDK 通常没有 `libLLVM.so`/`libLLVM.dylib`，直接加载 `libamice` 很容易失败。优先使用 release 里的 Android NDK bundle：

```bash
tar xf amice-android-ndk-r29-linux-x86_64.tar.gz
cd amice-android-ndk-r29-linux-x86_64

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

详细说明见 [Android NDK 使用说明](docs/AndroidNDKSupport_zh_CN.md)。

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
