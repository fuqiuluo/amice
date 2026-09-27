# Rust 接入

[English](RustUsage_en_US.md) | 简体中文

## 适用范围与工具链

本文的直接加载示例适用于 Linux/macOS，使用 nightly 的 `-Zllvm-plugins`。它属于 Rust 的不稳定编译器接口，不能把插件当成普通 crate 加进应用的 `Cargo.toml`。接口说明见 [Rust Unstable Book](https://doc.rust-lang.org/unstable-book/compiler-flags/llvm-plugins.html)。

先选择一个已安装的 nightly 工具链，核对它自带的 LLVM，再按 [LLVM 环境配置](LLVMSetup_zh_CN.md) 构建对应插件。`rustc -vV` 中的 LLVM 版本与 `llvm-config --version` 的主版本必须相同；这只是必要条件，Rust 的 LLVM fork、符号和动态库也需要兼容。不要通过重命名插件或修改 prefix 变量绕过不匹配。

```bash
rustup toolchain list
AMICE_RUST_TOOLCHAIN=nightly
# 若使用固定版本，将上行改为已安装的 nightly-YYYY-MM-DD
rustc +"$AMICE_RUST_TOOLCHAIN" -vV
```

如果尚未安装合适的 nightly，用 `rustup toolchain install <工具链名称>` 安装，再核对版本。最新 nightly 可能使用尚未支持的 LLVM，不能保证与默认 LLVM 21 匹配。

下面假设已经按构建指南准备 LLVM 和插件。设置真实的绝对路径；macOS 把 `.so` 改为 `.dylib`。由于 `-Zllvm-plugins` 的列表以空格分隔，示例使用不含空格的插件路径。

```bash
AMICE_LLVM=/usr/lib/llvm-21
AMICE_PLUGIN=/absolute/path/to/amice/target/release/libamice.so
test -f "$AMICE_PLUGIN" || exit 1
AMICE_RUST_LLVM=$(rustc +"$AMICE_RUST_TOOLCHAIN" -vV | awk '/^LLVM version:/ {split($3,v,"."); print v[1]}')
AMICE_PLUGIN_LLVM=$("$AMICE_LLVM/bin/llvm-config" --version | cut -d. -f1)
if [ -z "$AMICE_RUST_LLVM" ] || [ "$AMICE_RUST_LLVM" != "$AMICE_PLUGIN_LLVM" ]; then
  echo "ERROR: rustc and plugin LLVM versions do not match"
  exit 1
fi
```

## 最小 Cargo 示例

在仓库之外的新临时目录创建演示项目，避免修改 AMICE 的 workspace。环境变量、插件更新不会自动成为 Cargo 的重建依据，因此两次编译使用独立产物目录。

```bash
AMICE_DEMO=$(mktemp -d)
cargo +"$AMICE_RUST_TOOLCHAIN" new --bin --name amice-hello "$AMICE_DEMO/hello" || exit 1
cd "$AMICE_DEMO/hello" || exit 1
cat > src/main.rs <<'RS'
fn main() {
    println!("AMICE_RUST_STRING_TEST");
}
RS

cargo +"$AMICE_RUST_TOOLCHAIN" build --release --target-dir target/plain || exit 1
RUST_LOG=amice=info CARGO_INCREMENTAL=0 \
AMICE_STRING_ENCRYPTION=true AMICE_STRING_ONLY_DOT_STRING=false \
cargo +"$AMICE_RUST_TOOLCHAIN" rustc --release --bin amice-hello \
  --target-dir target/with-amice -- \
  "-Zllvm-plugins=$AMICE_PLUGIN" -Cpasses= -Ccodegen-units=1 --emit=llvm-ir,link || exit 1

./target/plain/release/amice-hello > target/plain.txt || exit 1
./target/with-amice/release/amice-hello > target/with-amice.txt || exit 1
cmp target/plain.txt target/with-amice.txt || exit 1
cat target/with-amice.txt
"$AMICE_LLVM/bin/llvm-strings" target/plain/release/amice-hello > target/plain.strings || exit 1
"$AMICE_LLVM/bin/llvm-strings" target/with-amice/release/amice-hello > target/with-amice.strings || exit 1
grep -Fq AMICE_RUST_STRING_TEST target/plain.strings || exit 1
if grep -Fq AMICE_RUST_STRING_TEST target/with-amice.strings; then
  echo "ERROR: string encryption did not hide the marker"
  exit 1
fi
echo "PASS: same output, plaintext marker hidden"
```

成功输出 `AMICE_RUST_STRING_TEST` 和 `PASS: same output, plaintext marker hidden`。只检查最终二进制，源码和基线产物仍保留原始字符串。

## 接入现有项目

- `cargo rustc --bin <名称> -- ...` 将这些参数传给所选目标；不会自动混淆所有依赖或标准库。库目标改用 `--lib`，但依赖 crate 仍需要单独规划。
- 先只开一个 Pass，比较现有测试、输出、体积和运行耗时，再增加选项。Rust 字符串加密通常需要 `AMICE_STRING_ONLY_DOT_STRING=false`；不要把所有全局字节数组都当成可以安全处理的字符串。
- 修改 `AMICE_*`、配置文件或插件后，用新的 `--target-dir` 验证，或清理这个专用产物目录后重编译，避免复用旧结果。
- 调试时使用 `RUST_LOG=amice=debug`。VMP 等 Pass 可以跳过不支持的函数；编译成功不等于目标函数已混淆。
- C/C++ 的 `__attribute__((annotate(...)))` 不能直接粘贴到 Rust。本文使用全局配置，不承诺 Rust 存在对应的稳定注解语法。

## Windows 与测试工具的边界

`win-link-opt` / `win-link-lld` DLL 绑定对应的 LLVM 宿主，不能直接把它传给 rustc 的 `-Zllvm-plugins`。Windows C/C++ 首次验证请按 [原生 opt 示例](LLVMSetup_zh_CN.md#windows) 操作。Windows Rust 的直接加载不作为本文已支持的入门路径。

仓库的 Rust 集成测试在主版本不匹配时可以采用“输出 IR → opt → llc → 链接”的专用流程；其中包含特定 IR 兼容处理，不能据此推断任意 Rust 项目都能跨 LLVM 版本使用插件。测试工具里的 `AMICE_RUST_TOOLCHAIN` 只选择测试使用的 Rust 工具链，不会改变插件的 LLVM feature。
