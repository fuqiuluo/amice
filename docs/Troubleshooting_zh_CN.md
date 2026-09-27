# 故障排除指南

[English](Troubleshooting_en_US.md) | 简体中文

## 先判断卡在哪一步

| 现象 | 先检查 |
| --- | --- |
| 构建找不到 LLVM / C++ 编译失败 | feature、prefix 与 `llvm-config --version` 是否对应；是否安装开发头文件和 C++ 工具链 |
| 找不到 `libLLVM`、DLL 或 `undefined symbol` | 插件宿主系统/架构、LLVM 版本及动态库是否来自兼容工具链 |
| 编译成功但没有混淆效果 | 开关、Pass 允许列表、函数注解、重建缓存及 Pass 跳过日志 |
| 修改配置后效果不变 | 是否重新编译了目标源码，而非复用旧对象/二进制 |

### 插件加载失败

在 [LLVM 环境配置](LLVMSetup_zh_CN.md) 中选定工具链后，使用其绝对路径运行 `clang --version`、`opt --version`、`llvm-config --version`。prefix 只帮助构建插件，不会把任意 clang 自动切换为正确版本。

Linux 上若报缺少 `libLLVM.so`，确认匹配的动态库确实存在，然后在运行编译器的终端设置：

```bash
export LD_LIBRARY_PATH="$AMICE_LLVM/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
```

这里的 `AMICE_LLVM` 是所选 LLVM 安装目录。macOS 如缺少 `libLLVM.dylib`，检查安装路径与插件依赖；可对同一工具链设置 `DYLD_LIBRARY_PATH`。路径设置不能解决 ABI 不兼容。

Windows 按 [opt 加载示例](LLVMSetup_zh_CN.md#windows) 检查 `LLVM-C.dll`、`opt.exe` 和 `opt.lib`；`win-link-opt` 与 `win-link-lld` 的加载宿主不同。当前终端使用 `$env:LLVM_SYS_*_PREFIX` 设置环境，执行 `setx` 后立即构建仍会读取旧环境。

Android 的 host LLVM 缺失、NDK revision 不匹配及 macOS 签名问题，见 [NDK 常见错误](AndroidNDKSupport_zh_CN.md#常见错误)。

### 编译成功但没有效果

1. 用 [快速上手](QuickStart_zh_CN.md) 的最小示例单独开启字符串加密，比较运行输出并检查最终二进制中的 marker。插件加载成功本身不能证明变换已发生。
2. 查看 `AMICE_CONFIG_PATH`、`AMICE_PASS_ORDER` 和其他 `AMICE_*`。显式 Pass 列表会排除未列出的功能；函数注解可以覆盖全局开关。名称和拼写以 [环境变量](EnvConfig_zh_CN.md) 为准。
3. 配置文件路径不存在或 TOML/YAML/JSON 解析失败时，当前实现会回退默认配置。先使用绝对路径及 [完整配置示例](PassOrder_zh_CN.md)，必要时用单个环境变量排除文件配置问题。
4. 在编译器启动前设置日志，重新编译：

```bash
export RUST_LOG=amice=debug
```

```powershell
$env:RUST_LOG = 'amice=debug'
```

`amice plugin initializing` 表示加载入口被调用；`(PassName) pass done` 表示有变换。VMP 的 `skip function` 日志会说明跳过原因。某个 Pass 的成功日志不代表所有函数都经过处理。

5. 确认构建系统确实重新编译了源码。Rust 的 `AMICE_*` 和插件更新不会自动触发 Cargo 重建，使用新的专用 `--target-dir`；详见 [Rust 接入](RustUsage_zh_CN.md)。

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
