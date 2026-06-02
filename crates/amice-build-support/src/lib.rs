//! Shared build-script helpers for the amice workspace.
//!
//! Every native-linking crate in this repo (`amice-llvm`, `amice-plugin`, ...)
//! needs the same chores in its `build.rs`:
//!
//! 1. figure out which LLVM `(major, minor)` it was built against,
//! 2. locate a compatible `llvm-config`,
//! 3. compile a C++ FFI shim against the LLVM headers, and
//! 4. emit the right `rustc-link-lib` directives.
//!
//! Previously each crate carried a ~160-line copy of the `llvm-sys`-derived
//! probing logic. This crate is the single shared implementation.
//!
//! # Why env vars instead of `cfg!`
//!
//! The LLVM version is selected through a `llvmXX-Y` Cargo feature on the
//! *consuming* crate, not on this helper. A build script cannot see the
//! consumer's features via `cfg!`, but Cargo exports them to the build-script
//! process as `CARGO_FEATURE_<NAME>` environment variables. We read those, so
//! the same compiled helper works for any consumer regardless of its features.

use std::env;
use std::ffi::OsStr;
use std::io::{self, ErrorKind};
use std::path::PathBuf;
use std::process::Command;

use regex::Regex;
use semver::Version;

/// Re-exported so consumers compile against the exact same `cc` as this helper
/// (the [`cc::Build`] returned by [`cxx_build`] must be the same type).
pub use cc;

/// Supported `(major, minor)` versions and their matching Cargo feature env var.
///
/// Ordered low-to-high; the first enabled feature wins.
const LLVM_FEATURES: &[((u32, u32), &str)] = &[
    ((11, 0), "CARGO_FEATURE_LLVM11_0"),
    ((12, 0), "CARGO_FEATURE_LLVM12_0"),
    ((13, 0), "CARGO_FEATURE_LLVM13_0"),
    ((14, 0), "CARGO_FEATURE_LLVM14_0"),
    ((15, 0), "CARGO_FEATURE_LLVM15_0"),
    ((16, 0), "CARGO_FEATURE_LLVM16_0"),
    ((17, 0), "CARGO_FEATURE_LLVM17_0"),
    ((18, 1), "CARGO_FEATURE_LLVM18_1"),
    ((19, 1), "CARGO_FEATURE_LLVM19_1"),
    ((20, 1), "CARGO_FEATURE_LLVM20_1"),
    ((21, 1), "CARGO_FEATURE_LLVM21_1"),
];

/// Determine the LLVM `(major, minor)` from the consuming crate's enabled
/// `llvmXX-Y` feature (read via `CARGO_FEATURE_*`).
///
/// Panics if no `llvm*` feature is enabled, mirroring the old behaviour.
pub fn llvm_version_from_features() -> (u32, u32) {
    for (version, var) in LLVM_FEATURES {
        if env::var_os(var).is_some() {
            return *version;
        }
    }
    panic!(
        "amice-build-support: the consuming crate has no `llvmXX-Y` feature enabled \
         (expected exactly one of llvm11-0 .. llvm21-1)"
    );
}

/// A located `llvm-config` for the version the consumer was built against.
pub struct LlvmProbe {
    pub major: u32,
    pub minor: u32,
    config_path: Option<PathBuf>,
}

impl LlvmProbe {
    /// Resolve the LLVM version from features and search for a compatible
    /// `llvm-config` (honouring `LLVM_SYS_<major><minor>_PREFIX`).
    pub fn detect() -> Self {
        let (major, minor) = llvm_version_from_features();
        let config_path = locate_llvm_config(major, minor);
        Self {
            major,
            minor,
            config_path,
        }
    }

    /// `true` if a compatible `llvm-config` was found.
    pub fn is_found(&self) -> bool {
        self.config_path.is_some()
    }

    /// The `LLVM_SYS_<major><minor>_PREFIX` env var name for this version.
    pub fn env_prefix_var(&self) -> String {
        format!("LLVM_SYS_{}{}_PREFIX", self.major, self.minor)
    }

    /// `LLVM_VERSION_MAJOR` value, e.g. `211` for LLVM 21.1.
    pub fn version_tag(&self) -> String {
        format!("{}{}", self.major, self.minor)
    }

    /// Run `llvm-config <arg>` and return its trimmed stdout.
    ///
    /// Panics if `llvm-config` was not found; guard with [`is_found`].
    ///
    /// [`is_found`]: Self::is_found
    pub fn config(&self, arg: &str) -> String {
        let path = self
            .config_path
            .as_ref()
            .expect("amice-build-support: llvm-config not found (check LlvmProbe::is_found first)");

        // Preserve the legacy side-channel file (gitignored, may be read by
        // external editor/LSP tooling). Written to the consumer crate root.
        let output_path = PathBuf::from(".llvm-config-path");
        if !output_path.exists() {
            if let Some(s) = path.to_str() {
                let _ = std::fs::write(output_path, s);
            }
        }

        llvm_config_ex(path, arg)
            .expect("amice-build-support: surprising failure from llvm-config")
            .trim()
            .to_string()
    }

    /// `llvm-config --includedir`.
    pub fn includedir(&self) -> String {
        self.config("--includedir")
    }

    /// `llvm-config --libdir`.
    pub fn libdir(&self) -> String {
        self.config("--libdir")
    }

    /// Whether LLVM was built with RTTI (`llvm-config --has-rtti`).
    pub fn has_rtti(&self) -> bool {
        self.config("--has-rtti") == "YES"
    }
}

