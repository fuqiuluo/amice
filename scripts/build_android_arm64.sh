#!/usr/bin/env bash
# Build an Android ARM64 binary with amice obfuscation passes
#
# Usage:
#   ./scripts/build_android_arm64.sh <source.c> [output] [extra clang flags...]
#
# Environment overrides:
#   AMICE_ANDROID_BUNDLE - unpacked AMICE Android NDK bundle from release
#   NDK_HOME      - Android NDK root (auto-detected from ~/Android/Sdk/ndk/*)
#   API_LEVEL     - Android API level (default: 35)
#   LLVM_PREFIX   - LLVM 21 prefix (default: /usr/lib64/llvm21)
#   CLANG         - host clang used when not using AMICE_ANDROID_BUNDLE
#   AMICE_LLVM_LIBDIR - directory containing libLLVM.so for plugin loading
#
# Obfuscation env vars (pass before the script or export them):
#   AMICE_BOGUS_CONTROL_FLOW=true
#   AMICE_BOGUS_CONTROL_FLOW_MODE=basic|polaris-primes
#   AMICE_BOGUS_CONTROL_FLOW_PROB=80
#   AMICE_BOGUS_CONTROL_FLOW_LOOPS=1
#   AMICE_FLATTEN=true
#   AMICE_STRING_ENCRYPTION=true
#   ... (see docs/EnvConfig_en_US.md for all variables)

set -e

SOURCE="${1:?Usage: $0 <source.c> [output] [extra clang flags...]}"
OUTPUT="${2:-${SOURCE%.*}}"
shift 2 2>/dev/null || shift 1
EXTRA_FLAGS=("$@")

API_LEVEL="${API_LEVEL:-35}"

if [[ -n "${AMICE_ANDROID_BUNDLE:-}" ]]; then
    BUNDLE_CLANG="$AMICE_ANDROID_BUNDLE/amice/bin/aarch64-linux-android-clang"
    if [[ ! -x "$BUNDLE_CLANG" ]]; then
        echo "ERROR: bundle wrapper not found: $BUNDLE_CLANG" >&2
        exit 1
    fi

    echo "Bundle: $AMICE_ANDROID_BUNDLE"
    echo "API:    $API_LEVEL"
    echo "Source: $SOURCE -> $OUTPUT"
    echo ""

    AMICE_ANDROID_API="$API_LEVEL" "$BUNDLE_CLANG" \
        "${EXTRA_FLAGS[@]}" \
        "$SOURCE" -o "$OUTPUT"

    echo "Done: $OUTPUT"
    file "$OUTPUT"
    exit 0
fi

LLVM_PREFIX="${LLVM_PREFIX:-/usr/lib64/llvm21}"
export LLVM_SYS_211_PREFIX="$LLVM_PREFIX"

if [[ -z "$NDK_HOME" ]]; then
    NDK_BASE="$HOME/Android/Sdk/ndk"
    if [[ -d "$NDK_BASE" ]]; then
        # Pick the newest versioned NDK directory (e.g. 27.1.12297006), skip non-version dirs
        NDK_VERSION=$(ls "$NDK_BASE" | grep -E '^[0-9]+\.[0-9]+\.[0-9]+$' | sort -V | tail -1)
        if [[ -n "$NDK_VERSION" ]]; then
            NDK_HOME="$NDK_BASE/$NDK_VERSION"
        fi
    fi
fi

if [[ -z "$NDK_HOME" || ! -d "$NDK_HOME" ]]; then
    echo "ERROR: Android NDK not found. Set NDK_HOME or install via Android Studio." >&2
    exit 1
fi

case "$(uname -s)" in
    Linux) HOST_TAG="linux-x86_64" ;;
    Darwin) HOST_TAG="darwin-x86_64" ;;
    *)
        echo "ERROR: unsupported host OS: $(uname -s)" >&2
        exit 1
        ;;
esac

NDK_TOOLCHAIN="$NDK_HOME/toolchains/llvm/prebuilt/$HOST_TAG"
if [[ ! -d "$NDK_TOOLCHAIN" ]]; then
    echo "ERROR: Android NDK toolchain not found: $NDK_TOOLCHAIN" >&2
    exit 1
fi

NDK_SYSROOT="$NDK_TOOLCHAIN/sysroot"
NDK_LLD="$NDK_TOOLCHAIN/bin/ld.lld"
NDK_CLANG_RESOURCE="$(find "$NDK_TOOLCHAIN/lib/clang" -mindepth 1 -maxdepth 1 -type d | sort | tail -n 1)"

if [[ -z "$NDK_CLANG_RESOURCE" || ! -d "$NDK_CLANG_RESOURCE" ]]; then
    echo "ERROR: cannot find NDK clang resource dir under $NDK_TOOLCHAIN/lib/clang" >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PLUGIN="$PROJECT_ROOT/target/release/libamice.so"

if [[ ! -f "$PLUGIN" ]]; then
    echo "Plugin not found, building..."
    (cd "$PROJECT_ROOT" && cargo build --release)
fi

if [[ -n "${AMICE_LLVM_LIBDIR:-}" ]]; then
    if [[ "$(uname -s)" == "Darwin" ]]; then
        export DYLD_LIBRARY_PATH="$AMICE_LLVM_LIBDIR${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}"
    else
        export LD_LIBRARY_PATH="$AMICE_LLVM_LIBDIR${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    fi
fi

echo "NDK:    $NDK_HOME"
echo "API:    $API_LEVEL"
echo "Plugin: $PLUGIN"
echo "Source: $SOURCE -> $OUTPUT"
echo ""

"${CLANG:-clang}" \
    --target="aarch64-linux-android${API_LEVEL}" \
    --sysroot="$NDK_SYSROOT" \
    -fuse-ld="$NDK_LLD" \
    -resource-dir="$NDK_CLANG_RESOURCE" \
    -fpass-plugin="$PLUGIN" \
    "${EXTRA_FLAGS[@]}" \
    "$SOURCE" -o "$OUTPUT"

echo "Done: $OUTPUT"
file "$OUTPUT"
