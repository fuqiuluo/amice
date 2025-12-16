#!/bin/bash
# Amice Integration Test Runner
# Usage: ./tests/scripts/run_tests.sh [options] [test_filter]
#
# Options:
#   -b, --build     Build amice before running tests (default: auto-detect)
#   -r, --release   Use release build (default)
#   -v, --verbose   Show verbose output
#   -l, --list      List available tests without running
#   -h, --help      Show this help message
#
# LLVM Detection:
#   1. First checks LLVM_SYS_*_PREFIX environment variables
#   2. Falls back to llvm-config --version auto-detection
#   3. Supports llvm-config-XX variant names (e.g., llvm-config-18)
#
# Examples:
#   ./tests/scripts/run_tests.sh                    # Run all tests
#   ./tests/scripts/run_tests.sh string             # Run tests matching 'string'
#   ./tests/scripts/run_tests.sh -v md5             # Run MD5 tests with verbose output
#   ./tests/scripts/run_tests.sh --list             # List all available tests

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Default options
BUILD=auto
VERBOSE=""
LIST_ONLY=false
TEST_FILTER=""
LLVM_FEATURE=""
LLVM_PREFIX=""
LLVM_ENV_VAR=""

# Detect project root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Print colored message
info() { echo -e "${BLUE}[INFO]${NC} $1"; }
success() { echo -e "${GREEN}[OK]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1"; }

# Show help
show_help() {
    head -24 "$0" | tail -22 | sed 's/^#//'
    exit 0
}

# Map LLVM version to feature flag and env var
# Args: major_version minor_version
map_llvm_version() {
    local major=$1
    local minor=${2:-0}

    case "$major" in
        21) LLVM_FEATURE="llvm21-1"; LLVM_ENV_VAR="LLVM_SYS_210_PREFIX" ;;
        20) LLVM_FEATURE="llvm20-1"; LLVM_ENV_VAR="LLVM_SYS_201_PREFIX" ;;
        19) LLVM_FEATURE="llvm19-1"; LLVM_ENV_VAR="LLVM_SYS_191_PREFIX" ;;
        18) LLVM_FEATURE="llvm18-1"; LLVM_ENV_VAR="LLVM_SYS_181_PREFIX" ;;
        17) LLVM_FEATURE="llvm17-0"; LLVM_ENV_VAR="LLVM_SYS_170_PREFIX" ;;
        16) LLVM_FEATURE="llvm16-0"; LLVM_ENV_VAR="LLVM_SYS_160_PREFIX" ;;
        15) LLVM_FEATURE="llvm15-0"; LLVM_ENV_VAR="LLVM_SYS_150_PREFIX" ;;
        14) LLVM_FEATURE="llvm14-0"; LLVM_ENV_VAR="LLVM_SYS_140_PREFIX" ;;
        13) LLVM_FEATURE="llvm13-0"; LLVM_ENV_VAR="LLVM_SYS_130_PREFIX" ;;
        12) LLVM_FEATURE="llvm12-0"; LLVM_ENV_VAR="LLVM_SYS_120_PREFIX" ;;
        11) LLVM_FEATURE="llvm11-0"; LLVM_ENV_VAR="LLVM_SYS_110_PREFIX" ;;
        *)
            error "Unsupported LLVM version: $major"
            return 1
            ;;
    esac
    return 0
}

# Find llvm-config executable
# Tries: llvm-config, llvm-config-XX (for versions 21 down to 11)
find_llvm_config() {
    # Try plain llvm-config first
    if command -v llvm-config &>/dev/null; then
        echo "llvm-config"
        return 0
    fi

    # Try version-specific variants
    for ver in 21 20 19 18 17 16 15 14 13 12 11; do
        if command -v "llvm-config-$ver" &>/dev/null; then
            echo "llvm-config-$ver"
            return 0
        fi
    done

    # macOS Homebrew paths
    if [[ "$(uname -s)" == "Darwin" ]]; then
        for ver in 21 20 19 18 17 16 15 14; do
            local brew_path="/opt/homebrew/opt/llvm@$ver/bin/llvm-config"
            if [[ -x "$brew_path" ]]; then
                echo "$brew_path"
                return 0
            fi
            # Intel Mac path
            brew_path="/usr/local/opt/llvm@$ver/bin/llvm-config"
            if [[ -x "$brew_path" ]]; then
                echo "$brew_path"
                return 0
            fi
        done
    fi

    return 1
}

# Detect LLVM from llvm-config
detect_llvm_from_config() {
    local llvm_config
    llvm_config=$(find_llvm_config) || return 1

    # Get version string (e.g., "18.1.8" or "19.0.0git")
    local version_str
    version_str=$("$llvm_config" --version 2>/dev/null) || return 1

    # Extract major.minor version
    local major minor
    major=$(echo "$version_str" | cut -d. -f1)
    minor=$(echo "$version_str" | cut -d. -f2)

    # Get prefix path
    LLVM_PREFIX=$("$llvm_config" --prefix 2>/dev/null) || return 1

    if map_llvm_version "$major" "$minor"; then
        info "Detected LLVM $version_str via $llvm_config"
        info "  Prefix: $LLVM_PREFIX"
        info "  Feature: $LLVM_FEATURE"
        return 0
    fi

    return 1
}