/// Build a [`cc::Build`] preconfigured to compile an LLVM C++ FFI shim:
/// the LLVM include dir, C++17, `-fno-rtti` when LLVM lacks RTTI, warnings off.
///
/// The caller adds `.file(...)` entries and calls `.compile(name)`.
pub fn cxx_build(probe: &LlvmProbe) -> cc::Build {
    let mut build = cc::Build::new();
    build.cpp(true).include(probe.includedir());

    // Pass the standard unconditionally: every compiler we target supports
    // C++17, and the LLVM headers require it. `flag_if_supported` was observed
    // to silently drop the flag in release builds, leaving the default
    // (pre-C++17) standard and breaking on std::optional / std::size.
    if is_msvc() {
        build.flag("/std:c++17");
    } else {
        build.flag("-std=c++17");
    }

    if probe.has_rtti() {
        if is_msvc() {
            build.flag_if_supported("/GR-");
        } else {
            build.flag_if_supported("-fno-rtti");
        }
    }

    build.warnings(false);
    build
}

/// Emit the `rustc-link-search` / `rustc-link-lib` directives for linking
/// against LLVM, including the Windows `opt`/`lld` selection driven by the
/// consumer's `win-link-opt` / `win-link-lld` features.
pub fn emit_llvm_link(probe: &LlvmProbe) {
    let libdir = probe.libdir();
    println!("cargo:rustc-link-search=native={libdir}");

    if target_os() == "windows" {
        println!("cargo:rustc-link-lib=dylib=LLVM-C");
        if env::var_os("CARGO_FEATURE_WIN_LINK_OPT").is_some() {
            println!("cargo:rustc-link-lib=dylib=opt");
        }
        if env::var_os("CARGO_FEATURE_WIN_LINK_LLD").is_some() {
            println!("cargo:rustc-link-lib=dylib=lld");
        }
    } else {
        println!("cargo:rustc-link-lib=dylib=LLVM");
    }
}

/// Resolve target os from `CARGO_CFG_TARGET_OS` (set by Cargo for build
/// scripts), which reflects the *target* — correct under cross-compilation.
fn target_os() -> String {
    env::var("CARGO_CFG_TARGET_OS").unwrap_or_default()
}

fn is_msvc() -> bool {
    env::var("CARGO_CFG_TARGET_ENV").map(|e| e == "msvc").unwrap_or(false)
}

// ---------------------------------------------------------------------------
// llvm-config location. Adapted from the `llvm-sys` crate, like the upstream
// `llvm-plugin` build script it replaces.
// ---------------------------------------------------------------------------

fn locate_llvm_config(major: u32, minor: u32) -> Option<PathBuf> {
    let prefix_var = format!("LLVM_SYS_{major}{minor}_PREFIX");
    let prefix = env::var_os(&prefix_var)
        .map(|p| PathBuf::from(p).join("bin"))
        .unwrap_or_default();

    // Preserve the legacy side-channel file (gitignored).
    let prefix_output = PathBuf::from(".llvm-prefix-path");
    if !prefix_output.exists() {
        if let Some(s) = prefix.to_str() {
            let _ = std::fs::write(prefix_output, s);
        }
    }

    for binary_name in llvm_config_binary_names(major, minor) {
        let binary_name = prefix.join(binary_name);
        match llvm_version(&binary_name) {
            // `llvm-sys` already does strict version checking for us, so we
            // only need the major to match.
            Ok(version) if major as u64 == version.major => return Some(binary_name),
            Ok(_) => continue,
            Err(ref e) if e.kind() == ErrorKind::NotFound => {
                // Keep searching the remaining candidate names.
            },
            Err(e) => panic!("amice-build-support: failed to search PATH for llvm-config: {e}"),
        }
    }

    None
}

fn llvm_config_binary_names(major: u32, minor: u32) -> std::vec::IntoIter<String> {
    let mut base_names = vec![
        "llvm-config".to_string(),
        format!("llvm-config-{major}"),
        format!("llvm{major}-config"),
        format!("llvm-config-{major}.{minor}"),
        format!("llvm-config{major}{minor}"),
    ];

    if target_os() == "windows" {
        let exe_names: Vec<String> = base_names.iter().map(|n| format!("{n}.exe")).collect();
        base_names.extend(exe_names);
    }

    base_names.into_iter()
}

fn llvm_config_ex<S: AsRef<OsStr>>(binary: S, arg: &str) -> io::Result<String> {
    Command::new(binary).arg(arg).output().and_then(|output| {
        if output.stdout.is_empty() {
            Err(io::Error::new(ErrorKind::NotFound, "llvm-config returned empty output"))
        } else {
            Ok(String::from_utf8(output.stdout).expect("Output from llvm-config was not valid UTF-8"))
        }
    })
}

fn llvm_version<S: AsRef<OsStr>>(binary: &S) -> io::Result<Version> {
    let version_str = llvm_config_ex(binary.as_ref(), "--version")?;

    // LLVM isn't strictly semver (e.g. '3.8.0svn'), so parse only the numeric
    // prefix.
    let re = Regex::new(r"^(?P<major>\d+)\.(?P<minor>\d+)(?:\.(?P<patch>\d+))??").unwrap();
    let c = match re.captures(&version_str) {
        Some(c) => c,
        None => panic!("amice-build-support: could not parse LLVM version string: {version_str}"),
    };

    // Version requires a patch component; synthesize `.0` when missing.
    let s = match c.name("patch") {
        None => format!("{}.0", &c[0]),
        Some(_) => c[0].to_string(),
    };
    Ok(Version::parse(&s).unwrap())
}
