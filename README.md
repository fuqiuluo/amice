# Amice

[English](README_en_US.md) | 简体中文

Amice 是一个基于 Rust、`llvm-plugin` 和 `inkwell` 构建的 LLVM Pass 插件。它以 clang `-fpass-plugin` 动态插件形式加载到编译流程中，用于在编译期对 C/C++、Rust 等 LLVM IR 进行混淆变换。

当前仓库是 Cargo workspace，主插件 crate 位于 `crates/amice`，构建产物是 `target/release/libamice.so`、`target/release/libamice.dylib` 或 `target/release/amice.dll`。

---

## 快速上手

### 1. 构建插件

默认 LLVM feature 是 `llvm21-1`，需要 LLVM 21 开发包或可用的 `llvm-config`。

```bash
# macOS
brew install llvm@21
export LLVM_SYS_211_PREFIX=$(brew --prefix llvm@21)

# Linux 示例：如果 llvm-config/llvm-config-21 已在 PATH，可以不设置 PREFIX
# export LLVM_SYS_211_PREFIX=/usr/lib/llvm-21

cargo build --release
```

### 2. 注入到 clang

```bash
cat > /tmp/amice_hello.c <<'SRC'
extern int puts(const char *);
int main(void) { return puts("AMICE_STRING_TEST") < 0; }
SRC

AMICE_STRING_ENCRYPTION=true \
clang -fpass-plugin="$(pwd)/target/release/libamice.so" /tmp/amice_hello.c -o /tmp/amice_hello
```

macOS 下插件后缀是 `.dylib`，请把路径替换为 `target/release/libamice.dylib`。

---

## 支持的混淆

| Pass | 环境变量开关 | C/C++ | Rust | ObjC | 说明 |
|:---|:---|:---:|:---:|:---:|:---|
| String Encryption | `AMICE_STRING_ENCRYPTION` | ✅ | ✅ | ⏳ | 字符串加密，支持 `xor` / `simd_xor`、lazy/global 解密、栈/堆解密配置 |
| Indirect Call | `AMICE_INDIRECT_CALL` | ✅ | ✅ | ❌ | 将直接调用改写为函数表/索引形式的间接调用 |
| Indirect Branch | `AMICE_INDIRECT_BRANCH` | ✅ | ✅ | ❌ | 将分支改写为 `indirectbr`，支持 dummy block、表重排、索引加密等 flags |
| Split Basic Block | `AMICE_SPLIT_BASIC_BLOCK` | ✅ | ✅ | ❌ | 按配置切割基本块 |
| Lower Switch | `AMICE_LOWER_SWITCH` | ✅ | ✅ | ❌ | 降级 LLVM `switch` 指令 |
| VM Flatten | `AMICE_VM_FLATTEN` | ✅ | ✅ | ❌ | VM 风格控制流扁平化 |
| Flatten | `AMICE_FLATTEN` | ✅ | ✅ | ❌ | 控制流平坦化，支持 `basic` / `dominator` 模式 |
| MBA | `AMICE_MBA` | ✅ | ✅ | ❌ | 混合布尔算术表达式重写 |
| Bogus Control Flow | `AMICE_BOGUS_CONTROL_FLOW` | ✅ | ✅ | ❌ | 插入虚假控制流，支持 basic / polaris-primes 模式 |
| Function Wrapper | `AMICE_FUNCTION_WRAPPER` | ✅ | ✅ | ❌ | 生成包装函数并替换调用点 |
| Clone Function | `AMICE_CLONE_FUNCTION` | ✅ | ✅ | ❌ | 常量参数特化克隆 |
| Alias Access | `AMICE_ALIAS_ACCESS` | ✅ | ✅ | ❌ | 基于指针链的别名访问混淆 |
| Custom Calling Conv | `AMICE_CUSTOM_CALLING_CONV` | ⏳ | ⏳ | ❌ | 自定义调用约定，通常通过函数注解按函数启用 |
| Delay Offset Loading | `AMICE_DELAY_OFFSET_LOADING` | ✅ | ⏳ | ❌ | GEP 偏移延迟加载/可选 XOR 保护 |
| Param Aggregate | `AMICE_PARAM_AGGREGATE` | ✅ | ⏳ | ❌ | 参数结构化聚合混淆 |
| Basic Block Outlining | `AMICE_BASIC_BLOCK_OUTLINING` | ✅ | ⏳ | ❌ | 将基础块提取为独立子函数，亦称 BB2Func |
| Shuffle Blocks | `AMICE_SHUFFLE_BLOCKS` | ✅ | ⏳ | ❌ | 基本块重排 |

> 说明：
> - ✅ 已支持
> - ⏳ 进行中 / 计划中 / 未测试
> - ❌ 暂未规划
>
> Rust 字符串加密通常需要设置 `AMICE_STRING_ONLY_DOT_STRING=false`（兼容旧名 `AMICE_STRING_ONLY_LLVM_STRING=false`），详见 [运行时环境变量](docs/EnvConfig_zh_CN.md)。

历史 README 中的规划项目前没有独立环境变量：反 Class 导出（C/C++ ❌、Rust ❌、ObjC ⏳）和指令虚拟化（C/C++ ⏳、Rust ⏳、ObjC ❌）。

完整配置项请看 [运行时环境变量](docs/EnvConfig_zh_CN.md)，按函数启用/禁用请看 [函数注解](docs/FunctionAnnotations_zh_CN.md)。

---

## 配置方式

Amice 的配置优先级如下：

