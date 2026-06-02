#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'EOF'
Usage:
  package_android_ndk_bundle.sh \
    --ndk-home <official-ndk-root> \
    --llvm-home <matching-unstripped-clang-root> \
    --plugin <target/release/libamice.so|libamice.dylib> \
    --ndk-release <r29> \
    --llvm-feature <llvm21-1> \
    --clang-revision <r563880c> \
    --host-tag <linux-x86_64|darwin-x86_64> \
    --out-dir <dist>

The package contains:
  android-ndk-<release>/     official Android NDK
  amice/lib/                 libamice plugin
  amice/llvm-lib/            matching libLLVM/libc++ runtime for plugin loading
  amice/bin/                 wrapper clang scripts that inject -fpass-plugin
EOF
}

NDK_HOME=""
LLVM_HOME=""
PLUGIN=""
NDK_RELEASE=""
LLVM_FEATURE=""
CLANG_REVISION=""
HOST_TAG=""
OUT_DIR="dist"
PACKAGE_NAME=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --ndk-home)
            NDK_HOME="${2:?missing --ndk-home value}"
            shift 2
            ;;
        --llvm-home)
            LLVM_HOME="${2:?missing --llvm-home value}"
            shift 2
            ;;
        --plugin)
            PLUGIN="${2:?missing --plugin value}"
            shift 2
            ;;
        --ndk-release)
            NDK_RELEASE="${2:?missing --ndk-release value}"
            shift 2
            ;;
        --llvm-feature)
            LLVM_FEATURE="${2:?missing --llvm-feature value}"
            shift 2
            ;;
        --clang-revision)
            CLANG_REVISION="${2:?missing --clang-revision value}"
            shift 2
            ;;
        --host-tag)
            HOST_TAG="${2:?missing --host-tag value}"
            shift 2
            ;;
        --out-dir)
            OUT_DIR="${2:?missing --out-dir value}"
            shift 2
            ;;
        --package-name)
            PACKAGE_NAME="${2:?missing --package-name value}"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "ERROR: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [[ -z "$NDK_HOME" || -z "$LLVM_HOME" || -z "$PLUGIN" || -z "$NDK_RELEASE" || -z "$LLVM_FEATURE" || -z "$CLANG_REVISION" ]]; then
    echo "ERROR: missing required arguments" >&2
    usage >&2
    exit 2
fi

if [[ -z "$HOST_TAG" ]]; then
    case "$(uname -s)" in
        Linux) HOST_TAG="linux-x86_64" ;;
        Darwin) HOST_TAG="darwin-x86_64" ;;
        *)
            echo "ERROR: unsupported host OS for Android NDK bundle: $(uname -s)" >&2
            exit 1
            ;;
    esac
fi

NDK_HOME="$(cd "$NDK_HOME" && pwd -P)"
LLVM_HOME="$(cd "$LLVM_HOME" && pwd -P)"
PLUGIN="$(cd "$(dirname "$PLUGIN")" && pwd)/$(basename "$PLUGIN")"
OUT_DIR="$(mkdir -p "$OUT_DIR" && cd "$OUT_DIR" && pwd)"

if [[ ! -d "$NDK_HOME/toolchains/llvm/prebuilt/$HOST_TAG" ]]; then
    echo "ERROR: NDK toolchain not found: $NDK_HOME/toolchains/llvm/prebuilt/$HOST_TAG" >&2
    exit 1
fi

if [[ ! -x "$NDK_HOME/toolchains/llvm/prebuilt/$HOST_TAG/bin/clang" ]]; then
    echo "ERROR: NDK clang not executable under host tag: $HOST_TAG" >&2
    exit 1
fi

if [[ ! -x "$LLVM_HOME/bin/llvm-config" ]]; then
    echo "ERROR: matching llvm-config not found: $LLVM_HOME/bin/llvm-config" >&2
    exit 1
fi

if [[ ! -f "$PLUGIN" ]]; then
    echo "ERROR: plugin not found: $PLUGIN" >&2
    exit 1
fi

LLVM_LIBDIR="$("$LLVM_HOME/bin/llvm-config" --libdir)"

if [[ ! -d "$LLVM_LIBDIR" ]]; then
    echo "ERROR: llvm-config reported a missing libdir: $LLVM_LIBDIR" >&2
    exit 1
fi

if ! compgen -G "$LLVM_LIBDIR/libLLVM*" >/dev/null 2>&1; then
    echo "ERROR: no libLLVM* found under llvm-config libdir: $LLVM_LIBDIR" >&2
    exit 1
fi

PLUGIN_EXT="${PLUGIN##*.}"
if [[ "$PLUGIN_EXT" != "so" && "$PLUGIN_EXT" != "dylib" ]]; then
    echo "ERROR: unexpected plugin extension: $PLUGIN_EXT" >&2
    exit 1
fi

