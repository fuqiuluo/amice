# Amice Integration Test Runner for Windows
# Usage: .\tests\scripts\run_tests.ps1 [options] [test_filter]
#
# Options:
#   -Build          Build amice before running tests
#   -Verbose        Show verbose output
#   -List           List available tests without running
#   -Help           Show this help message
#
# Examples:
#   .\tests\scripts\run_tests.ps1                    # Run all tests
#   .\tests\scripts\run_tests.ps1 string             # Run tests matching 'string'
#   .\tests\scripts\run_tests.ps1 -Verbose md5       # Run MD5 tests with verbose output

param(
    [switch]$Build,
    [switch]$Verbose,
    [switch]$List,
    [switch]$Help,
    [Parameter(ValueFromRemainingArguments=$true)]
    [string[]]$TestFilter
)

$ErrorActionPreference = "Stop"

# Get project root
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectRoot = (Get-Item "$ScriptDir\..\..").FullName

# Colors
function Write-Info { Write-Host "[INFO] $args" -ForegroundColor Blue }
function Write-Success { Write-Host "[OK] $args" -ForegroundColor Green }
function Write-Warn { Write-Host "[WARN] $args" -ForegroundColor Yellow }
function Write-Error { Write-Host "[ERROR] $args" -ForegroundColor Red }

# Show help
function Show-Help {
    Get-Content $MyInvocation.ScriptName | Select-Object -First 15 | ForEach-Object { $_ -replace '^#\s?', '' }
    exit 0
}

# Detect LLVM version
function Get-LlvmFeature {
    $versions = @(
        @{ Env = "LLVM_SYS_210_PREFIX"; Feature = "llvm21-1" },
        @{ Env = "LLVM_SYS_201_PREFIX"; Feature = "llvm20-1" },
        @{ Env = "LLVM_SYS_191_PREFIX"; Feature = "llvm19-1" },
        @{ Env = "LLVM_SYS_181_PREFIX"; Feature = "llvm18-1" },
        @{ Env = "LLVM_SYS_170_PREFIX"; Feature = "llvm17-0" },
        @{ Env = "LLVM_SYS_160_PREFIX"; Feature = "llvm16-0" }
    )

    foreach ($v in $versions) {
        $value = [Environment]::GetEnvironmentVariable($v.Env)
        if ($value) {
            Write-Info "Detected LLVM: $($v.Feature) (from $($v.Env))"
            return $v.Feature
        }
    }

    Write-Warn "No LLVM environment variable detected, using default feature"
    return $null
}

# Build amice
function Build-Amice {
    param([string]$LlvmFeature)

    Write-Info "Building amice plugin..."

    $args = @("build", "--release")

    if ($LlvmFeature) {
        $args += @("--no-default-features", "--features", "$LlvmFeature,win-link-lld")
    } else {
        $args += @("--features", "win-link-lld")
    }

    Push-Location $ProjectRoot
    try {
        & cargo $args
        if ($LASTEXITCODE -ne 0) {
            Write-Error "Build failed"
            exit 1
        }
        Write-Success "Build completed"
    } finally {
        Pop-Location
    }
}

# Check plugin exists
function Test-Plugin {
    $plugin = Join-Path $ProjectRoot "target\release\amice.dll"
    return Test-Path $plugin
}

# Run tests
function Invoke-Tests {
    param(
        [string]$LlvmFeature,
        [switch]$VerboseOutput,
        [string[]]$Filter
    )

    Write-Info "Running integration tests..."

    $args = @("test", "--release")

    if ($LlvmFeature) {
        $args += @("--no-default-features", "--features", "$LlvmFeature,win-link-lld")
    } else {
        $args += @("--features", "win-link-lld")
    }

    if ($VerboseOutput) {
        $args += @("--", "--nocapture")
        if ($Filter) {
            $args += $Filter
        }
    } elseif ($Filter) {
        $args += @("--")
        $args += $Filter
    }

    Push-Location $ProjectRoot
    try {
        & cargo $args
        if ($LASTEXITCODE -ne 0) {
            Write-Error "Tests failed"
            exit 1
        }
    } finally {
        Pop-Location
    }
}

# List tests
function Get-Tests {
    param([string]$LlvmFeature)

    Write-Info "Available integration tests:"

    $args = @("test", "--release")

    if ($LlvmFeature) {
        $args += @("--no-default-features", "--features", "$LlvmFeature,win-link-lld")
    } else {
        $args += @("--features", "win-link-lld")
    }

    $args += @("--", "--list")

    Push-Location $ProjectRoot
    try {
        & cargo $args 2>$null | Where-Object { $_ -match "^test " } | ForEach-Object { "  " + ($_ -replace "^test ", "") }
    } finally {
        Pop-Location
    }
}

# Main
if ($Help) {
    Show-Help
}

Write-Host ""
Write-Host "=========================================="
Write-Host "  Amice Integration Test Runner"
Write-Host "=========================================="
Write-Host ""

$LlvmFeature = Get-LlvmFeature

if ($List) {
    Get-Tests -LlvmFeature $LlvmFeature
    exit 0
}

# Handle build
if ($Build -or -not (Test-Plugin)) {
    if (-not $Build -and -not (Test-Plugin)) {
        Write-Warn "Plugin not found, building..."
    }
    Build-Amice -LlvmFeature $LlvmFeature
}

# Create output directory
$outputDir = Join-Path $ProjectRoot "target\test-outputs"
if (-not (Test-Path $outputDir)) {
    New-Item -ItemType Directory -Path $outputDir | Out-Null
}

# Run tests
Write-Host ""
Invoke-Tests -LlvmFeature $LlvmFeature -VerboseOutput:$Verbose -Filter $TestFilter

Write-Host ""
Write-Success "All tests completed!"
