# Troubleshooting Guide

## LLVM Not Found

**Error message:**
```
error: No suitable version of LLVM was found system-wide or pointed
       to by LLVM_SYS_<VERSION>_PREFIX.

       Refer to the llvm-sys documentation for more information.

       llvm-sys: https://crates.io/crates/llvm-sys
```

**Cause:** LLVM is not installed or the build tools cannot locate it.

**Solution:** See [LLVM Setup Guide](LLVMSetup.md)

---

## libffi Not Found

**Error message:** Linker errors about missing `-lffi`

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

# If still having issues, set PKG_CONFIG_PATH
export PKG_CONFIG_PATH="$(brew --prefix libffi)/lib/pkgconfig:$PKG_CONFIG_PATH"
```

### Windows

libffi should be included with the LLVM installation. If issues persist, ensure you've installed the complete LLVM package with all components.