PACKAGE_NAME="${PACKAGE_NAME:-amice-android-ndk-${NDK_RELEASE}-${HOST_TAG}}"
STAGING="$OUT_DIR/.staging"
BUNDLE_DIR="$STAGING/$PACKAGE_NAME"
ARCHIVE="$OUT_DIR/$PACKAGE_NAME.tar.gz"

rm -rf "$BUNDLE_DIR" "$ARCHIVE"
mkdir -p "$BUNDLE_DIR/amice/bin" "$BUNDLE_DIR/amice/lib" "$BUNDLE_DIR/amice/llvm-lib"

echo "Copying Android NDK: $NDK_HOME"
cp -a "$NDK_HOME" "$BUNDLE_DIR/android-ndk-${NDK_RELEASE}"

echo "Copying AMICE plugin: $PLUGIN"
cp -a "$PLUGIN" "$BUNDLE_DIR/amice/lib/libamice.$PLUGIN_EXT"

echo "Copying LLVM runtime from: $LLVM_LIBDIR"
while IFS= read -r -d '' file; do
    cp -a "$file" "$BUNDLE_DIR/amice/llvm-lib/"
done < <(
    find "$LLVM_LIBDIR" -maxdepth 1 \( \
        -name 'libLLVM*' -o \
        -name 'libclang-cpp*' -o \
        -name 'libc++*' -o \
        -name 'libc++abi*' -o \
        -name 'libedit*' -o \
        -name 'libncurses*' -o \
        -name 'libtinfo*' -o \
        -name 'libunwind*' \
        -o -name 'libxml2*' \
    \) -print0
)

if ! compgen -G "$BUNDLE_DIR/amice/llvm-lib/libLLVM*" >/dev/null 2>&1; then
    echo "ERROR: no libLLVM runtime was copied into the bundle" >&2
    exit 1
fi

if [[ "$PLUGIN_EXT" == "so" && ! -e "$BUNDLE_DIR/amice/llvm-lib/libLLVM.so" ]]; then
    candidate="$(find "$BUNDLE_DIR/amice/llvm-lib" -maxdepth 1 -name 'libLLVM*.so*' -print | sort | head -n 1 || true)"
    if [[ -n "$candidate" ]]; then
        ln -s "$(basename "$candidate")" "$BUNDLE_DIR/amice/llvm-lib/libLLVM.so"
    fi
fi

if [[ "$PLUGIN_EXT" == "dylib" && ! -e "$BUNDLE_DIR/amice/llvm-lib/libLLVM.dylib" ]]; then
    candidate="$(find "$BUNDLE_DIR/amice/llvm-lib" -maxdepth 1 -name 'libLLVM*.dylib*' -print | sort | head -n 1 || true)"
    if [[ -n "$candidate" ]]; then
        ln -s "$(basename "$candidate")" "$BUNDLE_DIR/amice/llvm-lib/libLLVM.dylib"
    fi
fi

cat > "$BUNDLE_DIR/amice/bin/amice-clang-wrapper" <<EOF
#!/usr/bin/env bash
set -euo pipefail

tool_name="\$(basename "\$0")"
amice_dir="\$(cd "\$(dirname "\${BASH_SOURCE[0]}")/.." && pwd)"
bundle_root="\$(cd "\$amice_dir/.." && pwd)"
ndk_home="\${AMICE_ANDROID_NDK_HOME:-\$bundle_root/android-ndk-${NDK_RELEASE}}"
host_tag="\${AMICE_ANDROID_HOST_TAG:-${HOST_TAG}}"
toolchain="\$ndk_home/toolchains/llvm/prebuilt/\$host_tag"
plugin="\$amice_dir/lib/libamice.${PLUGIN_EXT}"
llvm_lib="\$amice_dir/llvm-lib"

if [[ ! -d "\$toolchain" ]]; then
    echo "ERROR: Android NDK toolchain not found: \$toolchain" >&2
    echo "Set AMICE_ANDROID_NDK_HOME if you moved the NDK out of this bundle." >&2
    exit 1
fi

if [[ ! -f "\$plugin" ]]; then
    echo "ERROR: AMICE plugin not found: \$plugin" >&2
    exit 1
fi

case "\$(uname -s)" in
    Darwin)
        export DYLD_LIBRARY_PATH="\$llvm_lib\${DYLD_LIBRARY_PATH:+:\$DYLD_LIBRARY_PATH}"
        ;;
    *)
        export LD_LIBRARY_PATH="\$llvm_lib\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}"
        ;;
esac

compiler="\$toolchain/bin/clang"
case "\$tool_name" in
    *++|*clang++)
        compiler="\$toolchain/bin/clang++"
        ;;
esac

