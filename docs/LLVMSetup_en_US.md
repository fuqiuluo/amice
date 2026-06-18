# LLVM Setup Guide

## Environment Variable Naming

The environment variable format is `LLVM_SYS_<MAJOR><MINOR>_PREFIX`:

| LLVM Version | Environment Variable  |
|--------------|-----------------------|
| 22.1         | `LLVM_SYS_221_PREFIX` |
| 21.1         | `LLVM_SYS_211_PREFIX` |
| 20.1         | `LLVM_SYS_201_PREFIX` |
| 19.1         | `LLVM_SYS_191_PREFIX` |
| 18.1         | `LLVM_SYS_181_PREFIX` |
| 17.0         | `LLVM_SYS_170_PREFIX` |
| 16.0         | `LLVM_SYS_160_PREFIX` |
| 15.0         | `LLVM_SYS_150_PREFIX` |
| 14.0         | `LLVM_SYS_140_PREFIX` |

---

## Linux (Fedora/RHEL/CentOS)

### Install

```bash
# Search for available versions
dnf search llvm

# Install latest stable
sudo dnf install llvm llvm-devel clang clang-devel

# Or install specific version (e.g., LLVM 22)
sudo dnf install llvm22 llvm22-devel clang22 clang22-devel
```

### Verify

```bash
which llvm-config
llvm-config --version
llvm-config --prefix
```

### Set Environment Variable

```bash
export LLVM_SYS_221_PREFIX=$(llvm-config --prefix)
```

---

## Linux (Ubuntu/Debian)

### Install

```bash
sudo apt update
sudo apt install llvm llvm-dev clang libclang-dev

# Or install specific version
sudo apt install llvm-22 llvm-22-dev clang-22 libclang-22-dev
```

### Verify

```bash
llvm-config --version
llvm-config --prefix
```

### Set Environment Variable

```bash
export LLVM_SYS_221_PREFIX=/usr/lib/llvm-22
```

---

## macOS (Homebrew)

### Install

```bash
# Install latest
brew install llvm

# Or install specific version
brew install llvm@22
```

### Set Environment Variable

```bash
# Latest version
export LLVM_SYS_221_PREFIX=$(brew --prefix llvm)

# Specific version
export LLVM_SYS_221_PREFIX=$(brew --prefix llvm@22)
```

### Add to PATH (optional)

```bash
export PATH="$(brew --prefix llvm)/bin:$PATH"
```

---

## Windows

### Option 1: Pre-built LLVM (Recommended)

Download pre-built LLVM from: https://github.com/jamesmth/llvm-project/releases

### Option 2: Official LLVM

Download from: https://releases.llvm.org/

### Set Environment Variable

```cmd
setx LLVM_SYS_221_PREFIX "C:\Program Files\LLVM"
```

Or in PowerShell:

```powershell
[Environment]::SetEnvironmentVariable("LLVM_SYS_221_PREFIX", "C:\Program Files\LLVM", "User")
```

### Build Flags

Windows builds require additional flags:

```bash
cargo build --features win-link-opt
# or
cargo build --features win-link-lld
```
