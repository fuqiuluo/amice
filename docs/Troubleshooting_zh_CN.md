# 故障排除指南

## LLVM 未找到

**错误信息：**
```
error: No suitable version of LLVM was found system-wide or pointed
       to by LLVM_SYS_<VERSION>_PREFIX.

       Refer to the llvm-sys documentation for more information.

       llvm-sys: https://crates.io/crates/llvm-sys
```

**原因：** LLVM 未安装或构建工具无法定位到 LLVM。

**解决方案：** 参见 [LLVM 环境配置指南](LLVMSetup_zh_CN.md)

---

## libffi 未找到

**错误信息：** 链接器报告缺少 `-lffi`

### Linux (Fedora/RHEL/CentOS)

```bash
sudo dnf install libffi-devel
```

### Linux (Ubuntu/Debian)

```bash
sudo apt install libffi-dev
```

### macOS

```bash
brew install libffi

# 如果仍有问题，设置 PKG_CONFIG_PATH
export PKG_CONFIG_PATH="$(brew --prefix libffi)/lib/pkgconfig:$PKG_CONFIG_PATH"
```

### Windows

libffi 应该包含在 LLVM 安装中。如果仍有问题，请确保安装了包含所有组件的完整 LLVM 包。

---

## Rust 相关问题

### Clone Function 混淆导致安全检查失效

**问题描述：** 启用 `AMICE_CLONE_FUNCTION=true` 后，某些 Rust 安全检查可能会失效或产生误报。

**原因：** Clone Function（常参特化克隆）混淆会为带有常量参数的函数调用创建特化版本，并修改调用点。这可能会干扰 Rust 编译器的某些安全分析，因为：

1. 函数签名被修改（常量参数被移除）
2. 原始调用被替换为特化函数调用
3. 参数属性（如 `noundef`、`nonnull` 等）在特化过程中可能被移除

**影响范围：**
- 边界检查优化可能受影响
- 某些 `debug_assert!` 可能被优化掉
- LLVM 的安全相关优化 Pass 可能无法正确分析特化后的代码

**建议：**
- 在安全关键代码中谨慎使用此混淆
- 使用函数注解 `-clone_function` 排除特定函数
- 在生产环境部署前进行充分的测试

### Rust Debug 构建无法应用混淆

**问题描述：** 使用 debug 构建时，混淆 Pass 报告找不到函数或调用点。

**原因：** Rust 默认使用增量编译和多代码生成单元，导致 LLVM 插件只能看到部分函数。

**解决方案：** 在 `Cargo.toml` 中配置：

```toml
[profile.dev]
codegen-units = 1
incremental = false
```
