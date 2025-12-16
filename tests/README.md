# Amice 集成测试

本目录包含 Amice LLVM 混淆插件的集成测试，支持 C/C++ 和 Rust 两种语言。

## 目录结构

```
tests/
├── common/                     # 公共测试工具模块
│   └── mod.rs                  # Language enum, CompileBuilder, RustCompileBuilder 等
│
├── c/                          # C/C++ 测试
│   ├── fixtures/               # C/C++ 测试源文件
│   │   ├── string_encryption/  # 字符串加密测试源码
│   │   ├── indirect_branch/    # 间接分支测试源码
│   │   ├── indirect_call/      # 间接调用测试源码
│   │   ├── control_flow/       # 控制流混淆测试源码
│   │   ├── shuffle_blocks/     # 基本块重排测试源码
│   │   ├── function_wrapper/   # 函数包装测试源码
│   │   ├── mba/                # MBA 混淆测试源码
│   │   └── integration/        # 综合测试源码 (MD5 等)
│   └── .gitignore
│
├── rust/                       # Rust 测试
│   └── string_encryption/      # Rust 字符串加密测试项目
│       ├── Cargo.toml
│       └── src/
│           └── main.rs
│
├── scripts/                    # 测试运行脚本
│   ├── run_tests.sh            # Linux/macOS
│   └── run_tests.ps1           # Windows
│
├── string_encryption.rs        # C/C++ 字符串加密测试
├── indirect_branch.rs          # 间接分支测试
├── indirect_call.rs            # 间接调用测试
├── control_flow.rs             # 控制流混淆测试 (BCF/Flatten/VM)
├── shuffle_blocks.rs           # 基本块重排测试
├── function_wrapper.rs         # 函数包装测试
├── mba.rs                      # MBA 混淆测试
├── integration.rs              # 综合测试
└── rust_string_encryption.rs   # Rust 字符串加密测试
```

## 快速开始

### 前置条件

1. 安装 Rust 工具链
2. 安装 LLVM 并设置环境变量：
   ```bash
   # Linux (以 LLVM 18 为例)
   export LLVM_SYS_181_PREFIX=/usr/lib/llvm-18

   # macOS (Homebrew)
   export LLVM_SYS_181_PREFIX=$(brew --prefix llvm@18)

   # Windows
   setx LLVM_SYS_181_PREFIX "C:\llvm"
   ```
3. 确保 `clang` 在 PATH 中

### 运行测试

**方式一：使用测试脚本（推荐）**

```bash
# Linux/macOS
./tests/scripts/run_tests.sh

# Windows PowerShell
.\tests\scripts\run_tests.ps1
```

**方式二：使用 Cargo**

```bash
# 运行所有集成测试（需要 --release 模式）
cargo test --release --no-default-features --features llvm18-1

# 仅运行单元测试（不包含集成测试）
cargo test --no-default-features --features llvm18-1 --lib
```

**注意：** 集成测试必须使用 `--release` 模式运行，因为测试依赖于 release 构建的 FFI 库。

## 测试脚本选项

```bash
# 显示帮助
./tests/scripts/run_tests.sh --help

# 强制重新构建
./tests/scripts/run_tests.sh --build

# 显示详细输出
./tests/scripts/run_tests.sh --verbose

# 列出所有可用测试
./tests/scripts/run_tests.sh --list

# 运行匹配名称的测试
./tests/scripts/run_tests.sh string      # 运行字符串相关测试
./tests/scripts/run_tests.sh md5         # 运行 MD5 测试
./tests/scripts/run_tests.sh -v bcf      # 详细模式运行 BCF 测试
```

## 测试模块说明

| 模块    | 文件                     | 测试内容                           |
|-------|------------------------|--------------------------------|
| 字符串加密 | `string_encryption.rs` | XOR/SIMD XOR 算法、懒加载/全局解密、栈/堆分配 |
| 间接分支  | `indirect_branch.rs`   | 基本间接分支、链式虚假块                   |
| 间接调用  | `indirect_call.rs`     | 函数指针间接化                        |
| 控制流   | `control_flow.rs`      | 虚假控制流(BCF)、控制流扁平化、VM扁平化        |
| 基本块重排 | `shuffle_blocks.rs`    | 随机/反转/旋转重排                     |
| 函数包装  | `function_wrapper.rs`  | 函数包装器、常量参数特化                   |
| MBA   | `mba.rs`               | 混合布尔算术混淆                       |
| 综合测试  | `integration.rs`       | MD5 等实际算法验证                    |

