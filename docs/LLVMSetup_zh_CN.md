# LLVM 环境配置指南

## 环境变量命名规则

环境变量格式为 `LLVM_SYS_<主版本号><次版本号>_PREFIX`：

| LLVM 版本 | 环境变量              |
|---------|-------------------|
| 21.1    | `LLVM_SYS_211_PREFIX` |
| 20.1    | `LLVM_SYS_201_PREFIX` |
| 19.1    | `LLVM_SYS_191_PREFIX` |
| 18.1    | `LLVM_SYS_181_PREFIX` |
| 17.0    | `LLVM_SYS_170_PREFIX` |
| 16.0    | `LLVM_SYS_160_PREFIX` |
| 15.0    | `LLVM_SYS_150_PREFIX` |
| 14.0    | `LLVM_SYS_140_PREFIX` |

---

## Linux (Fedora/RHEL/CentOS)

### 安装

```bash
# 搜索可用版本
dnf search llvm

# 安装最新稳定版
sudo dnf install llvm llvm-devel clang clang-devel

# 或安装指定版本（例如 LLVM 18）
sudo dnf install llvm18 llvm18-devel clang18 clang18-devel
```

### 验证安装

```bash
which llvm-config
llvm-config --version
llvm-config --prefix
```

### 设置环境变量

```bash
export LLVM_SYS_181_PREFIX=$(llvm-config --prefix)
```

---

## Linux (Ubuntu/Debian)

### 安装

```bash
sudo apt update
sudo apt install llvm llvm-dev clang libclang-dev

# 或安装指定版本
sudo apt install llvm-18 llvm-18-dev clang-18 libclang-18-dev
```

### 验证安装

```bash
llvm-config --version
llvm-config --prefix
```

### 设置环境变量

```bash
export LLVM_SYS_181_PREFIX=/usr/lib/llvm-18
```

---

## macOS (Homebrew)

### 安装

```bash
# 安装最新版
brew install llvm

# 或安装指定版本
brew install llvm@18
```

### 设置环境变量

```bash
# 最新版本
export LLVM_SYS_181_PREFIX=$(brew --prefix llvm)

# 指定版本
export LLVM_SYS_181_PREFIX=$(brew --prefix llvm@18)
```

### 添加到 PATH（可选）

```bash
export PATH="$(brew --prefix llvm)/bin:$PATH"
```

---

## Windows

### 方式一：预编译 LLVM（推荐）

从以下地址下载预编译的 LLVM：https://github.com/jamesmth/llvm-project/releases

### 方式二：官方 LLVM

从官网下载：https://releases.llvm.org/

### 设置环境变量

CMD：

```cmd
setx LLVM_SYS_181_PREFIX "C:\Program Files\LLVM"
```

PowerShell：

```powershell
[Environment]::SetEnvironmentVariable("LLVM_SYS_181_PREFIX", "C:\Program Files\LLVM", "User")
```

### 构建参数

Windows 构建需要额外的 feature 参数：

```bash
cargo build --features win-link-opt
# 或
cargo build --features win-link-lld
```
