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
    build
        .file("cpp/module.cc")
        .file("cpp/function.cc")
        .file("cpp/basic_block.cc")
        .file("cpp/instruction.cc")
        .file("cpp/value.cc")
        .file("cpp/dominators.cc")
        .file("cpp/code_extractor.cc")
        .file("cpp/attribute.cc")
        .file("cpp/support.cc");

    if std::env::var_os("CARGO_FEATURE_ANDROID_NDK").is_some() {
        build.define("AMICE_ENABLE_CLONE_FUNCTION", None);
    }

    build.compile("amice-llvm-ffi");

    amice_build_support::emit_llvm_link(&probe);

    println!("cargo:rerun-if-changed=cpp");
}