target=""
case "\$tool_name" in
    aarch64-linux-android-clang*) target="aarch64-linux-android\${AMICE_ANDROID_API:-23}" ;;
    armv7a-linux-androideabi-clang*) target="armv7a-linux-androideabi\${AMICE_ANDROID_API:-19}" ;;
    x86_64-linux-android-clang*) target="x86_64-linux-android\${AMICE_ANDROID_API:-23}" ;;
    i686-linux-android-clang*) target="i686-linux-android\${AMICE_ANDROID_API:-19}" ;;
esac

has_target=false
for arg in "\$@"; do
    case "\$arg" in
        --target=*|--target|-target)
            has_target=true
            break
            ;;
    esac
done

args=("-fpass-plugin=\$plugin")
if [[ -n "\$target" && "\$has_target" == false ]]; then
    args=("--target=\$target" "\${args[@]}")
fi

exec "\$compiler" "\${args[@]}" "\$@"
EOF

chmod +x "$BUNDLE_DIR/amice/bin/amice-clang-wrapper"
for wrapper in \
    amice-clang \
    amice-clang++ \
    aarch64-linux-android-clang \
    aarch64-linux-android-clang++ \
    armv7a-linux-androideabi-clang \
    armv7a-linux-androideabi-clang++ \
    x86_64-linux-android-clang \
    x86_64-linux-android-clang++ \
    i686-linux-android-clang \
    i686-linux-android-clang++; do
    ln -s amice-clang-wrapper "$BUNDLE_DIR/amice/bin/$wrapper"
done

cat > "$BUNDLE_DIR/amice/env.sh" <<EOF
#!/usr/bin/env bash
amice_dir="\$(cd "\$(dirname "\${BASH_SOURCE[0]}")" && pwd)"
export AMICE_ANDROID_NDK_HOME="\$amice_dir/../android-ndk-${NDK_RELEASE}"
export AMICE_ANDROID_HOST_TAG="\${AMICE_ANDROID_HOST_TAG:-${HOST_TAG}}"
case "\$(uname -s)" in
    Darwin)
        export DYLD_LIBRARY_PATH="\$amice_dir/llvm-lib\${DYLD_LIBRARY_PATH:+:\$DYLD_LIBRARY_PATH}"
        ;;
    *)
        export LD_LIBRARY_PATH="\$amice_dir/llvm-lib\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}"
        ;;
esac
export PATH="\$amice_dir/bin:\$PATH"
EOF
chmod +x "$BUNDLE_DIR/amice/env.sh"

cat > "$BUNDLE_DIR/README.md" <<EOF
# AMICE Android NDK Bundle

This bundle is meant for testing AMICE with Android NDK without hunting for a matching unstripped LLVM build.

Contents:

- \`android-ndk-${NDK_RELEASE}/\`: official Android NDK for this host.
- \`amice/lib/libamice.${PLUGIN_EXT}\`: AMICE pass plugin.
- \`amice/llvm-lib/\`: matching LLVM runtime libraries required when clang loads the plugin.
- \`amice/bin/\`: wrapper compilers. They set the runtime library path and add \`-fpass-plugin\`.

Build metadata:

- NDK release: \`${NDK_RELEASE}\`
- LLVM feature: \`${LLVM_FEATURE}\`
- Android clang revision: \`${CLANG_REVISION}\`
- Host tag: \`${HOST_TAG}\`

Smoke test:

\`\`\`bash
cat > hello.c <<'SRC'
int main(void) { return 0; }
SRC

AMICE_STRING_ENCRYPTION=true ./amice/bin/aarch64-linux-android-clang hello.c -o hello
file hello
\`\`\`

Default target wrapper API levels:

- \`aarch64-linux-android-*\`: API 23
- \`x86_64-linux-android-*\`: API 23
- \`armv7a-linux-androideabi-*\`: API 19
- \`i686-linux-android-*\`: API 19

Override with \`AMICE_ANDROID_API=21\`, or pass \`--target=...\` yourself.

The generic \`amice-clang\` and \`amice-clang++\` wrappers do not choose an Android target. Use them with an explicit target, for example \`--target=aarch64-linux-android23\`. If you want a default Android target, use the ABI-named wrappers such as \`aarch64-linux-android-clang\`.

For CMake/Gradle, use \`android-ndk-${NDK_RELEASE}\` as the NDK path, add \`-fpass-plugin=\$PWD/amice/lib/libamice.${PLUGIN_EXT}\`, and make sure \`amice/llvm-lib\` is in \`LD_LIBRARY_PATH\` on Linux or \`DYLD_LIBRARY_PATH\` on macOS.
EOF

(
    cd "$STAGING"
    tar -czf "$ARCHIVE" "$PACKAGE_NAME"
)

(
    cd "$OUT_DIR"
    sha256sum "$(basename "$ARCHIVE")" > "$(basename "$ARCHIVE").sha256" 2>/dev/null || shasum -a 256 "$(basename "$ARCHIVE")" > "$(basename "$ARCHIVE").sha256"
)

echo "Created: $ARCHIVE"
echo "Created: $ARCHIVE.sha256"
