#!/usr/bin/env bash
# Build an Android ARM64 binary with amice obfuscation passes
#
# Usage:
#   ./scripts/build_android_arm64.sh <source.c> [output] [extra clang flags...]
#
# Environment overrides:
#   NDK_HOME      - Android NDK root (auto-detected from ~/Android/Sdk/ndk/*)
#   API_LEVEL     - Android API level (default: 35)
#   LLVM_PREFIX   - LLVM 21 prefix (default: /usr/lib64/llvm21)
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

API_LEVEL="${API_LEVEL:-35}"
NDK_TOOLCHAIN="$NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64"
NDK_SYSROOT="$NDK_TOOLCHAIN/sysroot"
NDK_LLD="$NDK_TOOLCHAIN/bin/ld.lld"
NDK_CLANG_RESOURCE="$NDK_TOOLCHAIN/lib/clang/18"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PLUGIN="$PROJECT_ROOT/target/release/libamice.so"

if [[ ! -f "$PLUGIN" ]]; then
    echo "Plugin not found, building..."
    (cd "$PROJECT_ROOT" && cargo build --release)
fi

echo "NDK:    $NDK_HOME"
echo "API:    $API_LEVEL"
echo "Plugin: $PLUGIN"
echo "Source: $SOURCE -> $OUTPUT"
echo ""

clang \
    --target="aarch64-linux-android${API_LEVEL}" \
    --sysroot="$NDK_SYSROOT" \
    -fuse-ld="$NDK_LLD" \
    -resource-dir="$NDK_CLANG_RESOURCE" \
    -fpass-plugin="$PLUGIN" \
    "${EXTRA_FLAGS[@]}" \
    "$SOURCE" -o "$OUTPUT"

echo "Done: $OUTPUT"
file "$OUTPUT"