# Detect LLVM version from environment variables
detect_llvm_from_env() {
    local llvm_versions=(
        "LLVM_SYS_210_PREFIX:llvm21-1"
        "LLVM_SYS_201_PREFIX:llvm20-1"
        "LLVM_SYS_191_PREFIX:llvm19-1"
        "LLVM_SYS_181_PREFIX:llvm18-1"
        "LLVM_SYS_170_PREFIX:llvm17-0"
        "LLVM_SYS_160_PREFIX:llvm16-0"
        "LLVM_SYS_150_PREFIX:llvm15-0"
        "LLVM_SYS_140_PREFIX:llvm14-0"
        "LLVM_SYS_130_PREFIX:llvm13-0"
        "LLVM_SYS_120_PREFIX:llvm12-0"
        "LLVM_SYS_110_PREFIX:llvm11-0"
    )

    for pair in "${llvm_versions[@]}"; do
        local env_var="${pair%%:*}"
        local feature="${pair##*:}"
        if [[ -n "${!env_var:-}" ]]; then
            LLVM_FEATURE="$feature"
            LLVM_ENV_VAR="$env_var"
            LLVM_PREFIX="${!env_var}"
            info "Detected LLVM via environment variable"
            info "  $env_var=$LLVM_PREFIX"
            info "  Feature: $LLVM_FEATURE"
            return 0
        fi
    done

    return 1
}

# Main LLVM detection function
detect_llvm() {
    # Priority 1: Environment variables
    if detect_llvm_from_env; then
        return 0
    fi

    # Priority 2: llvm-config
    if detect_llvm_from_config; then
        # Export the env var for cargo/llvm-sys
        export "$LLVM_ENV_VAR"="$LLVM_PREFIX"
        info "Exported $LLVM_ENV_VAR=$LLVM_PREFIX"
        return 0
    fi

    warn "No LLVM installation detected!"
    warn "Please either:"
    warn "  1. Set LLVM_SYS_*_PREFIX environment variable"
    warn "  2. Ensure llvm-config is in PATH"
    warn "Using default feature (may fail)"
    return 1
}

# Build amice plugin
build_amice() {
    info "Building amice plugin..."

    local args=("build" "--release")

    if [[ -n "$LLVM_FEATURE" ]]; then
        args+=("--no-default-features" "--features" "$LLVM_FEATURE")
    fi

    cd "$PROJECT_ROOT"
    if cargo "${args[@]}"; then
        success "Build completed"
    else
        error "Build failed"
        exit 1
    fi
}

# Check if plugin exists
check_plugin() {
    local plugin=""
    case "$(uname -s)" in
        Darwin) plugin="$PROJECT_ROOT/target/release/libamice.dylib" ;;
        Linux)  plugin="$PROJECT_ROOT/target/release/libamice.so" ;;
        MINGW*|CYGWIN*|MSYS*) plugin="$PROJECT_ROOT/target/release/amice.dll" ;;
        *) plugin="$PROJECT_ROOT/target/release/libamice.so" ;;
    esac

    if [[ -f "$plugin" ]]; then
        return 0
    else
        return 1
    fi
}

# Run tests
run_tests() {
    info "Running integration tests..."

    local args=("test" "--release")

    if [[ -n "$LLVM_FEATURE" ]]; then
        args+=("--no-default-features" "--features" "$LLVM_FEATURE")
    fi

    if [[ -n "$VERBOSE" ]]; then
        args+=("--" "--nocapture")
    fi

    if [[ -n "$TEST_FILTER" ]]; then
        if [[ -n "$VERBOSE" ]]; then
            args+=("$TEST_FILTER")
        else
            args+=("--" "$TEST_FILTER")
        fi
    fi

    cd "$PROJECT_ROOT"
    cargo "${args[@]}"
}

# List available tests
list_tests() {
    info "Available integration tests:"

    local args=("test" "--release")

    if [[ -n "$LLVM_FEATURE" ]]; then
        args+=("--no-default-features" "--features" "$LLVM_FEATURE")
    fi

    args+=("--" "--list")

    cd "$PROJECT_ROOT"
    cargo "${args[@]}" 2>/dev/null | grep "^test " | sed 's/^test /  /'
}

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        -b|--build)
            BUILD=yes
            shift
            ;;
        -r|--release)
            # Already default
            shift
            ;;
        -v|--verbose)
            VERBOSE=yes
            shift
            ;;
        -l|--list)
            LIST_ONLY=true
            shift
            ;;
        -h|--help)
            show_help
            ;;
        -*)
            error "Unknown option: $1"
            show_help
            ;;
        *)
            TEST_FILTER="$1"
            shift
            ;;
    esac
done

# Main execution
echo ""
echo "=========================================="
echo "  Amice Integration Test Runner"
echo "=========================================="
echo ""

detect_llvm

if $LIST_ONLY; then
    list_tests
    exit 0
fi

# Handle build
if [[ "$BUILD" == "auto" ]]; then
    if check_plugin; then
        info "Plugin found, skipping build"
    else
        warn "Plugin not found, building..."
        build_amice
    fi
elif [[ "$BUILD" == "yes" ]]; then
    build_amice
fi

# Create output directory
mkdir -p "$PROJECT_ROOT/target/test-outputs"

# Run tests
echo ""
run_tests

echo ""
success "All tests completed!"