## 编写新测试

### 1. 添加测试源文件

将 C/C++ 测试文件放入对应的 `fixtures/` 子目录：

```bash
tests/fixtures/your_module/your_test.c
```

### 2. 使用公共工具

```rust
mod common;

use common::{ensure_plugin_built, fixture_path, CompileBuilder, ObfuscationConfig};

#[test]
fn test_your_feature() {
    // 确保插件已构建
    ensure_plugin_built();

    // 配置混淆选项
    let config = ObfuscationConfig {
        your_option: Some(true),
        ..ObfuscationConfig::disabled()  // 禁用其他所有选项
    };

    // 编译测试文件
    let result = CompileBuilder::new(
        fixture_path("your_module", "your_test.c"),
        "output_binary_name",
    )
    .config(config)
    .optimization("O2")  // 可选
    .compile();

    // 验证编译成功
    result.assert_success();

    // 运行并验证输出
    let run = result.run();
    run.assert_success();

    let lines = run.stdout_lines();
    assert_eq!(lines[0], "Expected output");
}
```

### 3. ObfuscationConfig 可用选项

```rust
ObfuscationConfig {
    // 字符串加密
    string_encryption: Option<bool>,
    string_algorithm: Option<String>,      // "xor" | "simd_xor"
    string_decrypt_timing: Option<String>, // "lazy" | "global"
    string_stack_alloc: Option<bool>,
    string_inline_decrypt_fn: Option<bool>,
    string_max_encryption_count: Option<u32>,

    // 间接分支
    indirect_branch: Option<bool>,
    indirect_branch_flags: Option<String>, // "chained_dummy_block"

    // 间接调用
    indirect_call: Option<bool>,

    // 控制流
    flatten: Option<bool>,
    bogus_control_flow: Option<bool>,
    vm_flatten: Option<bool>,

    // 基本块
    shuffle_blocks: Option<bool>,
    shuffle_blocks_flags: Option<String>,  // "random" | "reverse" | "rotate"
    split_basic_block: Option<bool>,

    // 其他
    mba: Option<bool>,
    function_wrapper: Option<bool>,
}
```

### 4. CompileBuilder 方法

```rust
CompileBuilder::new(source_path, output_name)
    .config(obfuscation_config)  // 设置混淆配置
    .optimization("O2")          // 设置优化级别
    .std("c++17")                // 设置 C/C++ 标准
    .arg("-Wall")                // 添加额外编译参数
    .without_plugin()            // 不使用混淆插件（用于基准对比）
    .compile()                   // 执行编译
```

## 测试输出目录

编译后的测试二进制文件保存在：

```
target/test-outputs/
```

## 常见问题

### Q: 测试失败提示找不到插件

确保先构建了 release 版本：

```bash
cargo build --release --no-default-features --features llvm18-1
```

或使用测试脚本的 `--build` 选项。

### Q: Windows 上链接失败

Windows 需要额外的链接特性：

```bash
cargo build --release --no-default-features --features llvm18-1,win-link-lld
```

### Q: 如何只运行特定测试

```bash
# 运行单个测试文件
cargo test --release --test string_encryption

# 运行匹配名称的测试
cargo test --release test_md5

# 运行某个模块的所有测试
cargo test --release --test integration
```

### Q: 测试输出乱码（中文显示问题）

确保终端支持 UTF-8 编码。Windows 下可以执行：

```powershell
chcp 65001
```

## 贡献指南

1. 新增混淆功能时，请同步添加对应的测试
2. 测试应验证混淆后程序的功能正确性
3. 使用 `ObfuscationConfig::disabled()` 作为基础，只启用要测试的选项
4. 复杂测试应与非混淆版本的输出进行对比
