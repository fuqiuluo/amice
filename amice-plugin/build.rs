use amice_build_support::LlvmProbe;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(LLVM_NOT_FOUND)");

    let probe = LlvmProbe::detect();
    println!("cargo:rustc-env=LLVM_VERSION_MAJOR={}", probe.version_tag());
    println!("cargo:rerun-if-env-changed={}", probe.env_prefix_var());

    if !probe.is_found() {
        println!("cargo:rustc-cfg=LLVM_NOT_FOUND");
        return;
    }

    let mut build = amice_build_support::cxx_build(&probe);
    build.file("cpp/ffi.cc");
    build.compile("amice-plugin-cpp");

    amice_build_support::emit_llvm_link(&probe);

    println!("cargo:rerun-if-changed=cpp");
}
