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