1. 若设置 `AMICE_CONFIG_PATH`，先读取 TOML/YAML/JSON 配置文件。
2. 未设置配置文件时使用默认配置。
3. 最后叠加 `AMICE_*` 环境变量，环境变量优先级最高。

示例：

```bash
cat > /tmp/amice.toml <<'TOML'
[string_encryption]
enable = true
algorithm = "xor"

[flatten]
enable = true
mode = "basic"
TOML

AMICE_CONFIG_PATH=/tmp/amice.toml \
clang -fpass-plugin="$(pwd)/target/release/libamice.so" input.c -o output
```

Pass 运行顺序可通过 `AMICE_PASS_ORDER` 或配置文件里的 `pass_order.order` 控制；显式顺序只运行列表中的 pass。详情见 [Pass 运行顺序](docs/PassOrder_zh_CN.md)。

---

## 构建指南

### Linux

```bash
# Fedora / RHEL
sudo dnf install llvm llvm-devel clang
cargo build --release

# Debian / Ubuntu，建议使用 https://apt.llvm.org/ 安装 LLVM 21
sudo apt install llvm-21 llvm-21-dev clang-21
# export LLVM_SYS_211_PREFIX=/usr/lib/llvm-21
cargo build --release
```

切换非默认 LLVM 版本时，Cargo feature 和 `LLVM_SYS_*_PREFIX` 需要匹配：

```bash
# LLVM 18 示例
export LLVM_SYS_181_PREFIX=/usr/lib/llvm-18
cargo build --release --no-default-features --features llvm18-1
```

支持的 LLVM feature：`llvm11-0` 到 `llvm21-1`。

### macOS

```bash
brew install llvm@21
export LLVM_SYS_211_PREFIX=$(brew --prefix llvm@21)
cargo build --release
```

### Windows

LLVM 官方预编译包通常无法直接支持动态 pass 插件。建议自行构建 LLVM，或使用支持插件加载的社区构建。

```powershell
setx LLVM_SYS_211_PREFIX "C:\llvm21"
cargo build --release --features win-link-lld
# 或使用 opt 链接：cargo build --release --features win-link-opt
```

如果关闭默认 feature，需要显式带上 LLVM feature，例如：

```powershell
cargo build --release --no-default-features --features llvm21-1,win-link-lld
```

### Android NDK

普通 Android NDK 通常缺少加载插件所需的 host `libLLVM.so` / `libLLVM.dylib`。优先使用 release 里的 Android NDK bundle：

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

## 测试

集成测试会调用 clang 加载 release 版插件，因此请使用 `--release`。测试脚本会自动探测 `llvm-config`，也支持 `LLVM_SYS_*_PREFIX`。

```bash
# 构建并运行全部测试
./crates/amice/tests/scripts/run_tests.sh --build

# 只运行名称匹配的测试
./crates/amice/tests/scripts/run_tests.sh -v string

# 直接使用 cargo
cargo test --release --no-default-features --features llvm21-1
cargo test --release --no-default-features --features llvm21-1 --test string_encryption
cargo test --release --no-default-features --features llvm21-1 test_md5
```

更多测试说明见 [crates/amice/tests/README.md](crates/amice/tests/README.md)。

---

## 项目结构

| 路径 | 说明 |
|:---|:---|
| `crates/amice` | 主 clang pass 插件，注册和实现各类混淆 pass |
| `crates/amice-llvm` | LLVM/inkwell 扩展层和 C++ FFI glue |
| `crates/amice-macro` | `#[amice(...)]` pass 注册宏和配置宏 |
| `crates/amice-plugin` | pass manager / pass builder 适配层 |
| `crates/amice-plugin-macros` | plugin 适配层宏 |
| `crates/amice-build-support` | 构建期 LLVM 探测辅助 |
| `docs` | 构建、环境变量、函数注解、Android NDK 和排障文档 |
| `scripts` | Android NDK bundle 打包和辅助构建脚本 |

---

## 文档入口

| 主题 | 文档 |
|:---|:---|
| LLVM 环境配置 | [docs/LLVMSetup_zh_CN.md](docs/LLVMSetup_zh_CN.md) |
| 运行时环境变量 | [docs/EnvConfig_zh_CN.md](docs/EnvConfig_zh_CN.md) |
| 函数注解 | [docs/FunctionAnnotations_zh_CN.md](docs/FunctionAnnotations_zh_CN.md) |
| Pass 运行顺序 | [docs/PassOrder_zh_CN.md](docs/PassOrder_zh_CN.md) |
| Android NDK | [docs/AndroidNDKSupport_zh_CN.md](docs/AndroidNDKSupport_zh_CN.md) |
| 故障排除 | [docs/Troubleshooting_zh_CN.md](docs/Troubleshooting_zh_CN.md) |

---

## 鸣谢

- LLVM Project: <https://llvm.org/>
- llvm-plugin-rs
  - <https://github.com/jamesmth/llvm-plugin-rs/tree/feat/llvm-20>
  - <https://github.com/stevefan1999-personal/llvm-plugin-rs>
- Obfuscator-LLVM: <https://github.com/obfuscator-llvm/obfuscator>
- SsagePass: <https://github.com/SsageParuders/SsagePass>
- Polaris-Obfuscator: <https://github.com/za233/Polaris-Obfuscator>
- YANSOllvm: <https://github.com/emc2314/YANSOllvm>
- MBA: <https://plzin.github.io/posts/mba>
- LLVM PassManager 变更及动态注册: <https://bbs.kanxue.com/thread-272801.htm>

---

> © 2025-2026 Fuqiuluo & Contributors.<br>
> 使用遵循本仓库 LICENSE。
