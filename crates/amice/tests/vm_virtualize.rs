//! AMICE VMP 指令级虚拟化集成测试。

mod common;

use common::{
    CompileResult, CppCompileBuilder, Language, LlvmConfig, ObfuscationConfig, clang_compiler_path, detect_llvm_config,
    ensure_plugin_built, fixture_path, output_dir, plugin_path, rust_fixture_project_path, sanitize_ir_for_llvm21,
};
use serial_test::serial;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn vm_virtualize_config() -> ObfuscationConfig {
    let mut config = ObfuscationConfig::disabled();
    config.vm_virtualize = Some(true);
    config
}

fn compatible_clang_available() -> bool {
    clang_major() == Some(llvm_major(&llvm_config()))
}

fn llvm_config() -> LlvmConfig {
    detect_llvm_config().unwrap_or_else(|| LlvmConfig {
        env_var: "LLVM_SYS_211_PREFIX".to_owned(),
        feature: "llvm21-1".to_owned(),
        prefix: "/usr/lib64/llvm21".to_owned(),
    })
}

fn llvm_major(config: &LlvmConfig) -> u32 {
    config
        .feature
        .strip_prefix("llvm")
        .and_then(|version| version.split_once('-'))
        .and_then(|(major, _)| major.parse::<u32>().ok())
        .expect("test LLVM feature should encode a major version")
}

fn clang_major() -> Option<u32> {
    let output = Command::new(clang_compiler_path(false))
        .env("CCACHE_DISABLE", "1")
        .arg("--version")
        .output()
        .expect("clang --version should run");
    let version = String::from_utf8_lossy(&output.stdout);
    version
        .split_whitespace()
        .skip_while(|word| *word != "version")
        .nth(1)
        .and_then(|version| version.split('.').next())
        .and_then(|major| major.parse::<u32>().ok())
}

fn compile_virtualized_ir(source: &Path, output_name: &str) -> PathBuf {
    compile_virtualized_ir_with_config(source, output_name, vm_virtualize_config())
}

fn compile_virtualized_ir_with_config(source: &Path, output_name: &str, config: ObfuscationConfig) -> PathBuf {
    if compatible_clang_available() {
        let result = CppCompileBuilder::new(source, output_name)
            .optimization("O1")
            .config(config)
            .arg("-S")
            .arg("-emit-llvm")
            .compile();
        result.assert_success();
        return result.binary_path;
    }

    compile_virtualized_ir_with_opt(source, output_name, config)
}

fn compile_virtualized_binary(source: &Path, output_name: &str) -> CompileResult {
    compile_virtualized_binary_with_config(source, output_name, vm_virtualize_config())
}

fn compile_virtualized_binary_with_config(
    source: &Path,
    output_name: &str,
    config: ObfuscationConfig,
) -> CompileResult {
    if compatible_clang_available() {
        return CppCompileBuilder::new(source, output_name)
            .optimization("O1")
            .config(config)
            .compile();
    }

    let ir_path = compile_virtualized_ir_with_opt(source, &format!("{output_name}.ll"), config);
    let binary_path = output_dir().join(output_name);
    let output = command_output(
        Command::new(clang_compiler_path(false))
            .env("CCACHE_DISABLE", "1")
            .arg(&ir_path)
            .arg("-o")
            .arg(&binary_path),
    );

    CompileResult { output, binary_path }
}

fn compile_virtualized_ir_with_opt(source: &Path, output_name: &str, config: ObfuscationConfig) -> PathBuf {
    eprintln!(
        "using llvm-opt fallback for vm_virtualize test: clang major {:?}, configured LLVM major {}",
        clang_major(),
        llvm_major(&llvm_config())
    );
    ensure_plugin_built();

    let llvm = llvm_config();
    let out_dir = output_dir();
    std::fs::create_dir_all(&out_dir).expect("test output dir should be creatable");
    let input_ir = out_dir.join(format!("{output_name}.input.ll"));
    let output_ir = out_dir.join(output_name);

    assert_success(command_output(
        Command::new(clang_compiler_path(false))
            .env("CCACHE_DISABLE", "1")
            .arg("-O1")
            .arg("-Xclang")
            .arg("-disable-lifetime-markers")
            .arg("-S")
            .arg("-emit-llvm")
            .arg(source)
            .arg("-o")
            .arg(&input_ir),
    ));
    sanitize_ir_for_llvm21(&input_ir);

    let opt = Path::new(&llvm.prefix).join("bin").join("opt");
    assert!(opt.exists(), "configured LLVM opt does not exist at {}", opt.display());
    let mut opt_command = Command::new(opt);
    opt_command
        .env(&llvm.env_var, &llvm.prefix)
        .arg(format!("--load-pass-plugin={}", plugin_path().display()))
        .arg("-passes=default<O1>")
        .arg("-S")
        .arg(&input_ir)
        .arg("-o")
        .arg(&output_ir);
    config.apply_to_command(&mut opt_command);
    assert_success(command_output(&mut opt_command));

    output_ir
}

fn compile_virtualized_ir_with_debug_log(
    source: &Path,
    output_name: &str,
    config: ObfuscationConfig,
) -> (PathBuf, Output) {
    ensure_plugin_built();

    let llvm = llvm_config();
    let out_dir = output_dir();
    std::fs::create_dir_all(&out_dir).expect("test output dir should be creatable");
    let input_ir = out_dir.join(format!("{output_name}.input.ll"));
    let output_ir = out_dir.join(output_name);

    assert_success(command_output(
        Command::new(clang_compiler_path(false))
            .env("CCACHE_DISABLE", "1")
            .arg("-O1")
            .arg("-Xclang")
            .arg("-disable-lifetime-markers")
            .arg("-S")
            .arg("-emit-llvm")
            .arg(source)
            .arg("-o")
            .arg(&input_ir),
    ));
    sanitize_ir_for_llvm21(&input_ir);

    let opt = Path::new(&llvm.prefix).join("bin").join("opt");
    assert!(opt.exists(), "configured LLVM opt does not exist at {}", opt.display());
    let mut opt_command = Command::new(opt);
    opt_command
        .env(&llvm.env_var, &llvm.prefix)
        .env("RUST_LOG", "amice=debug")
        .arg(format!("--load-pass-plugin={}", plugin_path().display()))
        .arg("-passes=default<O1>")
        .arg("-S")
        .arg(&input_ir)
        .arg("-o")
        .arg(&output_ir);
    config.apply_to_command(&mut opt_command);

    let output = command_output(&mut opt_command);
    (output_ir, output)
}

fn compile_rust_fixture_to_ir(source: &Path, output_name: &str) -> PathBuf {
    let out_dir = output_dir();
    std::fs::create_dir_all(&out_dir).expect("test output dir should be creatable");
    let output_ir = out_dir.join(output_name);

    assert_success(command_output(
        Command::new("rustc")
            .env("CCACHE_DISABLE", "1")
            .arg("--edition=2021")
            .arg("--crate-type=lib")
            .arg("-O")
            .arg("--emit=llvm-ir")
            .arg(source)
            .arg("-o")
            .arg(&output_ir),
    ));

    output_ir
}

fn optimize_ir_with_plugin(input_ir: &Path, output_name: &str, config: ObfuscationConfig) -> PathBuf {
    ensure_plugin_built();

    let llvm = llvm_config();
    let output_ir = output_dir().join(output_name);
    let opt = Path::new(&llvm.prefix).join("bin").join("opt");
    assert!(opt.exists(), "configured LLVM opt does not exist at {}", opt.display());

    let mut opt_command = Command::new(opt);
    opt_command
        .env(&llvm.env_var, &llvm.prefix)
        .arg(format!("--load-pass-plugin={}", plugin_path().display()))
        .arg("-passes=default<O1>")
        .arg("-S")
        .arg(input_ir)
        .arg("-o")
        .arg(&output_ir);
    config.apply_to_command(&mut opt_command);
    assert_success(command_output(&mut opt_command));

    output_ir
}

fn optimize_ir_with_plugin_debug(input_ir: &Path, output_name: &str, config: ObfuscationConfig) -> (PathBuf, Output) {
    optimize_ir_with_plugin_debug_pipeline(input_ir, output_name, "default<O1>", config)
}

fn optimize_ir_with_plugin_debug_pipeline(
    input_ir: &Path,
    output_name: &str,
    pipeline: &str,
    config: ObfuscationConfig,
) -> (PathBuf, Output) {
    ensure_plugin_built();

    let llvm = llvm_config();
    let output_ir = output_dir().join(output_name);
    let opt = Path::new(&llvm.prefix).join("bin").join("opt");
    assert!(opt.exists(), "configured LLVM opt does not exist at {}", opt.display());

    let mut opt_command = Command::new(opt);
    opt_command
        .env(&llvm.env_var, &llvm.prefix)
        .env("RUST_LOG", "amice=debug")
        .arg(format!("--load-pass-plugin={}", plugin_path().display()))
        .arg(format!("-passes={pipeline}"))
        .arg("-S")
        .arg(input_ir)
        .arg("-o")
        .arg(&output_ir);
    config.apply_to_command(&mut opt_command);

    let output = command_output(&mut opt_command);
    (output_ir, output)
}

fn compile_ir_with_c_harness(ir_path: &Path, c_source: &Path, output_name: &str) -> CompileResult {
    compile_ir_with_c_harness_and_args(ir_path, c_source, output_name, &[])
}

fn compile_ir_binary(ir_path: &Path, output_name: &str) -> CompileResult {
    let binary_path = output_dir().join(output_name);
    let output = command_output(
        Command::new(clang_compiler_path(false))
            .env("CCACHE_DISABLE", "1")
            .arg(ir_path)
            .arg("-o")
            .arg(&binary_path),
    );

    CompileResult { output, binary_path }
}

fn compile_ir_with_c_harness_and_args(
    ir_path: &Path,
    c_source: &Path,
    output_name: &str,
    extra_args: &[&str],
) -> CompileResult {
    let binary_path = output_dir().join(output_name);
    let mut command = Command::new(clang_compiler_path(false));
    command
        .env("CCACHE_DISABLE", "1")
        .arg(ir_path)
        .arg(c_source)
        .arg("-o")
        .arg(&binary_path);
    for arg in extra_args {
        command.arg(arg);
    }
    let output = command_output(&mut command);

    CompileResult { output, binary_path }
}

fn custom_abi_profile_path() -> PathBuf {
    let source_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../amice-vm/profiles/amice-simple-vmp")
        .canonicalize()
        .expect("built-in profile dir should exist");
    let profile_dir = output_dir().join("vm_virtualize_custom_abi_profile");
    std::fs::create_dir_all(&profile_dir).expect("custom profile dir should be creatable");

    for file in [
        "manifest.toml",
        "isa.vm",
        "lowering.vm",
        "bytecode.vm",
        "decoder.vm",
        "runtime.vm",
    ] {
        std::fs::copy(source_dir.join(file), profile_dir.join(file)).expect("profile file should be copyable");
    }

    let abi = std::fs::read_to_string(source_dir.join("abi.vm"))
        .expect("built-in ABI profile should be readable")
        .replace("arg0 -> x0 as i64", "arg0 -> x3 as i64")
        .replace("arg1 -> x1 as i64", "arg1 -> x4 as i64")
        .replace("arg2 -> x2 as i64", "arg2 -> x6 as i64")
        .replace("arg3 -> x3 as i64", "arg3 -> x7 as i64")
        .replace("arg4 -> x4 as i64", "arg4 -> x8 as i64")
        .replace("arg5 -> x5 as i64", "arg5 -> x9 as i64")
        .replace("arg6 -> x6 as i64", "arg6 -> x10 as i64")
        .replace("arg7 -> x7 as i64", "arg7 -> x11 as i64")
        .replace("ret0 <- x0 as i64", "ret0 <- x5 as i64")
        .replace("args = [x0..x7]", "args = [x8..x15]")
        .replace("returns = [x0, x1, x2]", "returns = [x16, x17, x18]")
        .replace("clobbers = [x0..x15]", "clobbers = [x8..x18]");
    std::fs::write(profile_dir.join("abi.vm"), abi).expect("custom ABI profile should be writable");

    profile_dir
}

fn handler_clone_profile_path() -> PathBuf {
    let source_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../amice-vm/profiles/amice-simple-vmp")
        .canonicalize()
        .expect("built-in profile dir should exist");
    let profile_dir = output_dir().join("vm_virtualize_handler_clone_profile");
    std::fs::create_dir_all(&profile_dir).expect("handler clone profile dir should be creatable");

    for file in [
        "manifest.toml",
        "abi.vm",
        "isa.vm",
        "lowering.vm",
        "bytecode.vm",
        "decoder.vm",
    ] {
        std::fs::copy(source_dir.join(file), profile_dir.join(file)).expect("profile file should be copyable");
    }

    let runtime = std::fs::read_to_string(source_dir.join("runtime.vm"))
        .expect("built-in runtime profile should be readable")
        .replace(
            "enhance handler_clone = disabled # 默认模块级 runtime 共享一套分派器，按需测试时再启用函数级克隆",
            "enhance handler_clone = func # 测试函数级 handler clone 语义",
        );
    std::fs::write(profile_dir.join("runtime.vm"), runtime).expect("handler clone runtime should be writable");

    profile_dir
}

fn module_bytecode_profile_path() -> PathBuf {
    let source_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../amice-vm/profiles/amice-simple-vmp")
        .canonicalize()
        .expect("built-in profile dir should exist");
    let profile_dir = output_dir().join("vm_virtualize_module_bytecode_profile");
    std::fs::create_dir_all(&profile_dir).expect("module bytecode profile dir should be creatable");

    for file in [
        "manifest.toml",
        "abi.vm",
        "isa.vm",
        "lowering.vm",
        "decoder.vm",
        "runtime.vm",
    ] {
        std::fs::copy(source_dir.join(file), profile_dir.join(file)).expect("profile file should be copyable");
    }

    let bytecode = std::fs::read_to_string(source_dir.join("bytecode.vm"))
        .expect("built-in bytecode profile should be readable")
        .replace(
            "bytecode.scope = func # 每个被保护函数拥有独立的字节码包和重定位表",
            "bytecode.scope = module # 测试同一 LLVM Module 内共享字节码全局容器",
        );
    std::fs::write(profile_dir.join("bytecode.vm"), bytecode).expect("module bytecode profile should be writable");

    profile_dir
}

fn decoder_variant_profile_path() -> PathBuf {
    let source_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../amice-vm/profiles/amice-simple-vmp")
        .canonicalize()
        .expect("built-in profile dir should exist");
    let profile_dir = output_dir().join("vm_virtualize_decoder_variant_profile");
    std::fs::create_dir_all(&profile_dir).expect("decoder variant profile dir should be creatable");

    for file in [
        "manifest.toml",
        "abi.vm",
        "isa.vm",
        "lowering.vm",
        "bytecode.vm",
        "runtime.vm",
    ] {
        std::fs::copy(source_dir.join(file), profile_dir.join(file)).expect("profile file should be copyable");
    }

    let decoder = std::fs::read_to_string(source_dir.join("decoder.vm"))
        .expect("built-in decoder profile should be readable")
        .replace(
            "step ror amount=3 # 第三步反转编译器侧左旋三位的编码",
            "step ror amount=5 # 测试 runtime 从 profile 读取 ror 旋转位数",
        )
        .replace(
            "step rol amount=1 # 第四步反转编译器侧右旋一位的编码",
            "step rol amount=2 # 测试 runtime 从 profile 读取 rol 旋转位数",
        );
    std::fs::write(profile_dir.join("decoder.vm"), decoder).expect("decoder variant profile should be writable");

    profile_dir
}

fn ruoke_profile_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../amice-vm/profiles/ruoke")
        .canonicalize()
        .expect("ruoke profile dir should exist")
}

fn handler_opcode_count(ir: &str) -> usize {
    ir.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '.'))
        .filter(|token| token.contains("handler."))
        .filter_map(|token| token.rsplit_once(".op").map(|(_, suffix)| suffix))
        .map(|suffix| {
            suffix
                .chars()
                .take_while(|ch| ch.is_ascii_hexdigit())
                .collect::<String>()
        })
        .filter(|suffix| !suffix.is_empty())
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

fn bytecode_dump_for_function<'a>(stderr: &'a str, function: &str) -> &'a str {
    let marker = format!("bytecode for \"{function}\":");
    let start = stderr
        .find(&marker)
        .unwrap_or_else(|| panic!("debug log should contain bytecode dump for {function}"));
    let rest = &stderr[start..];
    let after_marker = &rest[marker.len()..];
    if let Some(next) = after_marker.find("bytecode for \"") {
        &rest[..marker.len() + next]
    } else {
        rest
    }
}

fn semantic_renamed_add_profile_path() -> PathBuf {
    let source_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../amice-vm/profiles/amice-simple-vmp")
        .canonicalize()
        .expect("built-in profile dir should exist");
    let profile_dir = output_dir().join("vm_virtualize_semantic_renamed_add_profile");
    std::fs::create_dir_all(&profile_dir).expect("semantic renamed profile dir should be creatable");

    for file in ["manifest.toml", "abi.vm", "bytecode.vm", "decoder.vm", "runtime.vm"] {
        std::fs::copy(source_dir.join(file), profile_dir.join(file)).expect("profile file should be copyable");
    }

    let isa = std::fs::read_to_string(source_dir.join("isa.vm"))
        .expect("built-in ISA profile should be readable")
        .replacen("instr iadd", "instr add_alias", 1)
        .replacen("opcode alias [0x10, 0x2c, 0x5a, 0x6d, 0x7a]", "opcode alias [0x1f4]", 1);
    std::fs::write(profile_dir.join("isa.vm"), isa).expect("renamed ISA profile should be writable");

    let lowering = std::fs::read_to_string(source_dir.join("lowering.vm"))
        .expect("built-in lowering profile should be readable")
        .replace(
            "emit iadd dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # 发射 profile ISA 中的 iadd 指令",
            "emit add_alias dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # 发射改名后的加法指令",
        )
        .replace(
            "emit iadd dst=%vr, lhs=%vb, rhs=%vs, width=64 # 缩放偏移与基址相加",
            "emit add_alias dst=%vr, lhs=%vb, rhs=%vs, width=64 # 缩放偏移与基址相加",
        )
        .replace(
            "emit iadd dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # add 常量表达式发射 iadd 指令",
            "emit add_alias dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # add 常量表达式发射改名后的加法指令",
        )
        .replace(
            "sequence iadd, ixor # 只允许把连续的 iadd 与 ixor 两条 VM 指令融合",
            "sequence add_alias, ixor # 只允许把连续的改名加法与 ixor 两条 VM 指令融合",
        )
        .replace(
            "sequence load, iadd # 只允许把连续的 load 与 iadd 两条 VM 指令融合",
            "sequence load, add_alias # 只允许把连续的 load 与改名加法两条 VM 指令融合",
        );
    std::fs::write(profile_dir.join("lowering.vm"), lowering).expect("renamed lowering profile should be writable");

    profile_dir
}

fn same_semantic_alt_add_profile_path() -> PathBuf {
    let source_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../amice-vm/profiles/amice-simple-vmp")
        .canonicalize()
        .expect("built-in profile dir should exist");
    let profile_dir = output_dir().join("vm_virtualize_same_semantic_alt_add_profile");
    std::fs::create_dir_all(&profile_dir).expect("same-semantic profile dir should be creatable");

    for file in ["manifest.toml", "abi.vm", "bytecode.vm", "decoder.vm", "runtime.vm"] {
        std::fs::copy(source_dir.join(file), profile_dir.join(file)).expect("profile file should be copyable");
    }

    let alt_iadd = r#"instr iadd_alt(dst: vreg<i64>, lhs: vreg<i64>, rhs: vreg<i64>, width: imm<u8>) { # 第二条同语义整数加法处理器
opcode alias [0xfa] # iadd_alt 使用测试专用独立操作码，避免与内置原子操作别名冲突
semantic { # iadd_alt 保持与 iadd 相同的加法语义
reg[dst] = trunc_width(reg[lhs] + reg[rhs], width) # 加法结果按目标宽度掩码
pc = next # 执行继续到下一条字节码指令
} # 结束 iadd_alt 语义块
} # 结束 iadd_alt 指令
"#;
    let isa = std::fs::read_to_string(source_dir.join("isa.vm"))
        .expect("built-in ISA profile should be readable")
        .replace("instr isub", &format!("{alt_iadd}instr isub"));
    std::fs::write(profile_dir.join("isa.vm"), isa).expect("same-semantic ISA profile should be writable");

    let lowering = std::fs::read_to_string(source_dir.join("lowering.vm"))
        .expect("built-in lowering profile should be readable")
        .replace(
            "emit iadd dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # 发射 profile ISA 中的 iadd 指令",
            "emit iadd_alt dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # 发射第二条同语义加法处理器",
        );
    std::fs::write(profile_dir.join("lowering.vm"), lowering)
        .expect("same-semantic lowering profile should be writable");

    profile_dir
}

fn reordered_add_operands_profile_path() -> PathBuf {
    let source_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../amice-vm/profiles/amice-simple-vmp")
        .canonicalize()
        .expect("built-in profile dir should exist");
    let profile_dir = output_dir().join("vm_virtualize_reordered_add_operands_profile");
    std::fs::create_dir_all(&profile_dir).expect("reordered operand profile dir should be creatable");

    for file in [
        "manifest.toml",
        "abi.vm",
        "bytecode.vm",
        "decoder.vm",
        "runtime.vm",
        "lowering.vm",
    ] {
        std::fs::copy(source_dir.join(file), profile_dir.join(file)).expect("profile file should be copyable");
    }

    let isa = std::fs::read_to_string(source_dir.join("isa.vm")).expect("built-in ISA profile should be readable");
    let isa = isa.replace(
        "instr iadd(dst: vreg<i64>, lhs: vreg<i64>, rhs: vreg<i64>, width: imm<u8>) { # 整数加法处理器",
        "instr iadd(width: imm<u8>, rhs: vreg<i64>, dst: vreg<i64>, lhs: vreg<i64>) { # 整数加法处理器",
    );
    std::fs::write(profile_dir.join("isa.vm"), isa).expect("reordered ISA profile should be writable");

    profile_dir
}

fn renamed_add_lowering_rule_profile_path() -> PathBuf {
    let source_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../amice-vm/profiles/amice-simple-vmp")
        .canonicalize()
        .expect("built-in profile dir should exist");
    let profile_dir = output_dir().join("vm_virtualize_renamed_add_lowering_rule_profile");
    std::fs::create_dir_all(&profile_dir).expect("renamed lowering-rule profile dir should be creatable");

    for file in [
        "manifest.toml",
        "abi.vm",
        "isa.vm",
        "bytecode.vm",
        "decoder.vm",
        "runtime.vm",
    ] {
        std::fs::copy(source_dir.join(file), profile_dir.join(file)).expect("profile file should be copyable");
    }

    let lowering =
        std::fs::read_to_string(source_dir.join("lowering.vm")).expect("built-in lowering profile should be readable");
    let lowering = lowering.replacen("rule llvm.add.integer", "rule custom.add.integer", 1);
    std::fs::write(profile_dir.join("lowering.vm"), lowering)
        .expect("renamed lowering-rule profile should be writable");

    profile_dir
}

fn missing_add_lowering_profile_path() -> PathBuf {
    let source_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../amice-vm/profiles/amice-simple-vmp")
        .canonicalize()
        .expect("built-in profile dir should exist");
    let profile_dir = output_dir().join("vm_virtualize_missing_add_lowering_profile");
    std::fs::create_dir_all(&profile_dir).expect("missing lowering profile dir should be creatable");

    for file in [
        "manifest.toml",
        "abi.vm",
        "isa.vm",
        "bytecode.vm",
        "decoder.vm",
        "runtime.vm",
    ] {
        std::fs::copy(source_dir.join(file), profile_dir.join(file)).expect("profile file should be copyable");
    }

    let lowering =
        std::fs::read_to_string(source_dir.join("lowering.vm")).expect("built-in lowering profile should be readable");
    std::fs::write(
        profile_dir.join("lowering.vm"),
        remove_lowering_rule(&lowering, "llvm.add.integer"),
    )
    .expect("missing-add lowering profile should be writable");

    profile_dir
}

fn fixed_env_without_actions_profile_path() -> PathBuf {
    let source_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../amice-vm/profiles/amice-simple-vmp")
        .canonicalize()
        .expect("built-in profile dir should exist");
    let profile_dir = output_dir().join("vm_virtualize_fixed_env_without_actions_profile");
    std::fs::create_dir_all(&profile_dir).expect("fixed-env profile dir should be creatable");

    for file in [
        "manifest.toml",
        "abi.vm",
        "isa.vm",
        "bytecode.vm",
        "decoder.vm",
        "runtime.vm",
    ] {
        std::fs::copy(source_dir.join(file), profile_dir.join(file)).expect("profile file should be copyable");
    }

    let lowering =
        std::fs::read_to_string(source_dir.join("lowering.vm")).expect("built-in lowering profile should be readable");
    let lowering = lowering.replace(
        r#"    %va = materialize %a as integer # 将左操作数物化为 VM 整数值
    %vb = materialize %b as integer # 将右操作数物化为 VM 整数值
    %vr = vreg integer # 为 add 结果分配一个 VM x 寄存器
"#,
        "",
    );
    std::fs::write(profile_dir.join("lowering.vm"), lowering).expect("fixed-env lowering profile should be writable");

    profile_dir
}

fn add_lowering_as_sub_profile_path() -> PathBuf {
    let source_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../amice-vm/profiles/amice-simple-vmp")
        .canonicalize()
        .expect("built-in profile dir should exist");
    let profile_dir = output_dir().join("vm_virtualize_add_lowering_as_sub_profile");
    std::fs::create_dir_all(&profile_dir).expect("add-as-sub profile dir should be creatable");

    for file in [
        "manifest.toml",
        "abi.vm",
        "isa.vm",
        "bytecode.vm",
        "decoder.vm",
        "runtime.vm",
    ] {
        std::fs::copy(source_dir.join(file), profile_dir.join(file)).expect("profile file should be copyable");
    }

    let lowering =
        std::fs::read_to_string(source_dir.join("lowering.vm")).expect("built-in lowering profile should be readable");
    let lowering = lowering.replace(
        "emit iadd dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # 发射 profile ISA 中的 iadd 指令",
        "emit isub dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # 测试 lowering action 可将 LLVM add 改派到减法处理器",
    );
    std::fs::write(profile_dir.join("lowering.vm"), lowering).expect("add-as-sub lowering profile should be writable");

    profile_dir
}

fn sub_materialize_swapped_profile_path() -> PathBuf {
    let source_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../amice-vm/profiles/amice-simple-vmp")
        .canonicalize()
        .expect("built-in profile dir should exist");
    let profile_dir = output_dir().join("vm_virtualize_sub_materialize_swapped_profile");
    std::fs::create_dir_all(&profile_dir).expect("materialize-swap profile dir should be creatable");

    for file in [
        "manifest.toml",
        "abi.vm",
        "isa.vm",
        "bytecode.vm",
        "decoder.vm",
        "runtime.vm",
    ] {
        std::fs::copy(source_dir.join(file), profile_dir.join(file)).expect("profile file should be copyable");
    }

    let lowering =
        std::fs::read_to_string(source_dir.join("lowering.vm")).expect("built-in lowering profile should be readable");
    let sub_rule = r#"rule llvm.sub.integer { # 将 LLVM 整数 sub 降低为 isub VM 指令
  match %r = llvm.sub integer %a, %b # 匹配任意受支持整数宽度的 LLVM sub
  lower { # 开始声明 sub 的 lowering 动作
    %va = materialize %a as integer # 将左操作数物化为 VM 整数值
    %vb = materialize %b as integer # 将右操作数物化为 VM 整数值
    %vr = vreg integer # 为 sub 结果分配一个 VM x 寄存器
    emit isub dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # 发射 profile ISA 中的 isub 指令
    bind %r = %vr # 记录 LLVM 结果到 VM 寄存器的绑定
  } # 结束 sub lowering 动作
} # 结束 sub 规则"#;
    let swapped_sub_rule = r#"rule llvm.sub.integer { # 将 LLVM 整数 sub 降低为 isub VM 指令
  match %r = llvm.sub integer %a, %b # 匹配任意受支持整数宽度的 LLVM sub
  lower { # 开始声明 sub 的 lowering 动作
    %va = materialize %b as integer # 测试 materialize source 可将右操作数作为 lhs
    %vb = materialize %a as integer # 测试 materialize source 可将左操作数作为 rhs
    %vr = vreg integer # 为 sub 结果分配一个 VM x 寄存器
    emit isub dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # 发射 profile ISA 中的 isub 指令
    bind %r = %vr # 记录 LLVM 结果到 VM 寄存器的绑定
  } # 结束 sub lowering 动作
} # 结束 sub 规则"#;
    std::fs::write(
        profile_dir.join("lowering.vm"),
        lowering.replace(sub_rule, swapped_sub_rule),
    )
    .expect("materialize-swap lowering profile should be writable");

    profile_dir
}

fn add_bind_to_lhs_profile_path() -> PathBuf {
    let source_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../amice-vm/profiles/amice-simple-vmp")
        .canonicalize()
        .expect("built-in profile dir should exist");
    let profile_dir = output_dir().join("vm_virtualize_add_bind_to_lhs_profile");
    std::fs::create_dir_all(&profile_dir).expect("bind-to-lhs profile dir should be creatable");

    for file in [
        "manifest.toml",
        "abi.vm",
        "isa.vm",
        "bytecode.vm",
        "decoder.vm",
        "runtime.vm",
    ] {
        std::fs::copy(source_dir.join(file), profile_dir.join(file)).expect("profile file should be copyable");
    }

    let lowering =
        std::fs::read_to_string(source_dir.join("lowering.vm")).expect("built-in lowering profile should be readable");
    let add_rule = r#"rule llvm.add.integer { # 将 LLVM 整数 add 降低为 iadd VM 指令
  match %r = llvm.add integer %a, %b # 匹配任意受支持整数宽度的 LLVM add
  lower { # 开始声明 add 的 lowering 动作
    %va = materialize %a as integer # 将左操作数物化为 VM 整数值
    %vb = materialize %b as integer # 将右操作数物化为 VM 整数值
    %vr = vreg integer # 为 add 结果分配一个 VM x 寄存器
    emit iadd dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # 发射 profile ISA 中的 iadd 指令
    bind %r = %vr # 记录 LLVM 结果到 VM 寄存器的绑定
  } # 结束 add lowering 动作
} # 结束 add 规则"#;
    let rebound_add_rule = r#"rule llvm.add.integer { # 将 LLVM 整数 add 降低为 iadd VM 指令
  match %r = llvm.add integer %a, %b # 匹配任意受支持整数宽度的 LLVM add
  lower { # 开始声明 add 的 lowering 动作
    %va = materialize %a as integer # 将左操作数物化为 VM 整数值
    %vb = materialize %b as integer # 将右操作数物化为 VM 整数值
    %vr = vreg integer # 为 add 结果分配一个 VM x 寄存器
    emit iadd dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # 发射 profile ISA 中的 iadd 指令
    bind %r = %va # 测试 bind action 可将 LLVM add 结果重绑定到左操作数
  } # 结束 add lowering 动作
} # 结束 add 规则"#;
    let lowering = lowering.replace(add_rule, rebound_add_rule);
    std::fs::write(profile_dir.join("lowering.vm"), lowering).expect("bind-to-lhs lowering profile should be writable");

    profile_dir
}

fn add_vreg_renamed_profile_path() -> PathBuf {
    let source_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../amice-vm/profiles/amice-simple-vmp")
        .canonicalize()
        .expect("built-in profile dir should exist");
    let profile_dir = output_dir().join("vm_virtualize_add_vreg_renamed_profile");
    std::fs::create_dir_all(&profile_dir).expect("renamed-vreg profile dir should be creatable");

    for file in [
        "manifest.toml",
        "abi.vm",
        "isa.vm",
        "bytecode.vm",
        "decoder.vm",
        "runtime.vm",
    ] {
        std::fs::copy(source_dir.join(file), profile_dir.join(file)).expect("profile file should be copyable");
    }

    let lowering =
        std::fs::read_to_string(source_dir.join("lowering.vm")).expect("built-in lowering profile should be readable");
    let add_rule = r#"rule llvm.add.integer { # 将 LLVM 整数 add 降低为 iadd VM 指令
match %r = llvm.add integer %a, %b # 匹配任意受支持整数宽度的 LLVM add
lower { # 开始声明 add 的 lowering 动作
%va = materialize %a as integer # 将左操作数物化为 VM 整数值
%vb = materialize %b as integer # 将右操作数物化为 VM 整数值
%vr = vreg integer # 为 add 结果分配一个 VM x 寄存器
emit iadd dst=%vr, lhs=%va, rhs=%vb, width=type_width(%r) # 发射 profile ISA 中的 iadd 指令
bind %r = %vr # 记录 LLVM 结果到 VM 寄存器的绑定
} # 结束 add lowering 动作
} # 结束 add 规则"#;
    let renamed_vreg_add_rule = r#"rule llvm.add.integer { # 将 LLVM 整数 add 降低为 iadd VM 指令
match %r = llvm.add integer %a, %b # 匹配任意受支持整数宽度的 LLVM add
lower { # 开始声明 add 的 lowering 动作
%va = materialize %a as integer # 将左操作数物化为 VM 整数值
%vb = materialize %b as integer # 将右操作数物化为 VM 整数值
%vx = vreg integer # 测试 vreg action 可声明自定义 VM 结果寄存器变量
emit iadd dst=%vx, lhs=%va, rhs=%vb, width=type_width(%r) # 发射 profile ISA 中的 iadd 指令
bind %r = %vx # 记录 LLVM 结果到自定义 vreg 变量
} # 结束 add lowering 动作
} # 结束 add 规则"#;
    std::fs::write(
        profile_dir.join("lowering.vm"),
        lowering.replace(add_rule, renamed_vreg_add_rule),
    )
    .expect("renamed-vreg lowering profile should be writable");

    profile_dir
}

fn call_native_callee_literal_profile_path() -> PathBuf {
    let source_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../amice-vm/profiles/amice-simple-vmp")
        .canonicalize()
        .expect("built-in profile dir should exist");
    let profile_dir = output_dir().join("vm_virtualize_call_native_callee_literal_profile");
    std::fs::create_dir_all(&profile_dir).expect("call-native literal profile dir should be creatable");

    for file in [
        "manifest.toml",
        "abi.vm",
        "isa.vm",
        "bytecode.vm",
        "decoder.vm",
        "runtime.vm",
    ] {
        std::fs::copy(source_dir.join(file), profile_dir.join(file)).expect("profile file should be copyable");
    }

    let lowering =
        std::fs::read_to_string(source_dir.join("lowering.vm")).expect("built-in lowering profile should be readable");
    let lowering = lowering.replace(
        "emit call_native callee=native_id(%callee), argc=arg_count(%callee), arg0=arg0, ret_count=return_count(%callee) # 发射原生调用桥指令",
        "emit call_native callee=1, argc=arg_count(%callee), arg0=arg0, ret_count=return_count(%callee) # 测试 call_native record 由 lowering emit 表达式控制",
    );
    std::fs::write(profile_dir.join("lowering.vm"), lowering)
        .expect("call-native literal lowering profile should be writable");

    profile_dir
}

fn remove_lowering_rule(source: &str, rule_name: &str) -> String {
    let mut output = Vec::new();
    let mut skipping = false;
    let mut depth = 0_i32;

    for line in source.lines() {
        let semantic = line.split('#').next().unwrap_or_default().trim();
        if !skipping && semantic.starts_with(&format!("rule {rule_name} ")) {
            skipping = true;
            depth += semantic.matches('{').count() as i32;
            depth -= semantic.matches('}').count() as i32;
            continue;
        }

        if skipping {
            depth += semantic.matches('{').count() as i32;
            depth -= semantic.matches('}').count() as i32;
            if depth == 0 {
                skipping = false;
            }
            continue;
        }

        output.push(line);
    }

    output.join("\n")
}

fn command_output(command: &mut Command) -> Output {
    command.output().expect("test command should run")
}

fn assert_success(output: Output) {
    if output.status.success() {
        return;
    }

    panic!(
        "command failed\nSTDOUT:\n{}\nSTDERR:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[serial]
fn test_vm_virtualize_basic_c_matches_baseline() {
    ensure_plugin_built();

    let source = fixture_path("vm_virtualize", "basic.c", Language::C);
    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized = compile_virtualized_binary(&source, "vm_virtualize_basic");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();

    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());
}

#[test]
#[serial]
fn test_vm_virtualize_integer_div_rem_matches_baseline() {
    ensure_plugin_built();

    let source = output_dir().join("vm_virtualize_div_rem.c");
    std::fs::write(
        &source,
        r#"#include <stdio.h>

#define VMP __attribute__((noinline, annotate("+vm_virtualize")))

VMP int vm_divrem(int a, int b) {
    unsigned ua = (unsigned)(a * 17 + 1234);
    unsigned ub = (unsigned)(b | 1);
    int sa = a - 200;
    int sb = b | 1;
    unsigned uq = ua / ub;
    unsigned ur = ua % ub;
    int sq = sa / sb;
    int sr = sa % sb;
    return (int)((uq ^ ur) + (unsigned)(sq * 3 + sr));
}

int main(void) {
    int acc = 0;
    acc += vm_divrem(45, 7);
    acc += vm_divrem(123, 11);
    acc += vm_divrem(255, 13);
    printf("%d\n", acc);
    return 0;
}
"#,
    )
    .expect("div/rem fixture should be writable");
    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_div_rem_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized = compile_virtualized_binary(&source, "vm_virtualize_div_rem");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir_path = compile_virtualized_ir(&source, "vm_virtualize_div_rem.ll");
    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_divrem"));
    assert!(ir.contains("handler.iudiv"));
    assert!(ir.contains("handler.isdiv"));
    assert!(ir.contains("handler.iurem"));
    assert!(ir.contains("handler.isrem"));
}

#[test]
#[serial]
fn test_vm_virtualize_super_add_xor_fusion_matches_baseline() {
    ensure_plugin_built();

    let source = output_dir().join("vm_virtualize_super_add_xor.c");
    std::fs::write(
        &source,
        r#"#include <stdio.h>

#define VMP __attribute__((noinline, annotate("+vm_virtualize")))

VMP int vm_add_xor(int a, int b, int c) {
    int sum = a + b;
    return sum ^ c;
}

int main(void) {
    int acc = 0;
    acc += vm_add_xor(5, 7, 0x33);
    acc += vm_add_xor(100, 23, 0x55);
    acc += vm_add_xor(-9, 41, 0x7f);
    printf("%d\n", acc);
    return 0;
}
"#,
    )
    .expect("super add/xor fixture should be writable");
    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_super_add_xor_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized = compile_virtualized_binary(&source, "vm_virtualize_super_add_xor");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir_path = compile_virtualized_ir(&source, "vm_virtualize_super_add_xor.ll");
    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_add_xor"));
    assert!(ir.contains("handler.iadd_xor"));
}

#[test]
#[serial]
fn test_vm_virtualize_super_icmp_br_if_fusion_matches_baseline() {
    ensure_plugin_built();

    let source = output_dir().join("vm_virtualize_super_icmp_br_if.c");
    std::fs::write(
        &source,
        r#"#include <stdio.h>

#define VMP __attribute__((noinline, annotate("+vm_virtualize")))

VMP int vm_cmp_branch(int a, int b) {
    int acc = 0;
    if (a < b) {
        for (int i = 0; i < a; ++i) {
            acc += (i ^ b) + 3;
        }
    } else {
        for (int i = 0; i < b; ++i) {
            acc += (i + a) ^ 7;
        }
    }
    return acc;
}

int main(void) {
    int acc = 0;
    acc += vm_cmp_branch(3, 9);
    acc += vm_cmp_branch(11, 4);
    acc += vm_cmp_branch(5, 5);
    printf("%d\n", acc);
    return 0;
}
"#,
    )
    .expect("super icmp/br fixture should be writable");
    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_super_icmp_br_if_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized = compile_virtualized_binary(&source, "vm_virtualize_super_icmp_br_if");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir_path = compile_virtualized_ir(&source, "vm_virtualize_super_icmp_br_if.ll");
    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_cmp_branch"));
    assert!(ir.contains("handler.icmp_br_if"));
}

#[test]
#[serial]
fn test_vm_virtualize_super_gep_load_fusion_matches_baseline() {
    ensure_plugin_built();

    let source = output_dir().join("vm_virtualize_super_gep_load.c");
    std::fs::write(
        &source,
        r#"#include <stdio.h>

#define VMP __attribute__((noinline, annotate("+vm_virtualize")))

struct Pair {
    int left;
    int right;
};

VMP int vm_gep_load_field(const struct Pair *pair) {
    return pair->right;
}

int main(void) {
    struct Pair values[3] = {
        {1, 17},
        {3, 29},
        {5, 41},
    };
    int acc = 0;
    acc += vm_gep_load_field(&values[0]);
    acc += vm_gep_load_field(&values[1]);
    acc += vm_gep_load_field(&values[2]);
    printf("%d\n", acc);
    return 0;
}
"#,
    )
    .expect("super gep/load fixture should be writable");
    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_super_gep_load_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized = compile_virtualized_binary(&source, "vm_virtualize_super_gep_load");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir_path = compile_virtualized_ir(&source, "vm_virtualize_super_gep_load.ll");
    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_gep_load_field"));
    assert!(ir.contains("handler.gep_load"));
}

#[test]
#[serial]
fn test_vm_virtualize_super_load_add_fusion_matches_baseline() {
    ensure_plugin_built();

    let source = output_dir().join("vm_virtualize_super_load_iadd.c");
    std::fs::write(
        &source,
        r#"#include <stdio.h>

#define VMP __attribute__((noinline, annotate("+vm_virtualize")))

VMP int vm_load_add(const int *ptr, int addend) {
    return *ptr + addend;
}

int main(void) {
    int values[3] = {13, 21, 34};
    int acc = 0;
    acc += vm_load_add(&values[0], 5);
    acc += vm_load_add(&values[1], 8);
    acc += vm_load_add(&values[2], 13);
    printf("%d\n", acc);
    return 0;
}
"#,
    )
    .expect("super load/add fixture should be writable");
    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_super_load_iadd_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized = compile_virtualized_binary(&source, "vm_virtualize_super_load_iadd");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir_path = compile_virtualized_ir(&source, "vm_virtualize_super_load_iadd.ll");
    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_load_add"));
    assert!(ir.contains("handler.load_iadd"));
}

#[test]
#[serial]
fn test_vm_virtualize_freeze_scalar_ir_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_freeze_scalar.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_freeze_scalar'
source_filename = "vm_virtualize_freeze_scalar.ll"

define i32 @vm_freeze_scalar(i32 %a, i32 %b) {
entry:
  %sum = add i32 %a, %b
  %stable = freeze i32 %sum
  %cmp = icmp sgt i32 %stable, 20
  %chosen = select i1 %cmp, i32 %stable, i32 %b
  %mixed = xor i32 %chosen, %a
  %frozen_poison = freeze i32 poison
  %frozen_undef = freeze i32 undef
  %cancel_poison = sub i32 %frozen_poison, %frozen_poison
  %cancel_undef = xor i32 %frozen_undef, %frozen_undef
  %with_poison = add i32 %mixed, %cancel_poison
  %with_undef = add i32 %with_poison, %cancel_undef
  ret i32 %with_undef
}
"#,
    )
    .expect("freeze LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_freeze_scalar_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdio.h>
int vm_freeze_scalar(int a, int b);
int main(void) {
    int acc = 0;
    acc += vm_freeze_scalar(7, 5);
    acc += vm_freeze_scalar(20, 9);
    acc += vm_freeze_scalar(-3, 41);
    printf("%d\n", acc);
    return 0;
}
"#,
    )
    .expect("freeze C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_freeze_scalar_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized_ir = optimize_ir_with_plugin(&ir_source, "vm_virtualize_freeze_scalar.ll", vm_virtualize_config());
    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_freeze_scalar");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized freeze LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_freeze_scalar"));
    assert!(ir.contains("handler.mov"));
    assert!(!ir.contains(" freeze "));
}

#[test]
#[serial]
fn test_vm_virtualize_freeze_float_scalar_ir_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_freeze_float_scalar.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_freeze_float_scalar'
source_filename = "vm_virtualize_freeze_float_scalar.ll"

define i64 @vm_freeze_float_scalar(float %f, double %d, double %e) {
entry:
  %ff = freeze float %f
  %fd = freeze double %d
  %fs = fadd float %ff, 1.250000e+00
  %ds = fsub double %fd, %e
  %fi = bitcast float %fs to i32
  %di = bitcast double %ds to i64
  %fi64 = zext i32 %fi to i64
  %low = and i64 %di, 4294967295
  %mix = xor i64 %fi64, %low
  ret i64 %mix
}
"#,
    )
    .expect("float freeze LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_freeze_float_scalar_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_freeze_float_scalar(float f, double d, double e);

int main(void) {
    uint64_t a = vm_freeze_float_scalar(3.5f, 8.25, 2.5);
    uint64_t b = vm_freeze_float_scalar(-2.75f, -5.5, -3.125);
    printf("%llu %llu %llu\n", (unsigned long long)a, (unsigned long long)b, (unsigned long long)(a ^ b));
    return 0;
}
"#,
    )
    .expect("float freeze C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_freeze_float_scalar_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_freeze_float_scalar.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_freeze_float_scalar");
    assert!(
        dump.contains(": mov "),
        "float/double freeze should lower through the profile mov rule:\n{dump}"
    );
    assert!(
        dump.contains(": fadd ") && dump.contains(": fsub "),
        "fixture should exercise frozen float and double values through VM float handlers:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_freeze_float_scalar");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized float freeze LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_freeze_float_scalar"));
    assert!(ir.contains("handler.mov"));
    assert!(ir.contains("handler.fadd"));
    assert!(ir.contains("handler.fsub"));
    assert!(!ir.contains(" freeze "));
}

#[test]
#[serial]
fn test_vm_virtualize_addrspacecast_ir_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_addrspacecast.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_addrspacecast'
source_filename = "vm_virtualize_addrspacecast.ll"

define i64 @vm_addrspacecast_roundtrip(ptr %p) {
entry:
  %as1 = addrspacecast ptr %p to ptr addrspace(1)
  %back = addrspacecast ptr addrspace(1) %as1 to ptr
  %bits = ptrtoint ptr %back to i64
  ret i64 %bits
}
"#,
    )
    .expect("addrspacecast LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_addrspacecast_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

long vm_addrspacecast_roundtrip(int *p);

int main(void) {
    int value = 123;
    long bits = vm_addrspacecast_roundtrip(&value);
    printf("%d\n", bits != 0);
    return 0;
}
"#,
    )
    .expect("addrspacecast C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_addrspacecast_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized_ir = optimize_ir_with_plugin(&ir_source, "vm_virtualize_addrspacecast.ll", vm_virtualize_config());
    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_addrspacecast");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized addrspacecast LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_addrspacecast_roundtrip"));
    assert!(ir.contains("handler.bitcast"));
}

#[test]
#[serial]
fn test_vm_virtualize_scalar_bitcast_reinterpret_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_scalar_bitcast.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_scalar_bitcast'
source_filename = "vm_virtualize_scalar_bitcast.ll"

define i64 @vm_scalar_bitcast_reinterpret(i32 %fbits, i64 %dbits) {
entry:
  %f = bitcast i32 %fbits to float
  %d = bitcast i64 %dbits to double
  %fs = fadd float %f, 1.500000e+00
  %ds = fmul double %d, -2.000000e+00
  %fo = bitcast float %fs to i32
  %do = bitcast double %ds to i64
  %fo64 = zext i32 %fo to i64
  %dlow = and i64 %do, 4294967295
  %mix = xor i64 %fo64, %dlow
  ret i64 %mix
}
"#,
    )
    .expect("scalar bitcast LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_scalar_bitcast_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_scalar_bitcast_reinterpret(uint32_t fbits, uint64_t dbits);

int main(void) {
    uint64_t a = vm_scalar_bitcast_reinterpret(0x40400000u, 0x4002000000000000ULL);
    uint64_t b = vm_scalar_bitcast_reinterpret(0xc0200000u, 0xbff8000000000000ULL);
    printf("%llu %llu %llu\n", (unsigned long long)a, (unsigned long long)b, (unsigned long long)(a ^ b));
    return 0;
}
"#,
    )
    .expect("scalar bitcast C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_scalar_bitcast_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) = optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_scalar_bitcast.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_scalar_bitcast_reinterpret");
    assert!(
        dump.contains(": bitcast "),
        "scalar bitcast should lower through the profile bitcast rule:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_scalar_bitcast");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized scalar bitcast LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_scalar_bitcast_reinterpret"));
    assert!(ir.contains("handler.bitcast"));
}

#[test]
#[serial]
fn test_vm_virtualize_integer_intrinsics_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_integer_intrinsics.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_integer_intrinsics'
source_filename = "vm_virtualize_integer_intrinsics.ll"

declare i32 @llvm.ctpop.i32(i32)
declare i64 @llvm.ctpop.i64(i64)
declare i32 @llvm.ctlz.i32(i32, i1 immarg)
declare i64 @llvm.ctlz.i64(i64, i1 immarg)
declare i32 @llvm.cttz.i32(i32, i1 immarg)
declare i64 @llvm.cttz.i64(i64, i1 immarg)
declare i32 @llvm.abs.i32(i32, i1 immarg)
declare i64 @llvm.abs.i64(i64, i1 immarg)
declare i32 @llvm.smax.i32(i32, i32)
declare i32 @llvm.smin.i32(i32, i32)
declare i32 @llvm.umax.i32(i32, i32)
declare i32 @llvm.umin.i32(i32, i32)
declare i64 @llvm.smax.i64(i64, i64)
declare i64 @llvm.smin.i64(i64, i64)
declare i64 @llvm.umax.i64(i64, i64)
declare i64 @llvm.umin.i64(i64, i64)
declare i32 @llvm.uadd.sat.i32(i32, i32)
declare i32 @llvm.usub.sat.i32(i32, i32)
declare i32 @llvm.sadd.sat.i32(i32, i32)
declare i32 @llvm.ssub.sat.i32(i32, i32)
declare i64 @llvm.uadd.sat.i64(i64, i64)
declare i64 @llvm.usub.sat.i64(i64, i64)
declare i64 @llvm.sadd.sat.i64(i64, i64)
declare i64 @llvm.ssub.sat.i64(i64, i64)
declare i32 @llvm.ushl.sat.i32(i32, i32)
declare i32 @llvm.sshl.sat.i32(i32, i32)
declare i64 @llvm.ushl.sat.i64(i64, i64)
declare i64 @llvm.sshl.sat.i64(i64, i64)
declare { i32, i1 } @llvm.uadd.with.overflow.i32(i32, i32)
declare { i32, i1 } @llvm.sadd.with.overflow.i32(i32, i32)
declare { i32, i1 } @llvm.usub.with.overflow.i32(i32, i32)
declare { i32, i1 } @llvm.ssub.with.overflow.i32(i32, i32)
declare { i64, i1 } @llvm.uadd.with.overflow.i64(i64, i64)
declare { i64, i1 } @llvm.sadd.with.overflow.i64(i64, i64)
declare { i64, i1 } @llvm.usub.with.overflow.i64(i64, i64)
declare { i64, i1 } @llvm.ssub.with.overflow.i64(i64, i64)
declare { i32, i1 } @llvm.umul.with.overflow.i32(i32, i32)
declare { i32, i1 } @llvm.smul.with.overflow.i32(i32, i32)
declare { i64, i1 } @llvm.umul.with.overflow.i64(i64, i64)
declare { i64, i1 } @llvm.smul.with.overflow.i64(i64, i64)
declare i32 @llvm.bswap.i32(i32)
declare i64 @llvm.bswap.i64(i64)
declare i32 @llvm.bitreverse.i32(i32)
declare i64 @llvm.bitreverse.i64(i64)
declare i32 @llvm.fshl.i32(i32, i32, i32)
declare i64 @llvm.fshl.i64(i64, i64, i64)
declare i32 @llvm.fshr.i32(i32, i32, i32)
declare i64 @llvm.fshr.i64(i64, i64, i64)

define i64 @vm_integer_intrinsics(i32 %a, i64 %b) {
entry:
  %pop32 = call i32 @llvm.ctpop.i32(i32 %a)
  %lz32 = call i32 @llvm.ctlz.i32(i32 %a, i1 false)
  %tz32 = call i32 @llvm.cttz.i32(i32 %a, i1 false)
  %lz32_zero = call i32 @llvm.ctlz.i32(i32 0, i1 false)
  %tz32_zero = call i32 @llvm.cttz.i32(i32 0, i1 false)
  %neg32 = sub i32 0, %a
  %abs32 = call i32 @llvm.abs.i32(i32 %neg32, i1 false)
  %abs32_min = call i32 @llvm.abs.i32(i32 -2147483648, i1 false)
  %smax32 = call i32 @llvm.smax.i32(i32 %neg32, i32 12345)
  %smin32 = call i32 @llvm.smin.i32(i32 %neg32, i32 -12345)
  %umax32 = call i32 @llvm.umax.i32(i32 %neg32, i32 %a)
  %umin32 = call i32 @llvm.umin.i32(i32 %neg32, i32 %a)
  %swap32 = call i32 @llvm.bswap.i32(i32 %a)
  %rev32 = call i32 @llvm.bitreverse.i32(i32 %a)
  %fshl32 = call i32 @llvm.fshl.i32(i32 %a, i32 %swap32, i32 %pop32)
  %fshr32 = call i32 @llvm.fshr.i32(i32 %swap32, i32 %a, i32 %pop32)
  %pop64 = call i64 @llvm.ctpop.i64(i64 %b)
  %lz64 = call i64 @llvm.ctlz.i64(i64 %b, i1 false)
  %tz64 = call i64 @llvm.cttz.i64(i64 %b, i1 false)
  %lz64_zero = call i64 @llvm.ctlz.i64(i64 0, i1 false)
  %tz64_zero = call i64 @llvm.cttz.i64(i64 0, i1 false)
  %neg64 = sub i64 0, %b
  %abs64 = call i64 @llvm.abs.i64(i64 %neg64, i1 false)
  %smax64 = call i64 @llvm.smax.i64(i64 %neg64, i64 123456789)
  %smin64 = call i64 @llvm.smin.i64(i64 %neg64, i64 -123456789)
  %umax64 = call i64 @llvm.umax.i64(i64 %neg64, i64 %b)
  %umin64 = call i64 @llvm.umin.i64(i64 %neg64, i64 %b)
  %swap64 = call i64 @llvm.bswap.i64(i64 %b)
  %rev64 = call i64 @llvm.bitreverse.i64(i64 %b)
  %fshl64 = call i64 @llvm.fshl.i64(i64 %b, i64 %swap64, i64 %pop64)
  %fshr64 = call i64 @llvm.fshr.i64(i64 %swap64, i64 %b, i64 %pop64)
  %pop32x = zext i32 %pop32 to i64
  %lz32x = zext i32 %lz32 to i64
  %tz32x = zext i32 %tz32 to i64
  %lz32zx = zext i32 %lz32_zero to i64
  %tz32zx = zext i32 %tz32_zero to i64
  %abs32x = zext i32 %abs32 to i64
  %abs32minx = zext i32 %abs32_min to i64
  %smax32x = zext i32 %smax32 to i64
  %smin32x = zext i32 %smin32 to i64
  %umax32x = zext i32 %umax32 to i64
  %umin32x = zext i32 %umin32 to i64
  %swap32x = zext i32 %swap32 to i64
  %rev32x = zext i32 %rev32 to i64
  %fshl32x = zext i32 %fshl32 to i64
  %fshr32x = zext i32 %fshr32 to i64
  %a0 = xor i64 %pop32x, %swap32x
  %a1 = add i64 %a0, %rev32x
  %a2 = add i64 %a1, %lz32x
  %a3 = xor i64 %a2, %tz32x
  %a4 = add i64 %a3, %lz32zx
  %a5 = xor i64 %a4, %tz32zx
  %a6 = xor i64 %a5, %pop64
  %a7 = add i64 %a6, %lz64
  %a8 = xor i64 %a7, %tz64
  %a9 = add i64 %a8, %lz64_zero
  %a10 = xor i64 %a9, %tz64_zero
  %a11 = add i64 %a10, %abs32x
  %a12 = xor i64 %a11, %abs32minx
  %a13 = add i64 %a12, %smax32x
  %a14 = xor i64 %a13, %smin32x
  %a15 = add i64 %a14, %umax32x
  %a16 = xor i64 %a15, %umin32x
  %a17 = add i64 %a16, %abs64
  %a18 = xor i64 %a17, %smax64
  %a19 = add i64 %a18, %smin64
  %a20 = xor i64 %a19, %umax64
  %a21 = add i64 %a20, %umin64
  %a22 = add i64 %a21, %swap64
  %a23 = xor i64 %a22, %rev64
  %a24 = add i64 %a23, %fshl32x
  %a25 = xor i64 %a24, %fshr32x
  %a26 = add i64 %a25, %fshl64
  %a27 = xor i64 %a26, %fshr64
  ret i64 %a27
}

define i64 @vm_integer_saturating_intrinsics(i32 %a, i64 %b) {
entry:
  %uaddsat32 = call i32 @llvm.uadd.sat.i32(i32 %a, i32 4026531840)
  %usubsat32 = call i32 @llvm.usub.sat.i32(i32 %a, i32 4026531840)
  %saddsat32 = call i32 @llvm.sadd.sat.i32(i32 2147483647, i32 %a)
  %ssubsat32 = call i32 @llvm.ssub.sat.i32(i32 -2147483648, i32 %a)
  %uaddsat64 = call i64 @llvm.uadd.sat.i64(i64 %b, i64 8070450532247928832)
  %usubsat64 = call i64 @llvm.usub.sat.i64(i64 %b, i64 8070450532247928832)
  %saddsat64 = call i64 @llvm.sadd.sat.i64(i64 9223372036854775807, i64 %b)
  %ssubsat64 = call i64 @llvm.ssub.sat.i64(i64 -9223372036854775808, i64 %b)
  %uaddsat32x = zext i32 %uaddsat32 to i64
  %usubsat32x = zext i32 %usubsat32 to i64
  %saddsat32x = zext i32 %saddsat32 to i64
  %ssubsat32x = zext i32 %ssubsat32 to i64
  %r0 = add i64 %uaddsat32x, %uaddsat64
  %r1 = xor i64 %r0, %usubsat32x
  %r2 = add i64 %r1, %usubsat64
  %r3 = xor i64 %r2, %saddsat32x
  %r4 = add i64 %r3, %saddsat64
  %r5 = xor i64 %r4, %ssubsat32x
  %r6 = add i64 %r5, %ssubsat64
  ret i64 %r6
}

define i64 @vm_integer_saturating_shift_intrinsics(i32 %a, i64 %b, i32 %s) {
entry:
  %neg32 = sub i32 0, %a
  %neg64 = sub i64 0, %b
  %shift64 = zext i32 %s to i64
  %ushlsat32 = call i32 @llvm.ushl.sat.i32(i32 %a, i32 %s)
  %sshlsat32 = call i32 @llvm.sshl.sat.i32(i32 %neg32, i32 %s)
  %ushlsat64 = call i64 @llvm.ushl.sat.i64(i64 %b, i64 %shift64)
  %sshlsat64 = call i64 @llvm.sshl.sat.i64(i64 %neg64, i64 %shift64)
  %ushlsat32x = zext i32 %ushlsat32 to i64
  %sshlsat32x = zext i32 %sshlsat32 to i64
  %r0 = add i64 %ushlsat32x, %ushlsat64
  %r1 = xor i64 %r0, %sshlsat32x
  %r2 = add i64 %r1, %sshlsat64
  ret i64 %r2
}

define i64 @vm_integer_overflow_intrinsics(i32 %a, i64 %b) {
entry:
  %uadd32 = call { i32, i1 } @llvm.uadd.with.overflow.i32(i32 %a, i32 -16)
  %uadd32v = extractvalue { i32, i1 } %uadd32, 0
  %uadd32o = extractvalue { i32, i1 } %uadd32, 1
  %uadd32x = zext i32 %uadd32v to i64
  %uadd32ox = zext i1 %uadd32o to i64
  %r0 = xor i64 %uadd32x, %uadd32ox
  %sadd32 = call { i32, i1 } @llvm.sadd.with.overflow.i32(i32 2147483647, i32 %a)
  %sadd32v = extractvalue { i32, i1 } %sadd32, 0
  %sadd32o = extractvalue { i32, i1 } %sadd32, 1
  %sadd32x = zext i32 %sadd32v to i64
  %sadd32ox = zext i1 %sadd32o to i64
  %r1 = add i64 %r0, %sadd32x
  %r2 = xor i64 %r1, %sadd32ox
  %usub32 = call { i32, i1 } @llvm.usub.with.overflow.i32(i32 %a, i32 -268435456)
  %usub32v = extractvalue { i32, i1 } %usub32, 0
  %usub32o = extractvalue { i32, i1 } %usub32, 1
  %usub32x = zext i32 %usub32v to i64
  %usub32ox = zext i1 %usub32o to i64
  %r3 = add i64 %r2, %usub32x
  %r4 = xor i64 %r3, %usub32ox
  %ssub32 = call { i32, i1 } @llvm.ssub.with.overflow.i32(i32 -2147483648, i32 %a)
  %ssub32v = extractvalue { i32, i1 } %ssub32, 0
  %ssub32o = extractvalue { i32, i1 } %ssub32, 1
  %ssub32x = zext i32 %ssub32v to i64
  %ssub32ox = zext i1 %ssub32o to i64
  %r5 = add i64 %r4, %ssub32x
  %r6 = xor i64 %r5, %ssub32ox
  %uadd64 = call { i64, i1 } @llvm.uadd.with.overflow.i64(i64 %b, i64 -16)
  %uadd64v = extractvalue { i64, i1 } %uadd64, 0
  %uadd64o = extractvalue { i64, i1 } %uadd64, 1
  %uadd64ox = zext i1 %uadd64o to i64
  %r7 = add i64 %r6, %uadd64v
  %r8 = xor i64 %r7, %uadd64ox
  %sadd64 = call { i64, i1 } @llvm.sadd.with.overflow.i64(i64 9223372036854775807, i64 %b)
  %sadd64v = extractvalue { i64, i1 } %sadd64, 0
  %sadd64o = extractvalue { i64, i1 } %sadd64, 1
  %sadd64ox = zext i1 %sadd64o to i64
  %r9 = add i64 %r8, %sadd64v
  %r10 = xor i64 %r9, %sadd64ox
  %usub64 = call { i64, i1 } @llvm.usub.with.overflow.i64(i64 %b, i64 -1152921504606846976)
  %usub64v = extractvalue { i64, i1 } %usub64, 0
  %usub64o = extractvalue { i64, i1 } %usub64, 1
  %usub64ox = zext i1 %usub64o to i64
  %r11 = add i64 %r10, %usub64v
  %r12 = xor i64 %r11, %usub64ox
  %ssub64 = call { i64, i1 } @llvm.ssub.with.overflow.i64(i64 -9223372036854775808, i64 %b)
  %ssub64v = extractvalue { i64, i1 } %ssub64, 0
  %ssub64o = extractvalue { i64, i1 } %ssub64, 1
  %ssub64ox = zext i1 %ssub64o to i64
  %r13 = add i64 %r12, %ssub64v
  %r14 = xor i64 %r13, %ssub64ox
  %umul32 = call { i32, i1 } @llvm.umul.with.overflow.i32(i32 %a, i32 536870912)
  %umul32v = extractvalue { i32, i1 } %umul32, 0
  %umul32o = extractvalue { i32, i1 } %umul32, 1
  %umul32x = zext i32 %umul32v to i64
  %umul32ox = zext i1 %umul32o to i64
  %r15 = add i64 %r14, %umul32x
  %r16 = xor i64 %r15, %umul32ox
  %smul32 = call { i32, i1 } @llvm.smul.with.overflow.i32(i32 %a, i32 1073741824)
  %smul32v = extractvalue { i32, i1 } %smul32, 0
  %smul32o = extractvalue { i32, i1 } %smul32, 1
  %smul32x = zext i32 %smul32v to i64
  %smul32ox = zext i1 %smul32o to i64
  %r17 = add i64 %r16, %smul32x
  %r18 = xor i64 %r17, %smul32ox
  %umul64 = call { i64, i1 } @llvm.umul.with.overflow.i64(i64 %b, i64 1152921504606846976)
  %umul64v = extractvalue { i64, i1 } %umul64, 0
  %umul64o = extractvalue { i64, i1 } %umul64, 1
  %umul64ox = zext i1 %umul64o to i64
  %r19 = add i64 %r18, %umul64v
  %r20 = xor i64 %r19, %umul64ox
  %smul64 = call { i64, i1 } @llvm.smul.with.overflow.i64(i64 %b, i64 4611686018427387904)
  %smul64v = extractvalue { i64, i1 } %smul64, 0
  %smul64o = extractvalue { i64, i1 } %smul64, 1
  %smul64ox = zext i1 %smul64o to i64
  %r21 = add i64 %r20, %smul64v
  %r22 = xor i64 %r21, %smul64ox
  ret i64 %r22
}
"#,
    )
    .expect("integer intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_integer_intrinsics_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_integer_intrinsics(uint32_t a, uint64_t b);
uint64_t vm_integer_saturating_intrinsics(uint32_t a, uint64_t b);
uint64_t vm_integer_saturating_shift_intrinsics(uint32_t a, uint64_t b, uint32_t s);
uint64_t vm_integer_overflow_intrinsics(uint32_t a, uint64_t b);

int main(void) {
    uint64_t result = vm_integer_intrinsics(0x12345678u, 0x0123456789abcdefULL);
    result ^= vm_integer_saturating_intrinsics(0x12345678u, 0x0123456789abcdefULL);
    result ^= vm_integer_saturating_shift_intrinsics(0x12345678u, 0x0123456789abcdefULL, 37u);
    result ^= vm_integer_overflow_intrinsics(0x12345678u, 0x0123456789abcdefULL);
    printf("%llu\n", (unsigned long long)result);
    return 0;
}
"#,
    )
    .expect("integer intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_integer_intrinsics_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized_ir = optimize_ir_with_plugin(
        &ir_source,
        "vm_virtualize_integer_intrinsics.ll",
        vm_virtualize_config(),
    );
    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_integer_intrinsics");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized integer intrinsic LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_integer_intrinsics"));
    assert!(ir.contains(".amice.vm.bytecode.vm_integer_saturating_intrinsics"));
    assert!(ir.contains(".amice.vm.bytecode.vm_integer_saturating_shift_intrinsics"));
    assert!(ir.contains(".amice.vm.bytecode.vm_integer_overflow_intrinsics"));
    assert!(ir.contains("handler.ctpop"));
    assert!(ir.contains("handler.ctlz"));
    assert!(ir.contains("handler.cttz"));
    assert!(ir.contains("handler.iabs"));
    assert!(ir.contains("handler.ismax"));
    assert!(ir.contains("handler.ismin"));
    assert!(ir.contains("handler.iumax"));
    assert!(ir.contains("handler.iumin"));
    assert!(ir.contains("handler.iuadd_sat"));
    assert!(ir.contains("handler.iusub_sat"));
    assert!(ir.contains("handler.isadd_sat"));
    assert!(ir.contains("handler.issub_sat"));
    assert!(ir.contains("handler.iushl_sat"));
    assert!(ir.contains("handler.isshl_sat"));
    assert!(ir.contains("handler.iuadd_overflow"));
    assert!(ir.contains("handler.isadd_overflow"));
    assert!(ir.contains("handler.iusub_overflow"));
    assert!(ir.contains("handler.issub_overflow"));
    assert!(ir.contains("handler.iumul_overflow"));
    assert!(ir.contains("handler.ismul_overflow"));
    assert!(ir.contains("handler.bswap"));
    assert!(ir.contains("handler.bitreverse"));
    assert!(ir.contains("handler.fshl"));
    assert!(ir.contains("handler.fshr"));
}

#[test]
#[serial]
fn test_vm_virtualize_readcounter_intrinsics_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_readcounter_intrinsics.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_readcounter_intrinsics'
source_filename = "vm_virtualize_readcounter_intrinsics.ll"

declare i64 @llvm.readcyclecounter()
declare i64 @llvm.readsteadycounter()

define i64 @vm_read_counter_intrinsics(i64 %seed) {
entry:
  %cycle = call i64 @llvm.readcyclecounter()
  %steady = call i64 @llvm.readsteadycounter()
  %cycle_zero = xor i64 %cycle, %cycle
  %steady_zero = sub i64 %steady, %steady
  %combined = xor i64 %cycle_zero, %steady_zero
  %result = xor i64 %combined, %seed
  ret i64 %result
}
"#,
    )
    .expect("readcounter LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_readcounter_intrinsics_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_read_counter_intrinsics(uint64_t seed);

int main(void) {
    uint64_t a = vm_read_counter_intrinsics(0x123456789abcdef0ULL);
    uint64_t b = vm_read_counter_intrinsics(0x0fedcba987654321ULL);
    printf("%llu %llu\n", (unsigned long long)a, (unsigned long long)b);
    return 0;
}
"#,
    )
    .expect("readcounter C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_readcounter_intrinsics_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) = optimize_ir_with_plugin_debug_pipeline(
        &ir_source,
        "vm_virtualize_readcounter_intrinsics.ll",
        "default<O0>",
        config,
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_read_counter_intrinsics");
    assert!(
        dump.contains(": read_cycle "),
        "readcyclecounter should lower through profile read_cycle:\n{dump}"
    );
    assert!(
        dump.contains(": read_steady "),
        "readsteadycounter should lower through profile read_steady:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_readcounter_intrinsics");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized readcounter LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_read_counter_intrinsics"));
    assert!(ir.contains("handler.read_cycle"));
    assert!(ir.contains("handler.read_steady"));
    assert!(ir.contains("@llvm.readcyclecounter"));
    assert!(ir.contains("@llvm.readsteadycounter"));
}

#[test]
#[serial]
fn test_vm_virtualize_aggregate_overflow_registers_are_reused() {
    ensure_plugin_built();

    let pair_count = 24;
    let ir_source = output_dir().join("vm_virtualize_overflow_register_reuse.input.ll");
    let mut ir = String::from(
        r#"; ModuleID = 'vm_virtualize_overflow_register_reuse'
source_filename = "vm_virtualize_overflow_register_reuse.ll"

declare { i32, i1 } @llvm.uadd.with.overflow.i32(i32, i32)

define i64 @vm_many_overflow_pairs(i32 %seed) {
entry:
  %cur0 = xor i32 %seed, 305419896
  %acc0 = zext i32 %cur0 to i64
"#,
    );
    for index in 0..pair_count {
        let next = index + 1;
        let addend = 1009 + index * 37;
        let mixer = 17 + index * 101;
        ir.push_str(&format!(
            "  %pair{index} = call {{ i32, i1 }} @llvm.uadd.with.overflow.i32(i32 %cur{index}, i32 {addend})\n\
  %value{index} = extractvalue {{ i32, i1 }} %pair{index}, 0\n\
  %flag{index} = extractvalue {{ i32, i1 }} %pair{index}, 1\n\
  %value64_{index} = zext i32 %value{index} to i64\n\
  %flag64_{index} = zext i1 %flag{index} to i64\n\
  %mix{index} = xor i64 %acc{index}, %value64_{index}\n\
  %acc{next} = add i64 %mix{index}, %flag64_{index}\n\
  %cur{next} = xor i32 %value{index}, {mixer}\n"
        ));
    }
    ir.push_str(&format!("  ret i64 %acc{pair_count}\n}}\n"));
    std::fs::write(&ir_source, ir).expect("overflow register reuse LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_overflow_register_reuse_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_many_overflow_pairs(uint32_t seed);

int main(void) {
    uint64_t a = vm_many_overflow_pairs(0x12345678u);
    uint64_t b = vm_many_overflow_pairs(0xfedcba98u);
    printf("%llu %llu\n", (unsigned long long)a, (unsigned long long)b);
    return 0;
}
"#,
    )
    .expect("overflow register reuse C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_overflow_register_reuse_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_overflow_register_reuse.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_many_overflow_pairs");
    let overflow_count = dump.matches(": iuadd_overflow ").count();
    assert!(
        overflow_count >= pair_count,
        "all overflow pairs should virtualize without exhausting aggregate field registers:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_overflow_register_reuse");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized overflow reuse IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_many_overflow_pairs"));
    assert!(ir.contains("handler.iuadd_overflow"));
}

#[test]
#[serial]
fn test_vm_virtualize_expect_intrinsics_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_expect_intrinsics.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_expect_intrinsics'
source_filename = "vm_virtualize_expect_intrinsics.ll"

declare i1 @llvm.expect.i1(i1, i1)
declare i32 @llvm.expect.i32(i32, i32)
declare i32 @llvm.expect.with.probability.i32(i32, i32, double immarg)

define i32 @vm_expect_intrinsics(i32 %seed) {
entry:
  %cond = icmp sgt i32 %seed, 0
  %likely = call i1 @llvm.expect.i1(i1 %cond, i1 true)
  br i1 %likely, label %positive, label %negative

positive:
  %hinted = call i32 @llvm.expect.with.probability.i32(i32 %seed, i32 305419896, double 8.750000e-01)
  %mixed = xor i32 %hinted, 324508639
  ret i32 %mixed

negative:
  %neg = sub i32 0, %seed
  %hinted_neg = call i32 @llvm.expect.i32(i32 %neg, i32 7)
  %mixed_neg = add i32 %hinted_neg, 17
  ret i32 %mixed_neg
}
"#,
    )
    .expect("expect intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_expect_intrinsics_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdio.h>
int vm_expect_intrinsics(int seed);
int main(void) {
    int a = vm_expect_intrinsics(0x13572468);
    int b = vm_expect_intrinsics(-37);
    printf("%d %d\n", a, b);
    return 0;
}
"#,
    )
    .expect("expect intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_expect_intrinsics_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized_ir =
        optimize_ir_with_plugin(&ir_source, "vm_virtualize_expect_intrinsics.ll", vm_virtualize_config());
    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_expect_intrinsics");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized expect LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_expect_intrinsics"));
    assert!(ir.contains("handler.mov"));
}

#[test]
#[serial]
fn test_vm_virtualize_ssa_copy_intrinsics_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_ssa_copy_intrinsics.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_ssa_copy_intrinsics'
source_filename = "vm_virtualize_ssa_copy_intrinsics.ll"

declare i32 @llvm.ssa.copy.i32(i32)
declare ptr @llvm.ssa.copy.p0(ptr)
declare float @llvm.ssa.copy.f32(float)
declare double @llvm.ssa.copy.f64(double)

define i64 @vm_ssa_copy_intrinsics(i32 %seed, ptr %cell, float %fv, double %dv) {
entry:
  %copied_i = call i32 @llvm.ssa.copy.i32(i32 %seed)
  %copied_p = call ptr @llvm.ssa.copy.p0(ptr %cell)
  %copied_f = call float @llvm.ssa.copy.f32(float %fv)
  %copied_d = call double @llvm.ssa.copy.f64(double %dv)
  %loaded = load i32, ptr %copied_p, align 4
  %float_i = fptosi float %copied_f to i32
  %double_i = fptosi double %copied_d to i64
  %seed64 = sext i32 %copied_i to i64
  %loaded64 = sext i32 %loaded to i64
  %float64 = sext i32 %float_i to i64
  %mix0 = add i64 %seed64, %loaded64
  %mix1 = shl i64 %float64, 8
  %mix2 = xor i64 %mix0, %mix1
  %mix3 = add i64 %mix2, %double_i
  ret i64 %mix3
}
"#,
    )
    .expect("ssa.copy intrinsic LLVM IR fixture should be writable");

    let baseline_ir = output_dir().join("vm_virtualize_ssa_copy_intrinsics.baseline.ll");
    std::fs::write(
        &baseline_ir,
        r#"; ModuleID = 'vm_virtualize_ssa_copy_intrinsics_baseline'
source_filename = "vm_virtualize_ssa_copy_intrinsics_baseline.ll"

define i64 @vm_ssa_copy_intrinsics(i32 %seed, ptr %cell, float %fv, double %dv) {
entry:
  %loaded = load i32, ptr %cell, align 4
  %float_i = fptosi float %fv to i32
  %double_i = fptosi double %dv to i64
  %seed64 = sext i32 %seed to i64
  %loaded64 = sext i32 %loaded to i64
  %float64 = sext i32 %float_i to i64
  %mix0 = add i64 %seed64, %loaded64
  %mix1 = shl i64 %float64, 8
  %mix2 = xor i64 %mix0, %mix1
  %mix3 = add i64 %mix2, %double_i
  ret i64 %mix3
}
"#,
    )
    .expect("ssa.copy baseline LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_ssa_copy_intrinsics_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>
int64_t vm_ssa_copy_intrinsics(int32_t seed, int32_t *cell, float fv, double dv);
int main(void) {
    int32_t cell = 0x13572468;
    int64_t a = vm_ssa_copy_intrinsics(17, &cell, 12.75f, 4096.25);
    cell = -99;
    int64_t b = vm_ssa_copy_intrinsics(-33, &cell, -8.5f, -123.0);
    printf("%lld %lld\n", (long long)a, (long long)b);
    return 0;
}
"#,
    )
    .expect("ssa.copy intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(&baseline_ir, &harness, "vm_virtualize_ssa_copy_intrinsics_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) = optimize_ir_with_plugin_debug_pipeline(
        &ir_source,
        "vm_virtualize_ssa_copy_intrinsics.ll",
        "default<O0>",
        config,
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_ssa_copy_intrinsics");
    assert!(
        dump.matches(": mov ").count() >= 4,
        "ssa.copy scalar values should lower through profile mov actions:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_ssa_copy_intrinsics");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized ssa.copy LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_ssa_copy_intrinsics"));
    assert!(ir.contains("handler.mov"));
    assert!(!ir.contains(".amice.vm.original.vm_ssa_copy_intrinsics"));
}

#[test]
#[serial]
fn test_vm_virtualize_invariant_group_intrinsics_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_invariant_group_intrinsics.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_invariant_group_intrinsics'
source_filename = "vm_virtualize_invariant_group_intrinsics.ll"

declare ptr @llvm.launder.invariant.group.p0(ptr)
declare ptr @llvm.strip.invariant.group.p0(ptr)

define i32 @vm_invariant_group_intrinsics(ptr %base, i32 %index) {
entry:
  %idx64 = sext i32 %index to i64
  %laundered = call ptr @llvm.launder.invariant.group.p0(ptr %base)
  %stripped = call ptr @llvm.strip.invariant.group.p0(ptr %laundered)
  %slot = getelementptr inbounds i32, ptr %stripped, i64 %idx64
  %value = load i32, ptr %slot, align 4
  %mix = xor i32 %value, %index
  ret i32 %mix
}
"#,
    )
    .expect("invariant-group intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_invariant_group_intrinsics_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_invariant_group_intrinsics(int32_t *base, int32_t index);

int main(void) {
    int32_t values[4] = {0x13572468, -37, 0x10203040, -123456};
    int32_t a = vm_invariant_group_intrinsics(values, 0);
    int32_t b = vm_invariant_group_intrinsics(values, 2);
    printf("%d\n", (int)(a ^ b));
    return 0;
}
"#,
    )
    .expect("invariant-group intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(
        &ir_source,
        &harness,
        "vm_virtualize_invariant_group_intrinsics_baseline",
    );
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized_ir = optimize_ir_with_plugin(
        &ir_source,
        "vm_virtualize_invariant_group_intrinsics.ll",
        vm_virtualize_config(),
    );
    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_invariant_group_intrinsics");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized invariant-group LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_invariant_group_intrinsics"));
    assert!(ir.contains("handler.mov"));
    assert!(!ir.contains("@llvm.launder.invariant.group"));
    assert!(!ir.contains("@llvm.strip.invariant.group"));
}

#[test]
#[serial]
fn test_vm_virtualize_invariant_start_end_intrinsics_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_invariant_start_end_intrinsics.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_invariant_start_end_intrinsics'
source_filename = "vm_virtualize_invariant_start_end_intrinsics.ll"

declare ptr @llvm.invariant.start.p0(i64 immarg, ptr nocapture)
declare void @llvm.invariant.end.p0(ptr, i64 immarg, ptr nocapture)

define i32 @vm_invariant_start_end_intrinsics(ptr %base, i32 %seed) {
entry:
  %descriptor = call ptr @llvm.invariant.start.p0(i64 4, ptr %base)
  %value = load i32, ptr %base, align 4
  %mixed = xor i32 %value, %seed
  call void @llvm.invariant.end.p0(ptr %descriptor, i64 4, ptr %base)
  ret i32 %mixed
}
"#,
    )
    .expect("invariant.start/end intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_invariant_start_end_intrinsics_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_invariant_start_end_intrinsics(int32_t *base, int32_t seed);

int main(void) {
    int32_t values[2] = {0x13572468, -12345};
    int32_t a = vm_invariant_start_end_intrinsics(values, 0x24681357);
    int32_t b = vm_invariant_start_end_intrinsics(values + 1, -77);
    printf("%d\n", (int)(a ^ b));
    return 0;
}
"#,
    )
    .expect("invariant.start/end intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(
        &ir_source,
        &harness,
        "vm_virtualize_invariant_start_end_intrinsics_baseline",
    );
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_invariant_start_end_intrinsics.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_invariant_start_end_intrinsics");
    assert!(
        dump.contains(": mov "),
        "invariant.start should lower through mov:\n{dump}"
    );
    assert!(
        dump.contains(": fake_nop "),
        "invariant.end should lower through fake_nop:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(
        &virtualized_ir,
        &harness,
        "vm_virtualize_invariant_start_end_intrinsics",
    );
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir =
        std::fs::read_to_string(virtualized_ir).expect("virtualized invariant.start/end LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_invariant_start_end_intrinsics"));
    assert!(!ir.contains("@llvm.invariant.start"));
    assert!(!ir.contains("@llvm.invariant.end"));
}

#[test]
#[serial]
fn test_vm_virtualize_metadata_nop_intrinsics_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_metadata_nop_intrinsics.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_metadata_nop_intrinsics'
source_filename = "vm_virtualize_metadata_nop_intrinsics.ll"

declare void @llvm.prefetch.p0(ptr, i32 immarg, i32 immarg, i32 immarg)
declare void @llvm.experimental.noalias.scope.decl(metadata)
declare void @llvm.donothing()

define i32 @vm_metadata_nop_intrinsics(ptr %base, i32 %seed) {
entry:
  call void @llvm.experimental.noalias.scope.decl(metadata !0)
  call void @llvm.prefetch.p0(ptr %base, i32 0, i32 3, i32 1)
  call void @llvm.donothing()
  %value = load i32, ptr %base, align 4, !alias.scope !0
  %mixed = xor i32 %value, %seed
  ret i32 %mixed
}

!0 = !{!2}
!1 = distinct !{!1, !"vm_nop_domain"}
!2 = distinct !{!2, !1, !"vm_nop_scope"}
"#,
    )
    .expect("metadata nop intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_metadata_nop_intrinsics_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_metadata_nop_intrinsics(int32_t *base, int32_t seed);

int main(void) {
    int32_t values[2] = {0x10203040, -9999};
    int32_t a = vm_metadata_nop_intrinsics(values, 0x13572468);
    int32_t b = vm_metadata_nop_intrinsics(values + 1, -37);
    printf("%d\n", (int)(a ^ b));
    return 0;
}
"#,
    )
    .expect("metadata nop intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_metadata_nop_intrinsics_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_metadata_nop_intrinsics.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_metadata_nop_intrinsics");
    let fake_nops = dump.matches(": fake_nop ").count();
    assert!(
        fake_nops >= 3,
        "prefetch/noalias.scope.decl/donothing should lower through fake_nop:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_metadata_nop_intrinsics");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized metadata nop LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_metadata_nop_intrinsics"));
    assert!(!ir.contains("@llvm.prefetch"));
    assert!(!ir.contains("@llvm.experimental.noalias.scope.decl"));
    assert!(!ir.contains("@llvm.donothing"));
}

#[test]
#[serial]
fn test_vm_virtualize_ptrmask_intrinsic_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_ptrmask_intrinsic.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_ptrmask_intrinsic'
source_filename = "vm_virtualize_ptrmask_intrinsic.ll"

declare ptr @llvm.ptrmask.p0.i64(ptr, i64)

define i32 @vm_ptrmask_intrinsic(ptr %base, i64 %mask, i32 %seed) {
entry:
  %masked = call ptr @llvm.ptrmask.p0.i64(ptr %base, i64 %mask)
  %value = load i32, ptr %masked, align 4
  %mixed = xor i32 %value, %seed
  ret i32 %mixed
}
"#,
    )
    .expect("ptrmask intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_ptrmask_intrinsic_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_ptrmask_intrinsic(uint8_t *base, int64_t mask, int32_t seed);

int main(void) {
    int32_t values[4] = {0x10203040, -9999, 0x13572468, -77};
    int32_t a = vm_ptrmask_intrinsic(((uint8_t *)&values[1]) + 1, -4LL, 0x24681357);
    int32_t b = vm_ptrmask_intrinsic(((uint8_t *)&values[2]) + 2, -4LL, -37);
    printf("%d\n", (int)(a ^ b));
    return 0;
}
"#,
    )
    .expect("ptrmask intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_ptrmask_intrinsic_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_ptrmask_intrinsic.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_ptrmask_intrinsic");
    assert!(
        dump.contains(": ptrmask "),
        "ptrmask intrinsic should lower through the profile ptrmask instruction:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_ptrmask_intrinsic");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized ptrmask LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_ptrmask_intrinsic"));
    assert!(ir.contains("handler.ptrmask"));
    assert!(!ir.contains("@llvm.ptrmask"));
}

#[test]
#[serial]
fn test_vm_virtualize_threadlocal_address_intrinsic_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_threadlocal_address_intrinsic.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_threadlocal_address_intrinsic'
source_filename = "vm_virtualize_threadlocal_address_intrinsic.ll"

@vm_tls_value = thread_local global i32 270544960, align 4

declare ptr @llvm.threadlocal.address.p0(ptr)

define i32 @vm_threadlocal_address_intrinsic(i32 %seed) {
entry:
  %addr = call ptr @llvm.threadlocal.address.p0(ptr @vm_tls_value)
  %value = load i32, ptr %addr, align 4
  %mixed = xor i32 %value, %seed
  store i32 %mixed, ptr %addr, align 4
  ret i32 %mixed
}
"#,
    )
    .expect("threadlocal.address intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_threadlocal_address_intrinsic_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_threadlocal_address_intrinsic(int32_t seed);

int main(void) {
    int32_t a = vm_threadlocal_address_intrinsic(0x13572468);
    int32_t b = vm_threadlocal_address_intrinsic(-37);
    printf("%d %d %d\n", (int)a, (int)b, (int)(a ^ b));
    return 0;
}
"#,
    )
    .expect("threadlocal.address intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(
        &ir_source,
        &harness,
        "vm_virtualize_threadlocal_address_intrinsic_baseline",
    );
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_threadlocal_address_intrinsic.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_threadlocal_address_intrinsic");
    assert!(
        dump.contains(": tls_addr "),
        "threadlocal.address should lower through the profile tls_addr instruction:\n{dump}"
    );

    let virtualized =
        compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_threadlocal_address_intrinsic");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir =
        std::fs::read_to_string(virtualized_ir).expect("virtualized threadlocal.address LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_threadlocal_address_intrinsic"));
    assert!(ir.contains(".amice.vm.tls_addr.vm_threadlocal_address_intrinsic"));
    assert!(ir.contains("handler.tls_addr"));
}

#[test]
#[serial]
fn test_vm_virtualize_global_pointer_operands_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_global_pointer.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_global_pointer'
source_filename = "vm_virtualize_global_pointer.ll"

@vm_global_counter = global i32 270544960, align 4
@vm_global_values = global [4 x i32] [i32 7, i32 -11, i32 12345, i32 -9876], align 16
@vm_addrspace_counter = addrspace(1) global i32 -889275714, align 4

define i32 @vm_global_pointer(i32 %seed) {
entry:
  %old = load i32, ptr @vm_global_counter, align 4
  %slot = getelementptr inbounds [4 x i32], ptr @vm_global_values, i64 0, i64 2
  %extra = load i32, ptr %slot, align 4
  %mixed0 = xor i32 %old, %seed
  %mixed1 = add i32 %mixed0, %extra
  store i32 %mixed1, ptr @vm_global_counter, align 4
  ret i32 %mixed1
}

define i32 @vm_global_pointer_cast_expr(i32 %seed) {
entry:
  %old = load i32, ptr addrspacecast (ptr addrspace(1) @vm_addrspace_counter to ptr), align 4
  %mixed = xor i32 %old, %seed
  store i32 %mixed, ptr addrspacecast (ptr addrspace(1) @vm_addrspace_counter to ptr), align 4
  ret i32 %mixed
}
"#,
    )
    .expect("global pointer LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_global_pointer_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_global_pointer(int32_t seed);
int32_t vm_global_pointer_cast_expr(int32_t seed);

int main(void) {
    int32_t a = vm_global_pointer(0x13572468);
    int32_t b = vm_global_pointer(-37);
    int32_t c = vm_global_pointer_cast_expr(0x2468ace0);
    int32_t d = vm_global_pointer_cast_expr(91);
    printf("%d %d %d %d %d\n", (int)a, (int)b, (int)c, (int)d, (int)(a ^ b ^ c ^ d));
    return 0;
}
"#,
    )
    .expect("global pointer C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_global_pointer_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) = optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_global_pointer.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_global_pointer");
    assert!(
        dump.contains(": global_addr "),
        "global pointer constants should lower through the profile global_addr instruction:\n{dump}"
    );
    let cast_dump = bytecode_dump_for_function(&stderr, "vm_global_pointer_cast_expr");
    assert!(
        cast_dump.contains(": global_addr "),
        "pointer cast constant expressions should recurse into global_addr materialization:\n{cast_dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_global_pointer");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized global pointer LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_global_pointer"));
    assert!(ir.contains(".amice.vm.bytecode.vm_global_pointer_cast_expr"));
    assert!(ir.contains(".amice.vm.global_addr.vm_global_pointer"));
    assert!(ir.contains(".amice.vm.global_addr.vm_global_pointer_cast_expr"));
    assert!(ir.contains("handler.global_addr"));
}

#[test]
#[serial]
fn test_vm_virtualize_pointer_constant_expressions_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_pointer_constexpr.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_pointer_constexpr'
source_filename = "vm_virtualize_pointer_constexpr.ll"

@vm_constexpr_global = global i32 3405691582, align 4

declare i64 @vm_constexpr_i64_sink(i64)
declare i64 @vm_constexpr_ptr_sink(ptr)

define i64 @vm_pointer_constexpr(i64 %salt) {
entry:
  %global_bits = call i64 @vm_constexpr_i64_sink(i64 ptrtoint (ptr @vm_constexpr_global to i64))
  %literal_ptr = call i64 @vm_constexpr_ptr_sink(ptr inttoptr (i64 4096 to ptr))
  %sum = add i64 %global_bits, %literal_ptr
  %mixed = xor i64 %sum, %salt
  ret i64 %mixed
}
"#,
    )
    .expect("pointer constant expression LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_pointer_constexpr_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int64_t vm_pointer_constexpr(int64_t salt);

int64_t vm_constexpr_i64_sink(int64_t value) {
    return value != 0 ? 11 : -11;
}

int64_t vm_constexpr_ptr_sink(void *value) {
    return value == (void *)(uintptr_t)4096 ? 7 : -7;
}

int main(void) {
    int64_t a = vm_pointer_constexpr(0x12345678);
    int64_t b = vm_pointer_constexpr(-77);
    printf("%lld %lld %lld\n", (long long)a, (long long)b, (long long)(a ^ b));
    return 0;
}
"#,
    )
    .expect("pointer constant expression C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_pointer_constexpr_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_pointer_constexpr.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_pointer_constexpr");
    assert!(
        dump.contains(": global_addr "),
        "ptrtoint global constant expression should materialize the global address through profile:\n{dump}"
    );
    assert!(
        dump.contains(": bitcast "),
        "ptrtoint/inttoptr constant expressions should lower through profile cast handlers:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_pointer_constexpr");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir =
        std::fs::read_to_string(virtualized_ir).expect("virtualized pointer constant expression IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_pointer_constexpr"));
    assert!(ir.contains(".amice.vm.global_addr.vm_pointer_constexpr"));
    assert!(ir.contains("handler.call_native"));
    assert!(ir.contains("handler.bitcast"));
}

#[test]
#[serial]
fn test_vm_virtualize_integer_constant_expressions_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_integer_constexpr.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_integer_constexpr'
source_filename = "vm_virtualize_integer_constexpr.ll"

@vm_integer_constexpr_values = global [4 x i32] [i32 11, i32 22, i32 33, i32 44], align 16

declare i64 @vm_constexpr_i64_sink(i64)

define i64 @vm_integer_constexpr_ops(i64 %salt) {
entry:
  %loaded = load i32, ptr inttoptr (i64 add (i64 ptrtoint (ptr @vm_integer_constexpr_values to i64), i64 4) to ptr), align 4
  %bits0 = call i64 @vm_constexpr_i64_sink(i64 xor (i64 ptrtoint (ptr @vm_integer_constexpr_values to i64), i64 4660))
  %bits1 = call i64 @vm_constexpr_i64_sink(i64 add (i64 ptrtoint (ptr @vm_integer_constexpr_values to i64), i64 17))
  %bits2 = call i64 @vm_constexpr_i64_sink(i64 sub (i64 ptrtoint (ptr @vm_integer_constexpr_values to i64), i64 9))
  %loaded64 = sext i32 %loaded to i64
  %sum0 = add i64 %bits0, %bits1
  %sum1 = add i64 %sum0, %bits2
  %sum2 = add i64 %sum1, %loaded64
  %mixed = xor i64 %sum2, %salt
  ret i64 %mixed
}
"#,
    )
    .expect("integer constant expression LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_integer_constexpr_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int64_t vm_integer_constexpr_ops(int64_t salt);

int64_t vm_constexpr_i64_sink(int64_t value) {
    (void)value;
    return 23;
}

int main(void) {
    int64_t a = vm_integer_constexpr_ops(0x12345678);
    int64_t b = vm_integer_constexpr_ops(-77);
    printf("%lld %lld %lld\n", (long long)a, (long long)b, (long long)(a ^ b));
    return 0;
}
"#,
    )
    .expect("integer constant expression C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_integer_constexpr_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_integer_constexpr.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_integer_constexpr_ops");
    for marker in [": global_addr ", ": iadd ", ": isub ", ": ixor "] {
        assert!(
            dump.contains(marker),
            "integer constant expression lowering should emit {marker:?} through profile rules:\n{dump}"
        );
    }

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_integer_constexpr");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir =
        std::fs::read_to_string(virtualized_ir).expect("virtualized integer constant expression IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_integer_constexpr_ops"));
    assert!(ir.contains(".amice.vm.global_addr.vm_integer_constexpr_ops"));
    assert!(ir.contains("handler.iadd"));
    assert!(ir.contains("handler.isub"));
    assert!(ir.contains("handler.ixor"));
}

#[test]
#[serial]
fn test_vm_virtualize_nested_gep_constant_expression_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_nested_gep_constexpr.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_nested_gep_constexpr'
source_filename = "vm_virtualize_nested_gep_constexpr.ll"

@vm_nested_constexpr_values = addrspace(1) global [4 x i32] [i32 11, i32 22, i32 33, i32 44], align 16

declare i64 @vm_constexpr_ptr_load_i32(ptr)

define i64 @vm_nested_gep_constexpr(i64 %salt) {
entry:
  %picked = call i64 @vm_constexpr_ptr_load_i32(ptr getelementptr (i8, ptr addrspacecast (ptr addrspace(1) getelementptr ([4 x i32], ptr addrspace(1) @vm_nested_constexpr_values, i64 0, i64 1) to ptr), i64 4))
  %mixed = xor i64 %picked, %salt
  ret i64 %mixed
}
"#,
    )
    .expect("nested getelementptr constant expression LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_nested_gep_constexpr_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int64_t vm_nested_gep_constexpr(int64_t salt);

int64_t vm_constexpr_ptr_load_i32(void *value) {
    return *(int32_t *)value;
}

int main(void) {
    int64_t a = vm_nested_gep_constexpr(0x1234);
    int64_t b = vm_nested_gep_constexpr(-55);
    printf("%lld %lld %lld\n", (long long)a, (long long)b, (long long)(a ^ b));
    return 0;
}
"#,
    )
    .expect("nested getelementptr constant expression C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_nested_gep_constexpr_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_nested_gep_constexpr.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_nested_gep_constexpr");
    assert!(
        dump.contains(": global_addr "),
        "nested getelementptr base should still materialize through profile global_addr:\n{dump}"
    );
    assert!(
        dump.matches(": gep ").count() >= 2,
        "nested getelementptr constant expressions should emit one profile gep per constant layer:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_nested_gep_constexpr");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir)
        .expect("virtualized nested getelementptr constant expression IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_nested_gep_constexpr"));
    assert!(ir.contains(".amice.vm.global_addr.vm_nested_gep_constexpr"));
    assert!(ir.contains("handler.global_addr"));
    assert!(ir.contains("handler.gep"));
}

#[test]
#[serial]
fn test_vm_virtualize_annotation_intrinsics_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_annotation_intrinsics.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_annotation_intrinsics'
source_filename = "vm_virtualize_annotation_intrinsics.ll"

@.ann = private unnamed_addr constant [4 x i8] c"ann\00"
@.file = private unnamed_addr constant [2 x i8] c"f\00"

declare i32 @llvm.annotation.i32(i32, ptr, ptr, i32)
declare ptr @llvm.ptr.annotation.p0.p0(ptr, ptr, ptr, i32, ptr)
declare void @llvm.var.annotation.p0.p0(ptr, ptr, ptr, i32, ptr)
declare void @llvm.codeview.annotation(metadata)

define i32 @vm_annotation_intrinsics(ptr %base, i32 %seed) {
entry:
  %ptr = call ptr @llvm.ptr.annotation.p0.p0(ptr %base, ptr @.ann, ptr @.file, i32 11, ptr @.ann)
  call void @llvm.var.annotation.p0.p0(ptr %ptr, ptr @.ann, ptr @.file, i32 12, ptr @.ann)
  call void @llvm.codeview.annotation(metadata !"vm.annotation")
  %value = load i32, ptr %ptr, align 4
  %hinted = call i32 @llvm.annotation.i32(i32 %seed, ptr @.ann, ptr @.file, i32 13)
  %mixed = xor i32 %value, %hinted
  ret i32 %mixed
}
"#,
    )
    .expect("annotation intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_annotation_intrinsics_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_annotation_intrinsics(int32_t *base, int32_t seed);

int main(void) {
    int32_t values[2] = {0x13572468, -19};
    int32_t a = vm_annotation_intrinsics(values, 0x24681357);
    int32_t b = vm_annotation_intrinsics(values + 1, -77);
    printf("%d\n", (int)(a ^ b));
    return 0;
}
"#,
    )
    .expect("annotation intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_annotation_intrinsics_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_annotation_intrinsics.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_annotation_intrinsics");
    assert!(
        dump.contains(": mov "),
        "annotation value intrinsics should lower through mov:\n{dump}"
    );
    assert!(
        dump.contains(": fake_nop "),
        "annotation metadata-only intrinsics should lower through fake_nop:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_annotation_intrinsics");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized annotation LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_annotation_intrinsics"));
    assert!(!ir.contains("@llvm.annotation"));
    assert!(!ir.contains("@llvm.ptr.annotation"));
    assert!(!ir.contains("@llvm.var.annotation"));
    assert!(!ir.contains("@llvm.codeview.annotation"));
}

#[test]
#[serial]
fn test_vm_virtualize_is_constant_intrinsic_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_is_constant_intrinsic.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_is_constant_intrinsic'
source_filename = "vm_virtualize_is_constant_intrinsic.ll"

declare i1 @llvm.is.constant.i32(i32)

define i32 @vm_is_constant_intrinsic(i32 %seed) {
entry:
  %known = call i1 @llvm.is.constant.i32(i32 123)
  %dynamic = call i1 @llvm.is.constant.i32(i32 %seed)
  %known_i32 = zext i1 %known to i32
  %dynamic_i32 = zext i1 %dynamic to i32
  %tag = shl i32 %known_i32, 4
  %combined = or i32 %tag, %dynamic_i32
  %mixed = xor i32 %combined, %seed
  ret i32 %mixed
}
"#,
    )
    .expect("llvm.is.constant intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_is_constant_intrinsic_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_is_constant_intrinsic(int32_t seed);

int main(void) {
    int32_t a = vm_is_constant_intrinsic(0x13572468);
    int32_t b = vm_is_constant_intrinsic(-19);
    printf("%d %d %d\n", (int)a, (int)b, (int)(a ^ b));
    return 0;
}
"#,
    )
    .expect("llvm.is.constant intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_is_constant_intrinsic_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_is_constant_intrinsic.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_is_constant_intrinsic");
    assert!(
        dump.contains(": mov_imm ") || dump.contains(": const_load "),
        "llvm.is.constant should materialize its i1 result through profile constants:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_is_constant_intrinsic");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized llvm.is.constant IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_is_constant_intrinsic"));
    assert!(!ir.contains("@llvm.is.constant"));
}

#[test]
#[serial]
fn test_vm_virtualize_objectsize_intrinsic_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_objectsize_intrinsic.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_objectsize_intrinsic'
source_filename = "vm_virtualize_objectsize_intrinsic.ll"

@vm_objectsize_global = internal global [40 x i8] zeroinitializer, align 16

declare i64 @llvm.objectsize.i64.p0(ptr, i1 immarg, i1 immarg, i1 immarg)

define i64 @vm_objectsize_intrinsic(i64 %seed) {
entry:
  %buf = alloca [24 x i8], align 16
  %buf_tail = getelementptr inbounds [24 x i8], ptr %buf, i64 0, i64 5
  %global_tail = getelementptr inbounds [40 x i8], ptr @vm_objectsize_global, i64 0, i64 9
  %buf_size = call i64 @llvm.objectsize.i64.p0(ptr %buf, i1 false, i1 true, i1 false)
  %buf_tail_size = call i64 @llvm.objectsize.i64.p0(ptr %buf_tail, i1 false, i1 true, i1 false)
  %global_size = call i64 @llvm.objectsize.i64.p0(ptr @vm_objectsize_global, i1 false, i1 true, i1 false)
  %global_tail_size = call i64 @llvm.objectsize.i64.p0(ptr %global_tail, i1 false, i1 true, i1 false)
  %a = add i64 %buf_size, %buf_tail_size
  %b = add i64 %global_size, %global_tail_size
  %sum = add i64 %a, %b
  %mixed = xor i64 %sum, %seed
  ret i64 %mixed
}
"#,
    )
    .expect("llvm.objectsize intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_objectsize_intrinsic_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_objectsize_intrinsic(uint64_t seed);

int main(void) {
    uint64_t a = vm_objectsize_intrinsic(0x123456789abcdef0ULL);
    uint64_t b = vm_objectsize_intrinsic(0x0fedcba987654321ULL);
    printf("%llu %llu %llu\n", (unsigned long long)a, (unsigned long long)b, (unsigned long long)(a ^ b));
    return 0;
}
"#,
    )
    .expect("llvm.objectsize intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_objectsize_intrinsic_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_objectsize_intrinsic.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_objectsize_intrinsic");
    assert!(
        dump.contains(": mov_imm "),
        "llvm.objectsize should materialize static sizes through profile mov_imm:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_objectsize_intrinsic");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized llvm.objectsize IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_objectsize_intrinsic"));
    assert!(!ir.contains("@llvm.objectsize"));
}

#[test]
#[serial]
fn test_vm_virtualize_unknown_objectsize_safely_skips() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_objectsize_skip.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_objectsize_skip'
source_filename = "vm_virtualize_objectsize_skip.ll"

declare i64 @llvm.objectsize.i64.p0(ptr, i1 immarg, i1 immarg, i1 immarg)

define i64 @vm_objectsize_unknown_skip(ptr %ptr, i64 %seed) {
entry:
  %size = call i64 @llvm.objectsize.i64.p0(ptr %ptr, i1 false, i1 true, i1 false)
  %mixed = xor i64 %size, %seed
  ret i64 %mixed
}
"#,
    )
    .expect("unsupported llvm.objectsize LLVM IR fixture should be writable");

    let (output_ir, output) = optimize_ir_with_plugin_debug_pipeline(
        &ir_source,
        "vm_virtualize_objectsize_skip.ll",
        "default<O0>",
        vm_virtualize_config(),
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);

    let ir = std::fs::read_to_string(output_ir).expect("objectsize skip output IR should be readable");
    assert!(!ir.contains(".amice.vm.bytecode.vm_objectsize_unknown_skip"));
    assert!(stderr.contains("skip function"));
    assert!(stderr.contains("vm_objectsize_unknown_skip"));
    assert!(stderr.contains("llvm.objectsize only supports static alloca, global, and constant-offset GEP operands"));
}

#[test]
#[serial]
fn test_vm_virtualize_is_fpclass_intrinsic_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_is_fpclass_intrinsic.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_is_fpclass_intrinsic'
source_filename = "vm_virtualize_is_fpclass_intrinsic.ll"

declare i1 @llvm.is.fpclass.f32(float, i32 immarg)
declare i1 @llvm.is.fpclass.f64(double, i32 immarg)

define i32 @vm_is_fpclass_intrinsic(float %f, double %d) {
entry:
  %f_nan = call i1 @llvm.is.fpclass.f32(float %f, i32 3)
  %f_pos_zero = call i1 @llvm.is.fpclass.f32(float %f, i32 64)
  %f_pos_normal = call i1 @llvm.is.fpclass.f32(float %f, i32 256)
  %f_pos_subnormal = call i1 @llvm.is.fpclass.f32(float %f, i32 128)
  %d_neg_inf = call i1 @llvm.is.fpclass.f64(double %d, i32 4)
  %d_neg_zero = call i1 @llvm.is.fpclass.f64(double %d, i32 32)
  %d_neg_normal = call i1 @llvm.is.fpclass.f64(double %d, i32 8)
  %d_neg_subnormal = call i1 @llvm.is.fpclass.f64(double %d, i32 16)
  %b0 = zext i1 %f_nan to i32
  %b1_raw = zext i1 %f_pos_zero to i32
  %b1 = shl i32 %b1_raw, 1
  %b2_raw = zext i1 %f_pos_normal to i32
  %b2 = shl i32 %b2_raw, 2
  %b3_raw = zext i1 %f_pos_subnormal to i32
  %b3 = shl i32 %b3_raw, 3
  %b4_raw = zext i1 %d_neg_inf to i32
  %b4 = shl i32 %b4_raw, 4
  %b5_raw = zext i1 %d_neg_zero to i32
  %b5 = shl i32 %b5_raw, 5
  %b6_raw = zext i1 %d_neg_normal to i32
  %b6 = shl i32 %b6_raw, 6
  %b7_raw = zext i1 %d_neg_subnormal to i32
  %b7 = shl i32 %b7_raw, 7
  %m1 = or i32 %b0, %b1
  %m2 = or i32 %m1, %b2
  %m3 = or i32 %m2, %b3
  %m4 = or i32 %m3, %b4
  %m5 = or i32 %m4, %b5
  %m6 = or i32 %m5, %b6
  %m7 = or i32 %m6, %b7
  ret i32 %m7
}
"#,
    )
    .expect("llvm.is.fpclass intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_is_fpclass_intrinsic_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_is_fpclass_intrinsic(float f, double d);

int main(void) {
    int32_t a = vm_is_fpclass_intrinsic(__builtin_nanf(""), -__builtin_huge_val());
    int32_t b = vm_is_fpclass_intrinsic(0.0f, -0.0);
    int32_t c = vm_is_fpclass_intrinsic(2.5f, -3.75);
    int32_t d = vm_is_fpclass_intrinsic(0x1p-149f, -0x1p-1074);
    printf("%d %d %d %d %d\n", (int)a, (int)b, (int)c, (int)d, (int)(a ^ b ^ c ^ d));
    return 0;
}
"#,
    )
    .expect("llvm.is.fpclass intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_is_fpclass_intrinsic_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_is_fpclass_intrinsic.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_is_fpclass_intrinsic");
    assert!(
        dump.contains(": fpclass "),
        "llvm.is.fpclass should lower to profile fpclass bytecode:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_is_fpclass_intrinsic");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized llvm.is.fpclass IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_is_fpclass_intrinsic"));
    assert!(ir.contains("handler.fpclass"));
    assert!(!ir.contains("@llvm.is.fpclass"));
}

#[test]
#[serial]
fn test_vm_virtualize_fabs_intrinsic_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_fabs_intrinsic.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_fabs_intrinsic'
source_filename = "vm_virtualize_fabs_intrinsic.ll"

declare float @llvm.fabs.f32(float)
declare double @llvm.fabs.f64(double)

define i64 @vm_fabs_intrinsic(float %f, double %d) {
entry:
  %af = call float @llvm.fabs.f32(float %f)
  %ad = call double @llvm.fabs.f64(double %d)
  %fbits = bitcast float %af to i32
  %dbits = bitcast double %ad to i64
  %fz = zext i32 %fbits to i64
  %dlow = and i64 %dbits, 4294967295
  %mix = xor i64 %fz, %dlow
  ret i64 %mix
}
"#,
    )
    .expect("llvm.fabs intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_fabs_intrinsic_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_fabs_intrinsic(float f, double d);

int main(void) {
    uint64_t a = vm_fabs_intrinsic(-0.0f, -0.0);
    uint64_t b = vm_fabs_intrinsic(-3.5f, -7.25);
    uint64_t c = vm_fabs_intrinsic(__builtin_nanf(""), -__builtin_huge_val());
    printf("%llu %llu %llu %llu\n",
           (unsigned long long)a,
           (unsigned long long)b,
           (unsigned long long)c,
           (unsigned long long)(a ^ b ^ c));
    return 0;
}
"#,
    )
    .expect("llvm.fabs intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_fabs_intrinsic_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) = optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_fabs_intrinsic.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_fabs_intrinsic");
    assert!(
        dump.contains(": fabs "),
        "llvm.fabs should lower to profile fabs bytecode:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_fabs_intrinsic");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized llvm.fabs IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_fabs_intrinsic"));
    assert!(ir.contains("handler.fabs"));
    assert!(!ir.contains("@llvm.fabs"));
}

#[test]
#[serial]
fn test_vm_virtualize_copysign_intrinsic_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_copysign_intrinsic.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_copysign_intrinsic'
source_filename = "vm_virtualize_copysign_intrinsic.ll"

declare float @llvm.copysign.f32(float, float)
declare double @llvm.copysign.f64(double, double)

define i64 @vm_copysign_intrinsic(float %magf, float %signf, double %magd, double %signd) {
entry:
  %cf = call float @llvm.copysign.f32(float %magf, float %signf)
  %cd = call double @llvm.copysign.f64(double %magd, double %signd)
  %fbits = bitcast float %cf to i32
  %dbits = bitcast double %cd to i64
  %fz = zext i32 %fbits to i64
  %dhigh = lshr i64 %dbits, 32
  %mix = xor i64 %fz, %dhigh
  ret i64 %mix
}
"#,
    )
    .expect("llvm.copysign intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_copysign_intrinsic_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_copysign_intrinsic(float magf, float signf, double magd, double signd);

int main(void) {
    uint64_t a = vm_copysign_intrinsic(3.5f, -0.0f, 7.25, -0.0);
    uint64_t b = vm_copysign_intrinsic(-2.0f, 1.0f, -9.5, 1.0);
    uint64_t c = vm_copysign_intrinsic(__builtin_nanf(""), -1.0f, __builtin_huge_val(), -1.0);
    printf("%llu %llu %llu %llu\n",
           (unsigned long long)a,
           (unsigned long long)b,
           (unsigned long long)c,
           (unsigned long long)(a ^ b ^ c));
    return 0;
}
"#,
    )
    .expect("llvm.copysign intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_copysign_intrinsic_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_copysign_intrinsic.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_copysign_intrinsic");
    assert!(
        dump.contains(": fcopysign "),
        "llvm.copysign should lower to profile fcopysign bytecode:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_copysign_intrinsic");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized llvm.copysign IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_copysign_intrinsic"));
    assert!(ir.contains("handler.fcopysign"));
    assert!(!ir.contains("@llvm.copysign"));
}

#[test]
#[serial]
fn test_vm_virtualize_sqrt_intrinsic_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_sqrt_intrinsic.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_sqrt_intrinsic'
source_filename = "vm_virtualize_sqrt_intrinsic.ll"

declare float @llvm.sqrt.f32(float)
declare double @llvm.sqrt.f64(double)

define i64 @vm_sqrt_intrinsic(float %f, double %d) {
entry:
  %sf = call float @llvm.sqrt.f32(float %f)
  %sd = call double @llvm.sqrt.f64(double %d)
  %fbits = bitcast float %sf to i32
  %dbits = bitcast double %sd to i64
  %fz = zext i32 %fbits to i64
  %dhigh = lshr i64 %dbits, 32
  %mix = xor i64 %fz, %dhigh
  ret i64 %mix
}
"#,
    )
    .expect("llvm.sqrt intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_sqrt_intrinsic_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_sqrt_intrinsic(float f, double d);

int main(void) {
    uint64_t a = vm_sqrt_intrinsic(0.0f, 0.0);
    uint64_t b = vm_sqrt_intrinsic(4.0f, 9.0);
    uint64_t c = vm_sqrt_intrinsic(144.0f, 625.0);
    uint64_t d = vm_sqrt_intrinsic(__builtin_huge_valf(), __builtin_huge_val());
    printf("%llu %llu %llu %llu %llu\n",
           (unsigned long long)a,
           (unsigned long long)b,
           (unsigned long long)c,
           (unsigned long long)d,
           (unsigned long long)(a ^ b ^ c ^ d));
    return 0;
}
"#,
    )
    .expect("llvm.sqrt intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_sqrt_intrinsic_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) = optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_sqrt_intrinsic.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_sqrt_intrinsic");
    assert!(
        dump.contains(": fsqrt "),
        "llvm.sqrt should lower to profile fsqrt bytecode:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_sqrt_intrinsic");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized llvm.sqrt IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_sqrt_intrinsic"));
    assert!(ir.contains("handler.fsqrt"));
}

#[test]
#[serial]
fn test_vm_virtualize_canonicalize_intrinsic_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_canonicalize_intrinsic.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_canonicalize_intrinsic'
source_filename = "vm_virtualize_canonicalize_intrinsic.ll"

declare float @llvm.canonicalize.f32(float)
declare double @llvm.canonicalize.f64(double)

define i64 @vm_canonicalize_intrinsic(float %f, double %d) {
entry:
  %cf = call float @llvm.canonicalize.f32(float %f)
  %cd = call double @llvm.canonicalize.f64(double %d)
  %fbits = bitcast float %cf to i32
  %dbits = bitcast double %cd to i64
  %fz = zext i32 %fbits to i64
  %dhigh = lshr i64 %dbits, 32
  %mix = xor i64 %fz, %dhigh
  ret i64 %mix
}
"#,
    )
    .expect("llvm.canonicalize intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_canonicalize_intrinsic_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_canonicalize_intrinsic(float f, double d);

int main(void) {
    uint64_t a = vm_canonicalize_intrinsic(0.0f, 0.0);
    uint64_t b = vm_canonicalize_intrinsic(-7.5f, -13.25);
    uint64_t c = vm_canonicalize_intrinsic(__builtin_nanf(""), __builtin_nan(""));
    printf("%llu %llu %llu %llu\n",
           (unsigned long long)a,
           (unsigned long long)b,
           (unsigned long long)c,
           (unsigned long long)(a ^ b ^ c));
    return 0;
}
"#,
    )
    .expect("llvm.canonicalize intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_canonicalize_intrinsic_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_canonicalize_intrinsic.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_canonicalize_intrinsic");
    assert!(
        dump.contains(": fcanonicalize "),
        "llvm.canonicalize should lower to profile fcanonicalize bytecode:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_canonicalize_intrinsic");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized llvm.canonicalize IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_canonicalize_intrinsic"));
    assert!(ir.contains("handler.fcanonicalize"));
}

#[test]
#[serial]
fn test_vm_virtualize_float_unary_intrinsics_match_baseline() {
    ensure_plugin_built();

    let cases = [
        ("floor", "ffloor"),
        ("ceil", "fceil"),
        ("trunc", "ftrunc"),
        ("rint", "frint"),
        ("nearbyint", "fnearbyint"),
        ("round", "fround"),
        ("roundeven", "froundeven"),
    ];

    let mut ir = String::from(
        r#"; ModuleID = 'vm_virtualize_float_unary_intrinsics'
source_filename = "vm_virtualize_float_unary_intrinsics.ll"

"#,
    );
    for (intrinsic, _) in cases {
        writeln!(ir, "declare float @llvm.{intrinsic}.f32(float)")
            .expect("float unary f32 declaration should be writable");
        writeln!(ir, "declare double @llvm.{intrinsic}.f64(double)")
            .expect("float unary f64 declaration should be writable");
    }
    for (intrinsic, _) in cases {
        write!(
            ir,
            r#"
define i64 @vm_{intrinsic}_intrinsic(float %f, double %d) {{
entry:
  %rf = call float @llvm.{intrinsic}.f32(float %f)
  %rd = call double @llvm.{intrinsic}.f64(double %d)
  %fbits = bitcast float %rf to i32
  %dbits = bitcast double %rd to i64
  %fz = zext i32 %fbits to i64
  %dhigh = lshr i64 %dbits, 32
  %mix = xor i64 %fz, %dhigh
  ret i64 %mix
}}
"#
        )
        .expect("float unary intrinsic function should be writable");
    }

    let ir_source = output_dir().join("vm_virtualize_float_unary_intrinsics.input.ll");
    std::fs::write(&ir_source, ir).expect("float unary intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_float_unary_intrinsics_harness.c");
    let mut harness_source = String::from(
        r#"#include <stdint.h>
#include <stdio.h>

"#,
    );
    for (intrinsic, _) in cases {
        writeln!(harness_source, "uint64_t vm_{intrinsic}_intrinsic(float f, double d);")
            .expect("float unary harness declaration should be writable");
    }
    harness_source.push_str(
        r#"
int main(void) {
    uint64_t acc = 0;
"#,
    );
    let inputs = [
        ("floor", "-2.75f", "-2.75"),
        ("ceil", "-2.75f", "-2.75"),
        ("trunc", "-2.75f", "-2.75"),
        ("rint", "2.5f", "2.5"),
        ("nearbyint", "-2.5f", "-2.5"),
        ("round", "2.5f", "-2.5"),
        ("roundeven", "3.5f", "2.5"),
    ];
    for (intrinsic, float_value, double_value) in inputs {
        writeln!(
            harness_source,
            "    uint64_t {intrinsic} = vm_{intrinsic}_intrinsic({float_value}, {double_value});"
        )
        .expect("float unary harness call should be writable");
        writeln!(harness_source, "    acc ^= {intrinsic};")
            .expect("float unary harness accumulator should be writable");
    }
    harness_source.push_str("    printf(\"");
    for _ in inputs {
        harness_source.push_str("%llu ");
    }
    harness_source.push_str("%llu\\n\"");
    for (intrinsic, _, _) in inputs {
        writeln!(harness_source, ",\n           (unsigned long long){intrinsic}")
            .expect("float unary harness printf argument should be writable");
    }
    harness_source.push_str(
        r#",
           (unsigned long long)acc);
    return 0;
}
"#,
    );
    std::fs::write(&harness, harness_source).expect("float unary intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness_and_args(
        &ir_source,
        &harness,
        "vm_virtualize_float_unary_intrinsics_baseline",
        &["-lm"],
    );
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_float_unary_intrinsics.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    for (intrinsic, instruction) in cases {
        let function = format!("vm_{intrinsic}_intrinsic");
        let dump = bytecode_dump_for_function(&stderr, &function);
        assert!(
            dump.contains(&format!(": {instruction} ")),
            "llvm.{intrinsic} should lower to profile {instruction} bytecode:\n{dump}"
        );
    }

    let virtualized = compile_ir_with_c_harness_and_args(
        &virtualized_ir,
        &harness,
        "vm_virtualize_float_unary_intrinsics",
        &["-lm"],
    );
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized float unary intrinsic IR should be readable");
    for (intrinsic, instruction) in cases {
        assert!(ir.contains(&format!(".amice.vm.bytecode.vm_{intrinsic}_intrinsic")));
        assert!(ir.contains(&format!("handler.{instruction}")));
    }
}

#[test]
#[serial]
fn test_vm_virtualize_fma_intrinsic_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_fma_intrinsic.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_fma_intrinsic'
source_filename = "vm_virtualize_fma_intrinsic.ll"

declare float @llvm.fma.f32(float, float, float)
declare double @llvm.fma.f64(double, double, double)

define i64 @vm_fma_intrinsic(float %af, float %bf, float %cf, double %ad, double %bd, double %cd) {
entry:
  %rf = call float @llvm.fma.f32(float %af, float %bf, float %cf)
  %rd = call double @llvm.fma.f64(double %ad, double %bd, double %cd)
  %fbits = bitcast float %rf to i32
  %dbits = bitcast double %rd to i64
  %fz = zext i32 %fbits to i64
  %dhigh = lshr i64 %dbits, 32
  %mix = xor i64 %fz, %dhigh
  ret i64 %mix
}
"#,
    )
    .expect("llvm.fma intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_fma_intrinsic_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_fma_intrinsic(float af, float bf, float cf, double ad, double bd, double cd);

int main(void) {
    uint64_t a = vm_fma_intrinsic(1.5f, 2.0f, 0.25f, 1.5, 2.0, 0.25);
    uint64_t b = vm_fma_intrinsic(-3.0f, 4.0f, 5.5f, -3.0, 4.0, 5.5);
    uint64_t c = vm_fma_intrinsic(0.0f, -7.0f, 9.0f, 0.0, -7.0, 9.0);
    printf("%llu %llu %llu %llu\n",
           (unsigned long long)a,
           (unsigned long long)b,
           (unsigned long long)c,
           (unsigned long long)(a ^ b ^ c));
    return 0;
}
"#,
    )
    .expect("llvm.fma intrinsic C harness should be writable");

    let baseline =
        compile_ir_with_c_harness_and_args(&ir_source, &harness, "vm_virtualize_fma_intrinsic_baseline", &["-lm"]);
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) = optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_fma_intrinsic.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_fma_intrinsic");
    assert!(
        dump.contains(": ffma width=48"),
        "llvm.fma should lower to 48-byte profile ffma bytecode:\n{dump}"
    );

    let virtualized =
        compile_ir_with_c_harness_and_args(&virtualized_ir, &harness, "vm_virtualize_fma_intrinsic", &["-lm"]);
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized llvm.fma IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_fma_intrinsic"));
    assert!(ir.contains("handler.ffma"));
}

#[test]
#[serial]
fn test_vm_virtualize_fmuladd_intrinsic_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_fmuladd_intrinsic.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_fmuladd_intrinsic'
source_filename = "vm_virtualize_fmuladd_intrinsic.ll"

declare float @llvm.fmuladd.f32(float, float, float)
declare double @llvm.fmuladd.f64(double, double, double)

define i64 @vm_fmuladd_intrinsic(float %af, float %bf, float %cf, double %ad, double %bd, double %cd) {
entry:
  %rf = call float @llvm.fmuladd.f32(float %af, float %bf, float %cf)
  %rd = call double @llvm.fmuladd.f64(double %ad, double %bd, double %cd)
  %rf_bits = bitcast float %rf to i32
  %rd_bits = bitcast double %rd to i64
  %rf64 = zext i32 %rf_bits to i64
  %mix = xor i64 %rf64, %rd_bits
  ret i64 %mix
}
"#,
    )
    .expect("llvm.fmuladd intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_fmuladd_intrinsic_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_fmuladd_intrinsic(float af, float bf, float cf, double ad, double bd, double cd);

int main(void) {
    uint64_t a = vm_fmuladd_intrinsic(1.5f, 2.0f, 0.25f, 1.5, 2.0, 0.25);
    uint64_t b = vm_fmuladd_intrinsic(-3.0f, 4.0f, 5.5f, -3.0, 4.0, 5.5);
    uint64_t c = vm_fmuladd_intrinsic(0.0f, -7.0f, 9.0f, 0.0, -7.0, 9.0);
    printf("%llu %llu %llu %llu\n",
           (unsigned long long)a,
           (unsigned long long)b,
           (unsigned long long)c,
           (unsigned long long)(a ^ b ^ c));
    return 0;
}
"#,
    )
    .expect("llvm.fmuladd intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_fmuladd_intrinsic_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_fmuladd_intrinsic.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_fmuladd_intrinsic");
    assert!(
        dump.contains(": ffmuladd width=48"),
        "llvm.fmuladd should lower to 48-byte profile ffmuladd bytecode:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_fmuladd_intrinsic");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized llvm.fmuladd IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_fmuladd_intrinsic"));
    assert!(ir.contains("handler.ffmuladd"));
}

#[test]
#[serial]
fn test_vm_virtualize_minnum_maxnum_intrinsics_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_minnum_maxnum_intrinsics.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_minnum_maxnum_intrinsics'
source_filename = "vm_virtualize_minnum_maxnum_intrinsics.ll"

declare float @llvm.minnum.f32(float, float)
declare float @llvm.maxnum.f32(float, float)
declare double @llvm.minnum.f64(double, double)
declare double @llvm.maxnum.f64(double, double)

define i64 @vm_minnum_maxnum_intrinsics(float %af, float %bf, double %ad, double %bd) {
entry:
  %minf = call float @llvm.minnum.f32(float %af, float %bf)
  %maxf = call float @llvm.maxnum.f32(float %af, float %bf)
  %mind = call double @llvm.minnum.f64(double %ad, double %bd)
  %maxd = call double @llvm.maxnum.f64(double %ad, double %bd)
  %minf_bits = bitcast float %minf to i32
  %maxf_bits = bitcast float %maxf to i32
  %mind_bits = bitcast double %mind to i64
  %maxd_bits = bitcast double %maxd to i64
  %minf64 = zext i32 %minf_bits to i64
  %maxf64 = zext i32 %maxf_bits to i64
  %mix0 = xor i64 %minf64, %maxf64
  %mix1 = xor i64 %mind_bits, %maxd_bits
  %mix = xor i64 %mix0, %mix1
  ret i64 %mix
}
"#,
    )
    .expect("llvm minnum/maxnum intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_minnum_maxnum_intrinsics_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_minnum_maxnum_intrinsics(float af, float bf, double ad, double bd);

int main(void) {
    uint64_t a = vm_minnum_maxnum_intrinsics(1.5f, 2.25f, 1.5, 2.25);
    uint64_t b = vm_minnum_maxnum_intrinsics(-7.0f, 3.5f, -7.0, 3.5);
    uint64_t c = vm_minnum_maxnum_intrinsics(0.0f, -0.0f, 0.0, -0.0);
    printf("%llu %llu %llu %llu\n",
           (unsigned long long)a,
           (unsigned long long)b,
           (unsigned long long)c,
           (unsigned long long)(a ^ b ^ c));
    return 0;
}
"#,
    )
    .expect("llvm minnum/maxnum intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_minnum_maxnum_intrinsics_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_minnum_maxnum_intrinsics.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_minnum_maxnum_intrinsics");
    assert!(
        dump.contains(": fminnum width=16"),
        "llvm.minnum should lower to profile fminnum bytecode:\n{dump}"
    );
    assert!(
        dump.contains(": fmaxnum width=16"),
        "llvm.maxnum should lower to profile fmaxnum bytecode:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_minnum_maxnum_intrinsics");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized llvm minnum/maxnum IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_minnum_maxnum_intrinsics"));
    assert!(ir.contains("handler.fminnum"));
    assert!(ir.contains("handler.fmaxnum"));
}

#[test]
#[serial]
fn test_vm_virtualize_minimum_maximum_intrinsics_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_minimum_maximum_intrinsics.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_minimum_maximum_intrinsics'
source_filename = "vm_virtualize_minimum_maximum_intrinsics.ll"

declare float @llvm.minimum.f32(float, float)
declare float @llvm.maximum.f32(float, float)
declare double @llvm.minimum.f64(double, double)
declare double @llvm.maximum.f64(double, double)

define i64 @vm_minimum_maximum_intrinsics(float %af, float %bf, double %ad, double %bd) {
entry:
  %minf = call float @llvm.minimum.f32(float %af, float %bf)
  %maxf = call float @llvm.maximum.f32(float %af, float %bf)
  %mind = call double @llvm.minimum.f64(double %ad, double %bd)
  %maxd = call double @llvm.maximum.f64(double %ad, double %bd)
  %minf_bits = bitcast float %minf to i32
  %maxf_bits = bitcast float %maxf to i32
  %mind_bits = bitcast double %mind to i64
  %maxd_bits = bitcast double %maxd to i64
  %minf64 = zext i32 %minf_bits to i64
  %maxf64 = zext i32 %maxf_bits to i64
  %mix0 = xor i64 %minf64, %maxf64
  %mix1 = xor i64 %mind_bits, %maxd_bits
  %mix = xor i64 %mix0, %mix1
  ret i64 %mix
}
"#,
    )
    .expect("llvm minimum/maximum intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_minimum_maximum_intrinsics_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_minimum_maximum_intrinsics(float af, float bf, double ad, double bd);

int main(void) {
    uint64_t a = vm_minimum_maximum_intrinsics(1.5f, 2.25f, 1.5, 2.25);
    uint64_t b = vm_minimum_maximum_intrinsics(-7.0f, 3.5f, -7.0, 3.5);
    uint64_t c = vm_minimum_maximum_intrinsics(0.0f, -0.0f, 0.0, -0.0);
    printf("%llu %llu %llu %llu\n",
           (unsigned long long)a,
           (unsigned long long)b,
           (unsigned long long)c,
           (unsigned long long)(a ^ b ^ c));
    return 0;
}
"#,
    )
    .expect("llvm minimum/maximum intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(
        &ir_source,
        &harness,
        "vm_virtualize_minimum_maximum_intrinsics_baseline",
    );
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_minimum_maximum_intrinsics.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_minimum_maximum_intrinsics");
    assert!(
        dump.contains(": fminimum width=16"),
        "llvm.minimum should lower to profile fminimum bytecode:\n{dump}"
    );
    assert!(
        dump.contains(": fmaximum width=16"),
        "llvm.maximum should lower to profile fmaximum bytecode:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_minimum_maximum_intrinsics");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized llvm minimum/maximum IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_minimum_maximum_intrinsics"));
    assert!(ir.contains("handler.fminimum"));
    assert!(ir.contains("handler.fmaximum"));
}

#[test]
#[serial]
fn test_vm_virtualize_scalar_select_pointer_and_float_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_scalar_select.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_scalar_select'
source_filename = "vm_virtualize_scalar_select.ll"

define i32 @vm_select_ptr_load(i32 %flag, ptr %a, ptr %b) {
entry:
  %cond = icmp ne i32 %flag, 0
  %ptr = select i1 %cond, ptr %a, ptr %b
  %value = load i32, ptr %ptr, align 4
  ret i32 %value
}

define double @vm_select_double(i32 %flag, double %a, double %b) {
entry:
  %cond = icmp slt i32 %flag, 0
  %chosen = select i1 %cond, double %a, double %b
  %mixed = fadd double %chosen, 1.500000e+00
  ret double %mixed
}
"#,
    )
    .expect("scalar select LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_scalar_select_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdio.h>

int vm_select_ptr_load(int flag, int *a, int *b);
double vm_select_double(int flag, double a, double b);

int main(void) {
    int left = 41;
    int right = 99;
    int acc = 0;
    acc += vm_select_ptr_load(1, &left, &right);
    acc += vm_select_ptr_load(0, &left, &right);
    double f = vm_select_double(-1, 3.25, 7.75) + vm_select_double(4, 3.25, 7.75);
    printf("%d %.2f\n", acc, f);
    return 0;
}
"#,
    )
    .expect("scalar select C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_scalar_select_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized_ir = optimize_ir_with_plugin(&ir_source, "vm_virtualize_scalar_select.ll", vm_virtualize_config());
    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_scalar_select");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized scalar select LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_select_ptr_load"));
    assert!(ir.contains(".amice.vm.bytecode.vm_select_double"));
    assert!(ir.contains("handler.br_if"));
    assert!(ir.contains("handler.mov"));
}

#[test]
#[serial]
fn test_vm_virtualize_pointer_icmp_predicates_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_pointer_icmp.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_pointer_icmp'
source_filename = "vm_virtualize_pointer_icmp.ll"

define i64 @vm_pointer_icmp(ptr %a, ptr %b, ptr %limit) {
entry:
  %eq = icmp eq ptr %a, %b
  %ne = icmp ne ptr %b, null
  %ult = icmp ult ptr %a, %limit
  %uge = icmp uge ptr %b, %a
  %eq64 = zext i1 %eq to i64
  %ne64 = zext i1 %ne to i64
  %ult64 = zext i1 %ult to i64
  %uge64 = zext i1 %uge to i64
  %s1 = shl i64 %ne64, 8
  %s2 = shl i64 %ult64, 16
  %s3 = shl i64 %uge64, 24
  %m0 = or i64 %eq64, %s1
  %m1 = or i64 %m0, %s2
  %m2 = or i64 %m1, %s3
  ret i64 %m2
}
"#,
    )
    .expect("pointer icmp LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_pointer_icmp_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_pointer_icmp(int *a, int *b, int *limit);

int main(void) {
    int values[4] = { 1, 2, 3, 4 };
    uint64_t a = vm_pointer_icmp(&values[0], &values[0], &values[3]);
    uint64_t b = vm_pointer_icmp(&values[2], &values[1], &values[3]);
    uint64_t c = vm_pointer_icmp(&values[1], 0, &values[3]);
    printf("%llu %llu %llu\n", (unsigned long long)a, (unsigned long long)b, (unsigned long long)c);
    return 0;
}
"#,
    )
    .expect("pointer icmp C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_pointer_icmp_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) = optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_pointer_icmp.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_pointer_icmp");
    assert!(
        dump.contains(": icmp "),
        "pointer icmp predicates should lower through the scalar icmp profile rule:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_pointer_icmp");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized pointer icmp LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_pointer_icmp"));
    assert!(ir.contains("handler.icmp"));
}

#[test]
#[serial]
fn test_vm_virtualize_switch_phi_edge_moves_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_switch_phi.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_switch_phi'
source_filename = "vm_virtualize_switch_phi.ll"

define i32 @vm_switch_phi(i32 %x, i32 %salt) {
entry:
  %key = and i32 %x, 7
  switch i32 %key, label %default [
    i32 0, label %case0
    i32 3, label %case3
    i32 5, label %case5
  ]

case0:
  %a0 = add i32 %salt, 11
  br label %join

case3:
  %a3 = xor i32 %salt, %x
  br label %join

case5:
  %m5 = mul i32 %salt, 5
  br label %join

default:
  %d0 = sub i32 %salt, %key
  br label %join

join:
  %v = phi i32 [ %a0, %case0 ], [ %a3, %case3 ], [ %m5, %case5 ], [ %d0, %default ]
  %mix = xor i32 %v, %key
  ret i32 %mix
}
"#,
    )
    .expect("switch phi LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_switch_phi_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_switch_phi(int32_t x, int32_t salt);

int main(void) {
    int32_t acc = 17;
    for (int32_t i = -9; i <= 13; ++i) {
        acc = acc * 131 + vm_switch_phi(i, i * 19 + 7);
    }
    printf("%d\n", acc);
    return 0;
}
"#,
    )
    .expect("switch phi C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_switch_phi_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) = optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_switch_phi.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_switch_phi");
    assert!(
        dump.contains(": br_if ") && dump.contains(": mov "),
        "switch case dispatch and phi edge moves should both be visible in bytecode:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_switch_phi");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized switch phi LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_switch_phi"));
    assert!(ir.contains("handler.br_if"));
    assert!(ir.contains("handler.mov"));
}

#[test]
#[serial]
fn test_vm_virtualize_aggregate_select_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_aggregate_select.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_aggregate_select'
source_filename = "vm_virtualize_aggregate_select.ll"

%Agg = type { i32, i64, [2 x i16], ptr }

define i64 @vm_aggregate_select(
    i32 %flag,
    i32 %a,
    i64 %b,
    ptr %pa,
    i32 %x,
    i64 %y,
    ptr %pb
) #0 {
entry:
  %arr_a0 = insertvalue [2 x i16] undef, i16 4951, 0
  %arr_a1 = insertvalue [2 x i16] %arr_a0, i16 9320, 1
  %agg_a0 = insertvalue %Agg undef, i32 %a, 0
  %agg_a1 = insertvalue %Agg %agg_a0, i64 %b, 1
  %agg_a2 = insertvalue %Agg %agg_a1, [2 x i16] %arr_a1, 2
  %agg_a3 = insertvalue %Agg %agg_a2, ptr %pa, 3
  %arr_b0 = insertvalue [2 x i16] undef, i16 -21555, 0
  %arr_b1 = insertvalue [2 x i16] %arr_b0, i16 17185, 1
  %agg_b0 = insertvalue %Agg undef, i32 %x, 0
  %agg_b1 = insertvalue %Agg %agg_b0, i64 %y, 1
  %agg_b2 = insertvalue %Agg %agg_b1, [2 x i16] %arr_b1, 2
  %agg_b3 = insertvalue %Agg %agg_b2, ptr %pb, 3
  %cond = icmp ne i32 %flag, 0
  %selected = select i1 %cond, %Agg %agg_a3, %Agg %agg_b3
  %r0 = extractvalue %Agg %selected, 0
  %r1 = extractvalue %Agg %selected, 1
  %r2 = extractvalue %Agg %selected, 2, 0
  %r3 = extractvalue %Agg %selected, 2, 1
  %rp = extractvalue %Agg %selected, 3
  %loaded = load i32, ptr %rp, align 4
  %r0x = sext i32 %r0 to i64
  %r2x = zext i16 %r2 to i64
  %r3x = zext i16 %r3 to i64
  %loadedx = sext i32 %loaded to i64
  %m0 = xor i64 %r1, %r0x
  %m1 = add i64 %m0, %r2x
  %m2 = xor i64 %m1, %r3x
  %m3 = add i64 %m2, %loadedx
  ret i64 %m3
}

attributes #0 = { noinline optnone }
"#,
    )
    .expect("aggregate select LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_aggregate_select_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_aggregate_select(
    int32_t flag,
    int32_t a,
    uint64_t b,
    int32_t *pa,
    int32_t x,
    uint64_t y,
    int32_t *pb);

int main(void) {
    int32_t left = 0x1234567;
    int32_t right = -91;
    uint64_t acc = 0;
    acc ^= vm_aggregate_select(
        1,
        0x10203040,
        0x0123456789abcdefULL,
        &left,
        -33,
        0xfedcba9876543210ULL,
        &right);
    acc ^= vm_aggregate_select(
        0,
        0x10203040,
        0x0123456789abcdefULL,
        &left,
        -33,
        0xfedcba9876543210ULL,
        &right);
    printf("%llu\n", (unsigned long long)acc);
    return 0;
}
"#,
    )
    .expect("aggregate select C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_aggregate_select_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_aggregate_select.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_aggregate_select");
    assert!(
        dump.contains(": br_if ") || dump.contains(": icmp_br_if "),
        "aggregate select should be driven by profile br_if action or its declared fusion:\n{dump}"
    );
    assert!(
        dump.matches(": mov ").count() >= 8,
        "aggregate select and extractvalue should copy leaf fields through profile mov actions:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_aggregate_select");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized aggregate select IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_aggregate_select"));
    assert!(ir.contains("handler.br_if") || ir.contains("handler.icmp_br_if"));
    assert!(ir.contains("handler.mov"));
}

#[test]
#[serial]
fn test_vm_virtualize_aggregate_phi_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_aggregate_phi.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_aggregate_phi'
source_filename = "vm_virtualize_aggregate_phi.ll"

%Agg = type { i32, i64, [2 x i16], ptr }

define i64 @vm_aggregate_phi(i32 %flag, i32 %a, i64 %b, ptr %pa, i32 %x, i64 %y, ptr %pb) #0 {
entry:
  %cond = icmp ne i32 %flag, 0
  br i1 %cond, label %left, label %right

left:
  %arr_a0 = insertvalue [2 x i16] undef, i16 4951, 0
  %arr_a1 = insertvalue [2 x i16] %arr_a0, i16 9320, 1
  %agg_a0 = insertvalue %Agg undef, i32 %a, 0
  %agg_a1 = insertvalue %Agg %agg_a0, i64 %b, 1
  %agg_a2 = insertvalue %Agg %agg_a1, [2 x i16] %arr_a1, 2
  %agg_a3 = insertvalue %Agg %agg_a2, ptr %pa, 3
  br label %join

right:
  %arr_b0 = insertvalue [2 x i16] undef, i16 -21555, 0
  %arr_b1 = insertvalue [2 x i16] %arr_b0, i16 17185, 1
  %agg_b0 = insertvalue %Agg undef, i32 %x, 0
  %agg_b1 = insertvalue %Agg %agg_b0, i64 %y, 1
  %agg_b2 = insertvalue %Agg %agg_b1, [2 x i16] %arr_b1, 2
  %agg_b3 = insertvalue %Agg %agg_b2, ptr %pb, 3
  br label %join

join:
  %chosen = phi %Agg [ %agg_a3, %left ], [ %agg_b3, %right ]
  %r0 = extractvalue %Agg %chosen, 0
  %r1 = extractvalue %Agg %chosen, 1
  %r2 = extractvalue %Agg %chosen, 2, 0
  %r3 = extractvalue %Agg %chosen, 2, 1
  %rp = extractvalue %Agg %chosen, 3
  %loaded = load i32, ptr %rp, align 4
  %r0x = sext i32 %r0 to i64
  %r2x = zext i16 %r2 to i64
  %r3x = zext i16 %r3 to i64
  %loadedx = sext i32 %loaded to i64
  %m0 = xor i64 %r1, %r0x
  %m1 = add i64 %m0, %r2x
  %m2 = xor i64 %m1, %r3x
  %m3 = add i64 %m2, %loadedx
  ret i64 %m3
}

attributes #0 = { noinline optnone }
"#,
    )
    .expect("aggregate phi LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_aggregate_phi_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_aggregate_phi(
    int32_t flag,
    int32_t a,
    uint64_t b,
    int32_t *pa,
    int32_t x,
    uint64_t y,
    int32_t *pb);

int main(void) {
    int32_t left = 0x1234567;
    int32_t right = -91;
    uint64_t acc = 0;
    acc ^= vm_aggregate_phi(1, 0x10203040, 0x0123456789abcdefULL, &left, -33, 0xfedcba9876543210ULL, &right);
    acc ^= vm_aggregate_phi(0, 0x10203040, 0x0123456789abcdefULL, &left, -33, 0xfedcba9876543210ULL, &right);
    printf("%llu\n", (unsigned long long)acc);
    return 0;
}
"#,
    )
    .expect("aggregate phi C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_aggregate_phi_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) = optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_aggregate_phi.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_aggregate_phi");
    assert!(
        dump.matches(": mov ").count() >= 8,
        "aggregate phi should copy incoming leaf fields through profile mov actions:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_aggregate_phi");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized aggregate phi IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_aggregate_phi"));
    assert!(ir.contains("handler.mov"));
}

#[test]
#[serial]
fn test_vm_virtualize_unfrozen_poison_safely_skips() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_unfrozen_poison.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_unfrozen_poison'
source_filename = "vm_virtualize_unfrozen_poison.ll"

define i32 @vm_unfrozen_poison(i32 %a) {
entry:
  %mixed = add i32 %a, poison
  ret i32 %mixed
}
"#,
    )
    .expect("unfrozen poison LLVM IR fixture should be writable");

    let (output_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_unfrozen_poison.ll", vm_virtualize_config());
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);

    let ir = std::fs::read_to_string(output_ir).expect("unfrozen poison output IR should be readable");
    assert!(!ir.contains(".amice.vm.bytecode.vm_unfrozen_poison"));

    assert!(stderr.contains("skip function"));
    assert!(stderr.contains("vm_unfrozen_poison"));
    assert!(stderr.contains("undef/poison values must be frozen before VM materialization"));
}

#[test]
#[serial]
fn test_vm_virtualize_dynamic_alloca_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_dynamic_alloca.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_dynamic_alloca'
source_filename = "vm_virtualize_dynamic_alloca.ll"

define i32 @vm_dynamic_alloca(i32 %count, i32 %seed) {
entry:
  %bounded = and i32 %count, 7
  %n = add nuw nsw i32 %bounded, 1
  %n64 = zext i32 %n to i64
  %slot = alloca i32, i64 %n64, align 4
  %last32 = add nsw i32 %n, -1
  %last64 = zext i32 %last32 to i64
  %tail = getelementptr inbounds i32, ptr %slot, i64 %last64
  store i32 %seed, ptr %tail, align 4
  %head = getelementptr inbounds i32, ptr %slot, i64 0
  store i32 %n, ptr %head, align 4
  %loaded_tail = load i32, ptr %tail, align 4
  %loaded_head = load i32, ptr %head, align 4
  %mixed = xor i32 %loaded_tail, %loaded_head
  ret i32 %mixed
}
"#,
    )
    .expect("dynamic alloca LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_dynamic_alloca_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_dynamic_alloca(int32_t count, int32_t seed);

int main(void) {
    int32_t acc = 0;
    acc ^= vm_dynamic_alloca(0, 0x13572468);
    acc ^= vm_dynamic_alloca(3, -77);
    acc ^= vm_dynamic_alloca(15, 0x24681357);
    printf("%d\n", acc);
    return 0;
}
"#,
    )
    .expect("dynamic alloca C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_dynamic_alloca_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) = optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_dynamic_alloca.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_dynamic_alloca");
    assert!(
        dump.contains(": alloca_dyn "),
        "dynamic alloca should lower through profile alloca_dyn:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_dynamic_alloca");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized dynamic alloca LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_dynamic_alloca"));
    assert!(ir.contains("handler.alloca_dyn"));
}

#[test]
#[serial]
fn test_vm_virtualize_lifetime_intrinsics_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_lifetime_intrinsics.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_lifetime_intrinsics'
source_filename = "vm_virtualize_lifetime_intrinsics.ll"

declare void @llvm.lifetime.start.p0(i64 immarg, ptr nocapture)
declare void @llvm.lifetime.end.p0(i64 immarg, ptr nocapture)

define i32 @vm_lifetime_intrinsics(i32 %seed) {
entry:
  %slot = alloca i32, align 4
  call void @llvm.lifetime.start.p0(i64 4, ptr %slot)
  store i32 %seed, ptr %slot, align 4
  %loaded = load i32, ptr %slot, align 4
  %mixed = xor i32 %loaded, 305419896
  call void @llvm.lifetime.end.p0(i64 4, ptr %slot)
  ret i32 %mixed
}
"#,
    )
    .expect("lifetime intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_lifetime_intrinsics_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdio.h>
int vm_lifetime_intrinsics(int seed);
int main(void) {
    int result = vm_lifetime_intrinsics(0x13572468);
    printf("%d\n", result);
    return 0;
}
"#,
    )
    .expect("lifetime intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_lifetime_intrinsics_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized_ir = optimize_ir_with_plugin(
        &ir_source,
        "vm_virtualize_lifetime_intrinsics.ll",
        vm_virtualize_config(),
    );
    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_lifetime_intrinsics");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized lifetime LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_lifetime_intrinsics"));
    assert!(ir.contains("handler.fake_nop"));
}

#[test]
#[serial]
fn test_vm_virtualize_assume_and_debug_intrinsics_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_assume_debug_intrinsics.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_assume_debug_intrinsics'
source_filename = "vm_virtualize_assume_debug_intrinsics.ll"

declare void @llvm.assume(i1)
declare void @llvm.dbg.value(metadata, metadata, metadata)

define i32 @vm_assume_debug_intrinsics(i32 %seed) !dbg !4 {
entry:
  %nonzero = icmp ne i32 %seed, 0
  call void @llvm.assume(i1 %nonzero)
  call void @llvm.dbg.value(metadata i32 %seed, metadata !9, metadata !DIExpression()), !dbg !10
  %mixed = xor i32 %seed, 324508639
  %rot = call i32 @llvm.fshl.i32(i32 %mixed, i32 %mixed, i32 7)
  ret i32 %rot
}

declare i32 @llvm.fshl.i32(i32, i32, i32)

!llvm.dbg.cu = !{!1}
!llvm.module.flags = !{!0}

!0 = !{i32 2, !"Debug Info Version", i32 3}
!1 = distinct !DICompileUnit(language: DW_LANG_C11, file: !2, producer: "amice-test", isOptimized: true, runtimeVersion: 0, emissionKind: FullDebug, enums: !3)
!2 = !DIFile(filename: "vm_virtualize_assume_debug_intrinsics.c", directory: "/tmp")
!3 = !{}
!4 = distinct !DISubprogram(name: "vm_assume_debug_intrinsics", scope: !2, file: !2, line: 1, type: !5, scopeLine: 1, flags: DIFlagPrototyped, spFlags: DISPFlagDefinition | DISPFlagOptimized, unit: !1, retainedNodes: !8)
!5 = !DISubroutineType(types: !6)
!6 = !{!7, !7}
!7 = !DIBasicType(name: "int", size: 32, encoding: DW_ATE_signed)
!8 = !{!9}
!9 = !DILocalVariable(name: "seed", arg: 1, scope: !4, file: !2, line: 1, type: !7)
!10 = !DILocation(line: 1, column: 1, scope: !4)
"#,
    )
    .expect("assume/debug intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_assume_debug_intrinsics_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdio.h>
int vm_assume_debug_intrinsics(int seed);
int main(void) {
    int result = vm_assume_debug_intrinsics(0x13572468);
    printf("%d\n", result);
    return 0;
}
"#,
    )
    .expect("assume/debug intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_assume_debug_intrinsics_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized_ir = optimize_ir_with_plugin(
        &ir_source,
        "vm_virtualize_assume_debug_intrinsics.ll",
        vm_virtualize_config(),
    );
    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_assume_debug_intrinsics");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized assume/debug LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_assume_debug_intrinsics"));
    assert!(ir.contains("handler.fake_nop"));
}

#[test]
#[serial]
fn test_vm_virtualize_fixed_memory_intrinsics_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_memory_intrinsics.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_memory_intrinsics'
source_filename = "vm_virtualize_memory_intrinsics.ll"

declare void @llvm.memset.p0.i64(ptr, i8, i64, i1 immarg)
declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1 immarg)
declare void @llvm.memmove.p0.p0.i64(ptr, ptr, i64, i1 immarg)

define i32 @vm_memory_intrinsics(i32 %seed) {
entry:
  %src = alloca [16 x i8], align 8
  %dst = alloca [16 x i8], align 8
  %srcp = getelementptr inbounds [16 x i8], ptr %src, i64 0, i64 0
  %dstp = getelementptr inbounds [16 x i8], ptr %dst, i64 0, i64 0
  call void @llvm.memset.p0.i64(ptr %srcp, i8 0, i64 16, i1 false)
  %b0 = trunc i32 %seed to i8
  store i8 %b0, ptr %srcp, align 1
  %s1 = lshr i32 %seed, 8
  %b1 = trunc i32 %s1 to i8
  %src1 = getelementptr inbounds i8, ptr %srcp, i64 1
  store i8 %b1, ptr %src1, align 1
  %s2 = lshr i32 %seed, 16
  %b2 = trunc i32 %s2 to i8
  %src2 = getelementptr inbounds i8, ptr %srcp, i64 2
  store i8 %b2, ptr %src2, align 1
  %s3 = lshr i32 %seed, 24
  %b3 = trunc i32 %s3 to i8
  %src3 = getelementptr inbounds i8, ptr %srcp, i64 3
  store i8 %b3, ptr %src3, align 1
  %src4 = getelementptr inbounds i8, ptr %srcp, i64 4
  store i8 17, ptr %src4, align 1
  %src5 = getelementptr inbounds i8, ptr %srcp, i64 5
  store i8 34, ptr %src5, align 1
  %src6 = getelementptr inbounds i8, ptr %srcp, i64 6
  store i8 51, ptr %src6, align 1
  %src7 = getelementptr inbounds i8, ptr %srcp, i64 7
  store i8 68, ptr %src7, align 1
  call void @llvm.memcpy.p0.p0.i64(ptr %dstp, ptr %srcp, i64 8, i1 false)
  %dst2 = getelementptr inbounds i8, ptr %dstp, i64 2
  call void @llvm.memmove.p0.p0.i64(ptr %dst2, ptr %dstp, i64 6, i1 false)
  %dst10 = getelementptr inbounds i8, ptr %dstp, i64 10
  call void @llvm.memset.p0.i64(ptr %dst10, i8 90, i64 3, i1 false)
  %l0 = load i8, ptr %dstp, align 1
  %l0x = zext i8 %l0 to i32
  %p1 = getelementptr inbounds i8, ptr %dstp, i64 1
  %l1 = load i8, ptr %p1, align 1
  %l1x = zext i8 %l1 to i32
  %a1 = add i32 %l0x, %l1x
  %p2 = getelementptr inbounds i8, ptr %dstp, i64 2
  %l2 = load i8, ptr %p2, align 1
  %l2x = zext i8 %l2 to i32
  %a2 = add i32 %a1, %l2x
  %p5 = getelementptr inbounds i8, ptr %dstp, i64 5
  %l5 = load i8, ptr %p5, align 1
  %l5x = zext i8 %l5 to i32
  %a3 = add i32 %a2, %l5x
  %p7 = getelementptr inbounds i8, ptr %dstp, i64 7
  %l7 = load i8, ptr %p7, align 1
  %l7x = zext i8 %l7 to i32
  %a4 = add i32 %a3, %l7x
  %p10 = getelementptr inbounds i8, ptr %dstp, i64 10
  %l10 = load i8, ptr %p10, align 1
  %l10x = zext i8 %l10 to i32
  %a5 = add i32 %a4, %l10x
  %p12 = getelementptr inbounds i8, ptr %dstp, i64 12
  %l12 = load i8, ptr %p12, align 1
  %l12x = zext i8 %l12 to i32
  %a6 = add i32 %a5, %l12x
  ret i32 %a6
}
"#,
    )
    .expect("memory intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_memory_intrinsics_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdio.h>
int vm_memory_intrinsics(int seed);
int main(void) {
    int acc = 0;
    acc += vm_memory_intrinsics(0x12345678);
    acc += vm_memory_intrinsics(0xa5b6c7d8);
    printf("%d\n", acc);
    return 0;
}
"#,
    )
    .expect("memory intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_memory_intrinsics_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized_ir =
        optimize_ir_with_plugin(&ir_source, "vm_virtualize_memory_intrinsics.ll", vm_virtualize_config());
    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_memory_intrinsics");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized memory intrinsic IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_memory_intrinsics"));
    assert!(ir.contains("handler.load"));
    assert!(ir.contains("handler.store"));
    assert!(ir.contains("handler.gep"));
    assert!(!ir.contains("call void @llvm.mem"));
}

#[test]
#[serial]
fn test_vm_virtualize_memcpy_inline_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_memcpy_inline.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_memcpy_inline'
source_filename = "vm_virtualize_memcpy_inline.ll"

declare void @llvm.memcpy.inline.p0.p0.i64(ptr, ptr, i64 immarg, i1 immarg)

define i32 @vm_memcpy_inline(i32 %seed) #0 {
entry:
  %src = alloca [16 x i8], align 8
  %dst = alloca [16 x i8], align 8
  %srcp = getelementptr inbounds [16 x i8], ptr %src, i64 0, i64 0
  %dstp = getelementptr inbounds [16 x i8], ptr %dst, i64 0, i64 0
  %b0 = trunc i32 %seed to i8
  store i8 %b0, ptr %srcp, align 1
  %s1 = lshr i32 %seed, 8
  %b1 = trunc i32 %s1 to i8
  %src1 = getelementptr inbounds i8, ptr %srcp, i64 1
  store i8 %b1, ptr %src1, align 1
  %s2 = lshr i32 %seed, 16
  %b2 = trunc i32 %s2 to i8
  %src2 = getelementptr inbounds i8, ptr %srcp, i64 2
  store i8 %b2, ptr %src2, align 1
  %s3 = lshr i32 %seed, 24
  %b3 = trunc i32 %s3 to i8
  %src3 = getelementptr inbounds i8, ptr %srcp, i64 3
  store i8 %b3, ptr %src3, align 1
  %src4 = getelementptr inbounds i8, ptr %srcp, i64 4
  store i8 17, ptr %src4, align 1
  %src5 = getelementptr inbounds i8, ptr %srcp, i64 5
  store i8 34, ptr %src5, align 1
  %src6 = getelementptr inbounds i8, ptr %srcp, i64 6
  store i8 51, ptr %src6, align 1
  %src7 = getelementptr inbounds i8, ptr %srcp, i64 7
  store i8 68, ptr %src7, align 1
  call void @llvm.memcpy.inline.p0.p0.i64(ptr %dstp, ptr %srcp, i64 8, i1 false)
  %p0 = load i8, ptr %dstp, align 1
  %p1 = getelementptr inbounds i8, ptr %dstp, i64 1
  %v1 = load i8, ptr %p1, align 1
  %p4 = getelementptr inbounds i8, ptr %dstp, i64 4
  %v4 = load i8, ptr %p4, align 1
  %p7 = getelementptr inbounds i8, ptr %dstp, i64 7
  %v7 = load i8, ptr %p7, align 1
  %x0 = zext i8 %p0 to i32
  %x1 = zext i8 %v1 to i32
  %x4 = zext i8 %v4 to i32
  %x7 = zext i8 %v7 to i32
  %a = shl i32 %x0, 24
  %b = shl i32 %x1, 16
  %c = shl i32 %x4, 8
  %d = xor i32 %a, %b
  %e = xor i32 %d, %c
  %f = xor i32 %e, %x7
  ret i32 %f
}

attributes #0 = { noinline optnone }
"#,
    )
    .expect("memcpy.inline LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_memcpy_inline_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_memcpy_inline(int32_t seed);

int main(void) {
    int32_t a = vm_memcpy_inline(0x12345678);
    int32_t b = vm_memcpy_inline(0xa5b6c7d8);
    printf("%d:%d\n", a, b);
    return 0;
}
"#,
    )
    .expect("memcpy.inline C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_memcpy_inline_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) = optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_memcpy_inline.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_memcpy_inline");
    assert!(
        dump.contains(": load ") && dump.contains(": store ") && dump.contains(": gep "),
        "memcpy.inline should lower through profile load/store/gep actions:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_memcpy_inline");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("memcpy.inline output IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_memcpy_inline"));
    assert!(ir.contains("handler.load"));
    assert!(ir.contains("handler.store"));
    assert!(ir.contains("handler.gep"));
    assert!(!ir.contains("call void @llvm.memcpy.inline"));
}

#[test]
#[serial]
fn test_vm_virtualize_memset_inline_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_memset_inline.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_memset_inline'
source_filename = "vm_virtualize_memset_inline.ll"

declare void @llvm.memset.inline.p0.i64(ptr, i8, i64 immarg, i1 immarg)

define i32 @vm_memset_inline(i8 %fill, i32 %seed) #0 {
entry:
  %buf = alloca [16 x i8], align 8
  %base = getelementptr inbounds [16 x i8], ptr %buf, i64 0, i64 0
  call void @llvm.memset.inline.p0.i64(ptr %base, i8 %fill, i64 8, i1 false)
  %b0 = trunc i32 %seed to i8
  store i8 %b0, ptr %base, align 1
  %p3 = getelementptr inbounds i8, ptr %base, i64 3
  store i8 90, ptr %p3, align 1
  %p0v = load i8, ptr %base, align 1
  %p1 = getelementptr inbounds i8, ptr %base, i64 1
  %p1v = load i8, ptr %p1, align 1
  %p3v = load i8, ptr %p3, align 1
  %p7 = getelementptr inbounds i8, ptr %base, i64 7
  %p7v = load i8, ptr %p7, align 1
  %x0 = zext i8 %p0v to i32
  %x1 = zext i8 %p1v to i32
  %x3 = zext i8 %p3v to i32
  %x7 = zext i8 %p7v to i32
  %a = shl i32 %x0, 24
  %b = shl i32 %x1, 16
  %c = shl i32 %x3, 8
  %d = xor i32 %a, %b
  %e = xor i32 %d, %c
  %f = xor i32 %e, %x7
  ret i32 %f
}

attributes #0 = { noinline optnone }
"#,
    )
    .expect("memset.inline LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_memset_inline_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_memset_inline(uint8_t fill, int32_t seed);

int main(void) {
    int32_t a = vm_memset_inline(0x11, 0x12345678);
    int32_t b = vm_memset_inline(0xa5, 0x66778899);
    printf("%d:%d\n", a, b);
    return 0;
}
"#,
    )
    .expect("memset.inline C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_memset_inline_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) = optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_memset_inline.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_memset_inline");
    assert!(
        dump.contains(": store ") && dump.contains(": gep "),
        "memset.inline should lower through profile store/gep actions:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_memset_inline");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("memset.inline output IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_memset_inline"));
    assert!(ir.contains("handler.store"));
    assert!(ir.contains("handler.gep"));
    assert!(!ir.contains("call void @llvm.memset.inline"));
}

#[test]
#[serial]
fn test_vm_virtualize_large_fixed_memory_intrinsics_use_dynamic_handlers() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_large_fixed_memory_intrinsics.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_large_fixed_memory_intrinsics'
source_filename = "vm_virtualize_large_fixed_memory_intrinsics.ll"

declare void @llvm.memset.p0.i64(ptr, i8, i64, i1 immarg)
declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1 immarg)
declare void @llvm.memmove.p0.p0.i64(ptr, ptr, i64, i1 immarg)

define i32 @vm_large_fixed_memcpy(i8 %fill) {
entry:
  %src = alloca [128 x i8], align 16
  %dst = alloca [128 x i8], align 16
  %srcp = getelementptr inbounds [128 x i8], ptr %src, i64 0, i64 0
  %dstp = getelementptr inbounds [128 x i8], ptr %dst, i64 0, i64 0
  call void @llvm.memset.p0.i64(ptr %srcp, i8 %fill, i64 96, i1 false)
  %src63 = getelementptr inbounds i8, ptr %srcp, i64 63
  store i8 42, ptr %src63, align 1
  %src95 = getelementptr inbounds i8, ptr %srcp, i64 95
  store i8 77, ptr %src95, align 1
  call void @llvm.memcpy.p0.p0.i64(ptr %dstp, ptr %srcp, i64 96, i1 false)
  %dst63 = getelementptr inbounds i8, ptr %dstp, i64 63
  %dst95 = getelementptr inbounds i8, ptr %dstp, i64 95
  %b0 = load i8, ptr %dstp, align 1
  %b63 = load i8, ptr %dst63, align 1
  %b95 = load i8, ptr %dst95, align 1
  %x0 = zext i8 %b0 to i32
  %x63 = zext i8 %b63 to i32
  %x95 = zext i8 %b95 to i32
  %a = shl i32 %x0, 16
  %b = shl i32 %x63, 8
  %c = xor i32 %a, %b
  %d = xor i32 %c, %x95
  ret i32 %d
}

define i32 @vm_large_fixed_memmove(i8 %fill) {
entry:
  %buf = alloca [160 x i8], align 16
  %base = getelementptr inbounds [160 x i8], ptr %buf, i64 0, i64 0
  call void @llvm.memset.p0.i64(ptr %base, i8 %fill, i64 128, i1 false)
  store i8 13, ptr %base, align 1
  %src63 = getelementptr inbounds i8, ptr %base, i64 63
  store i8 29, ptr %src63, align 1
  %src95 = getelementptr inbounds i8, ptr %base, i64 95
  store i8 71, ptr %src95, align 1
  %dst = getelementptr inbounds i8, ptr %base, i64 32
  call void @llvm.memmove.p0.p0.i64(ptr %dst, ptr %base, i64 96, i1 false)
  %p32 = getelementptr inbounds i8, ptr %base, i64 32
  %p95 = getelementptr inbounds i8, ptr %base, i64 95
  %p127 = getelementptr inbounds i8, ptr %base, i64 127
  %b0 = load i8, ptr %base, align 1
  %b32 = load i8, ptr %p32, align 1
  %b95 = load i8, ptr %p95, align 1
  %b127 = load i8, ptr %p127, align 1
  %x0 = zext i8 %b0 to i32
  %x32 = zext i8 %b32 to i32
  %x95 = zext i8 %b95 to i32
  %x127 = zext i8 %b127 to i32
  %a = shl i32 %x0, 24
  %b = shl i32 %x32, 16
  %c = shl i32 %x95, 8
  %d = xor i32 %a, %b
  %e = xor i32 %d, %c
  %f = xor i32 %e, %x127
  ret i32 %f
}
"#,
    )
    .expect("large fixed memory intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_large_fixed_memory_intrinsics_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_large_fixed_memcpy(uint8_t fill);
int32_t vm_large_fixed_memmove(uint8_t fill);

int main(void) {
    int32_t a = vm_large_fixed_memcpy(0x5a);
    int32_t b = vm_large_fixed_memmove(0x33);
    printf("%d:%d\n", a, b);
    return 0;
}
"#,
    )
    .expect("large fixed memory intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(
        &ir_source,
        &harness,
        "vm_virtualize_large_fixed_memory_intrinsics_baseline",
    );
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) = optimize_ir_with_plugin_debug_pipeline(
        &ir_source,
        "vm_virtualize_large_fixed_memory_intrinsics.ll",
        "default<O0>",
        config,
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);

    let memcpy_dump = bytecode_dump_for_function(&stderr, "vm_large_fixed_memcpy");
    assert!(
        memcpy_dump.contains(": memset_dyn ") && memcpy_dump.contains(": memcpy_dyn "),
        "large fixed memset/memcpy should lower through dynamic profile handlers:\n{memcpy_dump}"
    );
    let memmove_dump = bytecode_dump_for_function(&stderr, "vm_large_fixed_memmove");
    assert!(
        memmove_dump.contains(": memset_dyn ") && memmove_dump.contains(": memmove_dyn "),
        "large fixed memset/memmove should lower through dynamic profile handlers:\n{memmove_dump}"
    );

    let virtualized =
        compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_large_fixed_memory_intrinsics");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("large fixed memory intrinsic IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_large_fixed_memcpy"));
    assert!(ir.contains(".amice.vm.bytecode.vm_large_fixed_memmove"));
    assert!(ir.contains("handler.memset_dyn"));
    assert!(ir.contains("handler.memcpy_dyn"));
    assert!(ir.contains("handler.memmove_dyn"));
}

#[test]
#[serial]
fn test_vm_virtualize_dynamic_memset_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_dynamic_memset.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_dynamic_memset'
source_filename = "vm_virtualize_dynamic_memset.ll"

declare void @llvm.memset.p0.i64(ptr, i8, i64, i1 immarg)

define i32 @vm_dynamic_memset(i64 %n, i8 %value) {
entry:
  %bounded = and i64 %n, 15
  %len = add nuw nsw i64 %bounded, 1
  %buf = alloca [16 x i8], align 16
  %base = getelementptr inbounds [16 x i8], ptr %buf, i64 0, i64 0
  call void @llvm.memset.p0.i64(ptr %base, i8 %value, i64 %len, i1 false)
  %last_index = add nsw i64 %len, -1
  %tail = getelementptr inbounds i8, ptr %base, i64 %last_index
  %first = load i8, ptr %base, align 1
  %last = load i8, ptr %tail, align 1
  %first32 = zext i8 %first to i32
  %last32 = zext i8 %last to i32
  %len32 = trunc i64 %len to i32
  %shifted = shl i32 %first32, 8
  %mixed0 = xor i32 %shifted, %last32
  %mixed1 = xor i32 %mixed0, %len32
  ret i32 %mixed1
}
"#,
    )
    .expect("dynamic memset LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_dynamic_memset_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_dynamic_memset(uint64_t n, uint8_t value);

int main(void) {
    int32_t acc = 0;
    acc ^= vm_dynamic_memset(0, 0x11);
    acc ^= vm_dynamic_memset(7, 0x80);
    acc ^= vm_dynamic_memset(31, 0x5a);
    printf("%d\n", acc);
    return 0;
}
"#,
    )
    .expect("dynamic memset C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_dynamic_memset_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) = optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_dynamic_memset.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_dynamic_memset");
    assert!(
        dump.contains(": memset_dyn "),
        "dynamic memset should lower through profile memset_dyn:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_dynamic_memset");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized dynamic memset LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_dynamic_memset"));
    assert!(ir.contains("handler.memset_dyn"));
}

#[test]
#[serial]
fn test_vm_virtualize_struct_array_dynamic_gep_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_struct_array_gep.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_struct_array_gep'
source_filename = "vm_virtualize_struct_array_gep.ll"

%Pair = type { i32, [6 x i32] }

define i32 @vm_struct_array_gep(ptr %base, i64 %i, i64 %j) {
entry:
  %arr = getelementptr inbounds %Pair, ptr %base, i64 %i, i32 1, i64 %j
  %value = load i32, ptr %arr, align 4
  ret i32 %value
}
"#,
    )
    .expect("struct-array GEP LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_struct_array_gep_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdio.h>

struct Pair {
    int tag;
    int values[6];
};

int vm_struct_array_gep(struct Pair *base, long i, long j);

int main(void) {
    struct Pair pairs[3];
    for (int i = 0; i < 3; ++i) {
        pairs[i].tag = 100 + i;
        for (int j = 0; j < 6; ++j) {
            pairs[i].values[j] = (i + 1) * 1000 + j * 17;
        }
    }

    int acc = 0;
    acc += vm_struct_array_gep(pairs, 0, 5);
    acc += vm_struct_array_gep(pairs, 1, 3);
    acc += vm_struct_array_gep(pairs, 2, 1);
    printf("%d\n", acc);
    return 0;
}
"#,
    )
    .expect("struct-array GEP C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_struct_array_gep_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized_ir =
        optimize_ir_with_plugin(&ir_source, "vm_virtualize_struct_array_gep.ll", vm_virtualize_config());
    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_struct_array_gep");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized struct-array GEP IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_struct_array_gep"));
    assert!(ir.contains("handler.imul"));
    assert!(ir.contains("handler.iadd"));
    assert!(ir.contains("handler.gep"));
}

#[test]
#[serial]
fn test_vm_virtualize_nested_aggregate_insert_extract_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_nested_aggregate.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_nested_aggregate'
source_filename = "vm_virtualize_nested_aggregate.ll"

%Inner = type { i32, i64 }
%Outer = type { i8, %Inner, [2 x i16] }

define i64 @vm_nested_aggregate(i32 %a, i64 %b, i16 %c, i16 %d) #0 {
entry:
  %o0 = insertvalue %Outer undef, i8 11, 0
  %o1 = insertvalue %Outer %o0, i32 %a, 1, 0
  %o2 = insertvalue %Outer %o1, i64 %b, 1, 1
  %o3 = insertvalue %Outer %o2, i16 %c, 2, 0
  %o4 = insertvalue %Outer %o3, i16 %d, 2, 1
  %x = extractvalue %Outer %o4, 1, 0
  %y = extractvalue %Outer %o4, 1, 1
  %p = extractvalue %Outer %o4, 2, 0
  %q = extractvalue %Outer %o4, 2, 1
  %xx = sext i32 %x to i64
  %px = zext i16 %p to i64
  %qx = zext i16 %q to i64
  %r0 = xor i64 %xx, %y
  %r1 = add i64 %r0, %px
  %r2 = xor i64 %r1, %qx
  ret i64 %r2
}

attributes #0 = { noinline optnone }
"#,
    )
    .expect("nested aggregate LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_nested_aggregate_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_nested_aggregate(int32_t a, uint64_t b, uint16_t c, uint16_t d);

int main(void) {
    uint64_t acc = 0;
    acc ^= vm_nested_aggregate(0x12345678, 0x0123456789abcdefULL, 0x1357u, 0x2468u);
    acc ^= vm_nested_aggregate(-17, 0xfedcba9876543210ULL, 0xabcdU, 0x4321U);
    printf("%llu\n", (unsigned long long)acc);
    return 0;
}
"#,
    )
    .expect("nested aggregate C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_nested_aggregate_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_nested_aggregate.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_nested_aggregate");
    assert!(
        dump.contains(": mov "),
        "nested aggregate insert/extract should use profile mov actions:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_nested_aggregate");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized nested aggregate IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_nested_aggregate"));
    assert!(ir.contains("handler.mov"));
}

#[test]
#[serial]
fn test_vm_virtualize_aggregate_freeze_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_aggregate_freeze.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_aggregate_freeze'
source_filename = "vm_virtualize_aggregate_freeze.ll"

%Agg = type { ptr, i32, float, i64, i16 }

define i64 @vm_aggregate_freeze(ptr %p, i32 %a, float %f, i64 %b) #0 {
entry:
  %g0 = insertvalue %Agg undef, ptr %p, 0
  %g1 = insertvalue %Agg %g0, i32 %a, 1
  %g2 = insertvalue %Agg %g1, float %f, 2
  %g3 = insertvalue %Agg %g2, i64 %b, 3
  %fr = freeze %Agg %g3
  %rp = extractvalue %Agg %fr, 0
  %ra = extractvalue %Agg %fr, 1
  %rf = extractvalue %Agg %fr, 2
  %rb = extractvalue %Agg %fr, 3
  %loaded = load i32, ptr %rp, align 4
  %fi = fptosi float %rf to i32
  %ax = sext i32 %ra to i64
  %lx = sext i32 %loaded to i64
  %fx = sext i32 %fi to i64
  %m0 = xor i64 %rb, %ax
  %m1 = add i64 %m0, %lx
  %m2 = xor i64 %m1, %fx
  ret i64 %m2
}

attributes #0 = { noinline optnone }
"#,
    )
    .expect("aggregate freeze LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_aggregate_freeze_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_aggregate_freeze(int32_t *p, int32_t a, float f, uint64_t b);

int main(void) {
    int32_t cells[2] = { 0x13572468, -77 };
    uint64_t acc = 0;
    acc ^= vm_aggregate_freeze(&cells[0], 0x12345678, 19.75f, 0x0123456789abcdefULL);
    acc ^= vm_aggregate_freeze(&cells[1], -33, -8.50f, 0xfedcba9876543210ULL);
    printf("%llu\n", (unsigned long long)acc);
    return 0;
}
"#,
    )
    .expect("aggregate freeze C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_aggregate_freeze_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_aggregate_freeze.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_aggregate_freeze");
    assert!(
        dump.contains(": mov "),
        "aggregate freeze should lower each live field through profile mov actions:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_aggregate_freeze");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized aggregate freeze IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_aggregate_freeze"));
    assert!(ir.contains("handler.mov"));
}

#[test]
#[serial]
fn test_vm_virtualize_aggregate_load_store_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_aggregate_load_store.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_aggregate_load_store'
source_filename = "vm_virtualize_aggregate_load_store.ll"

%Inner = type { i16, i16 }
%Outer = type { i8, %Inner, [2 x i32], ptr }

define i64 @vm_aggregate_load_store(ptr %dst, ptr %src, ptr %replacement) #0 {
entry:
  %loaded = load %Outer, ptr %src, align 8
  %tag = extractvalue %Outer %loaded, 0
  %lo = extractvalue %Outer %loaded, 1, 0
  %hi = extractvalue %Outer %loaded, 1, 1
  %x = extractvalue %Outer %loaded, 2, 0
  %y = extractvalue %Outer %loaded, 2, 1
  %ptrv = extractvalue %Outer %loaded, 3
  %x2 = add i32 %x, 17
  %y2 = xor i32 %y, %x
  %o0 = insertvalue %Outer %loaded, i32 %x2, 2, 0
  %o1 = insertvalue %Outer %o0, i32 %y2, 2, 1
  %o2 = insertvalue %Outer %o1, ptr %replacement, 3
  store %Outer %o2, ptr %dst, align 8
  %tag64 = zext i8 %tag to i64
  %lo64 = zext i16 %lo to i64
  %hi64 = zext i16 %hi to i64
  %x64 = zext i32 %x to i64
  %y64 = zext i32 %y to i64
  %nonnull = icmp ne ptr %ptrv, null
  %ptrflag = zext i1 %nonnull to i64
  %m0 = shl i64 %lo64, 8
  %m1 = xor i64 %tag64, %m0
  %m2 = shl i64 %hi64, 24
  %m3 = xor i64 %m1, %m2
  %m4 = shl i64 %x64, 32
  %m5 = xor i64 %m3, %m4
  %m6 = xor i64 %m5, %y64
  %m7 = xor i64 %m6, %ptrflag
  ret i64 %m7
}

attributes #0 = { noinline optnone }
"#,
    )
    .expect("aggregate load/store LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_aggregate_load_store_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

struct Inner {
    uint16_t lo;
    uint16_t hi;
};

struct Outer {
    uint8_t tag;
    struct Inner inner;
    uint32_t words[2];
    uintptr_t ptr;
};

uint64_t vm_aggregate_load_store(struct Outer *dst, const struct Outer *src, void *replacement);

static void print_case(uint64_t value, const struct Outer *out, const void *replacement) {
    printf("%llu %u %u %u %u %u %d\n",
           (unsigned long long)value,
           (unsigned)out->tag,
           (unsigned)out->inner.lo,
           (unsigned)out->inner.hi,
           (unsigned)out->words[0],
           (unsigned)out->words[1],
           out->ptr == (uintptr_t)replacement);
}

int main(void) {
    int marker_a = 0;
    int marker_b = 0;
    int original_a = 0;
    int original_b = 0;
    struct Outer inputs[2] = {
        { 7, { 0x1234u, 0x4567u }, { 0x89abcdefu, 0x10203040u }, (uintptr_t)&original_a },
        { 19, { 0x0102u, 0x0304u }, { 0x55667788u, 0xaabbccddu }, (uintptr_t)&original_b },
    };
    struct Outer out = { 0 };

    uint64_t first = vm_aggregate_load_store(&out, &inputs[0], &marker_a);
    print_case(first, &out, &marker_a);
    uint64_t second = vm_aggregate_load_store(&out, &inputs[1], &marker_b);
    print_case(second, &out, &marker_b);
    return 0;
}
"#,
    )
    .expect("aggregate load/store C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_aggregate_load_store_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_aggregate_load_store.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_aggregate_load_store");
    assert!(
        dump.contains(": load ") && dump.contains(": store ") && dump.contains(": gep ") && dump.contains(": mov "),
        "aggregate load/store should expand through profile load/store/gep/mov actions:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_aggregate_load_store");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized aggregate load/store IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_aggregate_load_store"));
    assert!(ir.contains("handler.load"));
    assert!(ir.contains("handler.store"));
    assert!(ir.contains("handler.gep"));
    assert!(ir.contains("handler.mov"));
}

#[test]
#[serial]
fn test_vm_virtualize_volatile_aggregate_load_store_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_volatile_aggregate_load_store.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_volatile_aggregate_load_store'
source_filename = "vm_virtualize_volatile_aggregate_load_store.ll"

%Inner = type { i16, i16 }
%Outer = type { i8, %Inner, [2 x i32], ptr }

define i64 @vm_volatile_aggregate_load_store(ptr %dst, ptr %src, ptr %replacement) #0 {
entry:
  %loaded = load volatile %Outer, ptr %src, align 8
  %tag = extractvalue %Outer %loaded, 0
  %lo = extractvalue %Outer %loaded, 1, 0
  %hi = extractvalue %Outer %loaded, 1, 1
  %x = extractvalue %Outer %loaded, 2, 0
  %y = extractvalue %Outer %loaded, 2, 1
  %x2 = xor i32 %x, 324508639
  %y2 = add i32 %y, %x
  %o0 = insertvalue %Outer %loaded, i32 %x2, 2, 0
  %o1 = insertvalue %Outer %o0, i32 %y2, 2, 1
  %o2 = insertvalue %Outer %o1, ptr %replacement, 3
  store volatile %Outer %o2, ptr %dst, align 8
  %tag64 = zext i8 %tag to i64
  %lo64 = zext i16 %lo to i64
  %hi64 = zext i16 %hi to i64
  %x64 = zext i32 %x to i64
  %y64 = zext i32 %y to i64
  %m0 = shl i64 %lo64, 8
  %m1 = xor i64 %tag64, %m0
  %m2 = shl i64 %hi64, 24
  %m3 = xor i64 %m1, %m2
  %m4 = shl i64 %x64, 32
  %m5 = xor i64 %m3, %m4
  %m6 = xor i64 %m5, %y64
  ret i64 %m6
}

attributes #0 = { noinline optnone }
"#,
    )
    .expect("volatile aggregate load/store LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_volatile_aggregate_load_store_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

struct Inner {
    uint16_t lo;
    uint16_t hi;
};

struct Outer {
    uint8_t tag;
    struct Inner inner;
    uint32_t words[2];
    uintptr_t ptr;
};

uint64_t vm_volatile_aggregate_load_store(struct Outer *dst, const struct Outer *src, void *replacement);

static void print_case(uint64_t value, const struct Outer *out, const void *replacement) {
    printf("%llu %u %u %u %u %u %d\n",
           (unsigned long long)value,
           (unsigned)out->tag,
           (unsigned)out->inner.lo,
           (unsigned)out->inner.hi,
           (unsigned)out->words[0],
           (unsigned)out->words[1],
           out->ptr == (uintptr_t)replacement);
}

int main(void) {
    int marker_a = 0;
    int marker_b = 0;
    int original_a = 0;
    int original_b = 0;
    struct Outer inputs[2] = {
        { 31, { 0x2222u, 0x3333u }, { 0x12345678u, 0x11112222u }, (uintptr_t)&original_a },
        { 43, { 0x4444u, 0x5555u }, { 0xabcdef01u, 0x99887766u }, (uintptr_t)&original_b },
    };
    struct Outer out = { 0 };

    uint64_t first = vm_volatile_aggregate_load_store(&out, &inputs[0], &marker_a);
    print_case(first, &out, &marker_a);
    uint64_t second = vm_volatile_aggregate_load_store(&out, &inputs[1], &marker_b);
    print_case(second, &out, &marker_b);
    return 0;
}
"#,
    )
    .expect("volatile aggregate load/store C harness should be writable");

    let baseline = compile_ir_with_c_harness(
        &ir_source,
        &harness,
        "vm_virtualize_volatile_aggregate_load_store_baseline",
    );
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_volatile_aggregate_load_store.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_volatile_aggregate_load_store");
    assert!(
        dump.contains(": volatile_load ")
            && dump.contains(": volatile_store ")
            && dump.contains(": gep ")
            && dump.contains(": mov "),
        "volatile aggregate load/store should expand through profile volatile_load/volatile_store/gep/mov actions:\n{dump}"
    );

    let virtualized =
        compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_volatile_aggregate_load_store");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir)
        .expect("virtualized volatile aggregate load/store IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_volatile_aggregate_load_store"));
    assert!(ir.contains("handler.volatile_load"));
    assert!(ir.contains("handler.volatile_store"));
    assert!(ir.contains("handler.gep"));
    assert!(ir.contains("handler.mov"));
}

#[test]
#[serial]
fn test_vm_virtualize_direct_aggregate_parameter_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_aggregate_param.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_aggregate_param'
source_filename = "vm_virtualize_aggregate_param.ll"

%Inner = type { i16, i16 }
%Arg = type { i8, %Inner, [2 x i32], ptr, float }

@fmt = private unnamed_addr constant [6 x i8] c"%llu\0A\00"

declare i32 @printf(ptr, ...)

define i64 @vm_aggregate_param(%Arg %arg, i64 %salt) #0 {
entry:
  %tag = extractvalue %Arg %arg, 0
  %lo = extractvalue %Arg %arg, 1, 0
  %hi = extractvalue %Arg %arg, 1, 1
  %x = extractvalue %Arg %arg, 2, 0
  %y = extractvalue %Arg %arg, 2, 1
  %ptrv = extractvalue %Arg %arg, 3
  %fv = extractvalue %Arg %arg, 4
  %tag64 = zext i8 %tag to i64
  %lo64 = zext i16 %lo to i64
  %hi64 = zext i16 %hi to i64
  %x64 = zext i32 %x to i64
  %y64 = zext i32 %y to i64
  %fi = fptosi float %fv to i32
  %fi64 = sext i32 %fi to i64
  %nonnull = icmp ne ptr %ptrv, null
  %ptrflag = zext i1 %nonnull to i64
  %m0 = shl i64 %lo64, 8
  %m1 = xor i64 %tag64, %m0
  %m2 = shl i64 %hi64, 24
  %m3 = xor i64 %m1, %m2
  %m4 = shl i64 %x64, 32
  %m5 = xor i64 %m3, %m4
  %m6 = add i64 %m5, %y64
  %m7 = xor i64 %m6, %fi64
  %m8 = xor i64 %m7, %ptrflag
  %m9 = add i64 %m8, %salt
  ret i64 %m9
}

define i32 @main() {
entry:
  %a0 = insertvalue %Arg undef, i8 7, 0
  %a1 = insertvalue %Arg %a0, i16 4660, 1, 0
  %a2 = insertvalue %Arg %a1, i16 17767, 1, 1
  %a3 = insertvalue %Arg %a2, i32 2309737967, 2, 0
  %a4 = insertvalue %Arg %a3, i32 270544960, 2, 1
  %a5 = insertvalue %Arg %a4, ptr @fmt, 3
  %a6 = insertvalue %Arg %a5, float 1.975000e+01, 4
  %r0 = call i64 @vm_aggregate_param(%Arg %a6, i64 12345)
  %b0 = insertvalue %Arg undef, i8 19, 0
  %b1 = insertvalue %Arg %b0, i16 258, 1, 0
  %b2 = insertvalue %Arg %b1, i16 772, 1, 1
  %b3 = insertvalue %Arg %b2, i32 1432778632, 2, 0
  %b4 = insertvalue %Arg %b3, i32 2864434397, 2, 1
  %b5 = insertvalue %Arg %b4, ptr null, 3
  %b6 = insertvalue %Arg %b5, float -8.500000e+00, 4
  %r1 = call i64 @vm_aggregate_param(%Arg %b6, i64 67890)
  %acc = xor i64 %r0, %r1
  %printed = call i32 (ptr, ...) @printf(ptr @fmt, i64 %acc)
  ret i32 0
}

attributes #0 = { noinline optnone }
"#,
    )
    .expect("aggregate parameter LLVM IR fixture should be writable");

    let baseline = compile_ir_binary(&ir_source, "vm_virtualize_aggregate_param_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_aggregate_param.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_aggregate_param");
    assert!(
        dump.contains(": mov ") && dump.contains(": fptosi "),
        "direct aggregate parameter should be extracted through VM instructions:\n{dump}"
    );

    let virtualized = compile_ir_binary(&virtualized_ir, "vm_virtualize_aggregate_param");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized aggregate parameter IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_aggregate_param"));
    assert!(ir.contains("handler.mov"));
    assert!(ir.contains("handler.fptosi"));
}

#[test]
#[serial]
fn test_vm_virtualize_subaggregate_insert_extract_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_subaggregate_insert_extract.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_subaggregate_insert_extract'
source_filename = "vm_virtualize_subaggregate_insert_extract.ll"

%Inner = type { i32, i64 }
%Pair = type { i16, i16 }
%Outer = type { i8, %Inner, [2 x i16], %Pair }

define i64 @vm_subaggregate_insert_extract(i32 %a, i64 %b, i16 %c, i16 %d, i16 %e, i16 %f) #0 {
entry:
  %i0 = insertvalue %Inner undef, i32 %a, 0
  %i1 = insertvalue %Inner %i0, i64 %b, 1
  %arr0 = insertvalue [2 x i16] undef, i16 %c, 0
  %arr1 = insertvalue [2 x i16] %arr0, i16 %d, 1
  %p0 = insertvalue %Pair undef, i16 %e, 0
  %p1 = insertvalue %Pair %p0, i16 %f, 1
  %o0 = insertvalue %Outer undef, i8 9, 0
  %o1 = insertvalue %Outer %o0, %Inner %i1, 1
  %o2 = insertvalue %Outer %o1, [2 x i16] %arr1, 2
  %o3 = insertvalue %Outer %o2, %Pair %p1, 3
  %got_inner = extractvalue %Outer %o3, 1
  %got_arr = extractvalue %Outer %o3, 2
  %got_pair = extractvalue %Outer %o3, 3
  %x = extractvalue %Inner %got_inner, 0
  %y = extractvalue %Inner %got_inner, 1
  %r = extractvalue [2 x i16] %got_arr, 0
  %s = extractvalue [2 x i16] %got_arr, 1
  %u = extractvalue %Pair %got_pair, 0
  %v = extractvalue %Pair %got_pair, 1
  %xx = sext i32 %x to i64
  %rx = zext i16 %r to i64
  %sx = zext i16 %s to i64
  %ux = zext i16 %u to i64
  %vx = zext i16 %v to i64
  %m0 = xor i64 %xx, %y
  %m1 = add i64 %m0, %rx
  %m2 = xor i64 %m1, %sx
  %m3 = add i64 %m2, %ux
  %m4 = xor i64 %m3, %vx
  ret i64 %m4
}

attributes #0 = { noinline optnone }
"#,
    )
    .expect("subaggregate insert/extract LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_subaggregate_insert_extract_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_subaggregate_insert_extract(int32_t a, uint64_t b, uint16_t c, uint16_t d, uint16_t e, uint16_t f);

int main(void) {
    uint64_t acc = 0;
    acc ^= vm_subaggregate_insert_extract(0x12345678, 0x0123456789abcdefULL, 0x1357u, 0x2468u, 0xabcdU, 0x4321U);
    acc ^= vm_subaggregate_insert_extract(-91, 0xfedcba9876543210ULL, 0x0102u, 0x0304u, 0x0506u, 0x0708u);
    printf("%llu\n", (unsigned long long)acc);
    return 0;
}
"#,
    )
    .expect("subaggregate insert/extract C harness should be writable");

    let baseline = compile_ir_with_c_harness(
        &ir_source,
        &harness,
        "vm_virtualize_subaggregate_insert_extract_baseline",
    );
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_subaggregate_insert_extract.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_subaggregate_insert_extract");
    assert!(
        dump.contains(": mov "),
        "subaggregate insert/extract should lower leaf fields through profile mov actions:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_subaggregate_insert_extract");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized subaggregate IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_subaggregate_insert_extract"));
    assert!(ir.contains("handler.mov"));
}

#[test]
#[serial]
fn test_vm_virtualize_nested_aggregate_return_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_nested_aggregate_return.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_nested_aggregate_return'
source_filename = "vm_virtualize_nested_aggregate_return.ll"

%Inner = type { i16, i16 }
%Outer = type { i32, %Inner }

define %Outer @vm_nested_aggregate_return(i32 %a, i16 %b, i16 %c) #0 {
entry:
  %o0 = insertvalue %Outer undef, i32 %a, 0
  %o1 = insertvalue %Outer %o0, i16 %b, 1, 0
  %o2 = insertvalue %Outer %o1, i16 %c, 1, 1
  ret %Outer %o2
}

attributes #0 = { noinline optnone }
"#,
    )
    .expect("nested aggregate return LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_nested_aggregate_return_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

struct Inner {
    uint16_t lo;
    uint16_t hi;
};

struct Outer {
    uint32_t tag;
    struct Inner inner;
};

struct Outer vm_nested_aggregate_return(uint32_t a, uint16_t b, uint16_t c);

int main(void) {
    struct Outer a = vm_nested_aggregate_return(0x12345678u, 0x1357u, 0x2468u);
    struct Outer b = vm_nested_aggregate_return(0x9abcdef0u, 0xabcdU, 0x4321U);
    uint64_t acc = a.tag ^ b.tag;
    acc ^= ((uint64_t)a.inner.lo << 16) | a.inner.hi;
    acc ^= ((uint64_t)b.inner.lo << 32) | b.inner.hi;
    printf("%llu\n", (unsigned long long)acc);
    return 0;
}
"#,
    )
    .expect("nested aggregate return C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_nested_aggregate_return_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_nested_aggregate_return.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_nested_aggregate_return");
    assert!(
        dump.contains(": ret "),
        "nested aggregate return should still lower through profile ret action:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_nested_aggregate_return");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir =
        std::fs::read_to_string(virtualized_ir).expect("virtualized nested aggregate return IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_nested_aggregate_return"));
    assert!(ir.contains("amice.vm.ret.field"));
}

#[test]
#[serial]
fn test_vm_virtualize_native_call_nested_aggregate_return_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_native_nested_aggregate_return.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_native_nested_aggregate_return'
source_filename = "vm_virtualize_native_nested_aggregate_return.ll"

%Inner = type { i16, i16 }
%Outer = type { i32, %Inner }

declare %Outer @native_make_outer(i32, i16, i16)

define i64 @vm_call_native_nested_aggregate(i32 %a, i16 %b, i16 %c) #0 {
entry:
  %outer = call %Outer @native_make_outer(i32 %a, i16 %b, i16 %c)
  %tag = extractvalue %Outer %outer, 0
  %lo = extractvalue %Outer %outer, 1, 0
  %hi = extractvalue %Outer %outer, 1, 1
  %tagx = zext i32 %tag to i64
  %lox = zext i16 %lo to i64
  %hix = zext i16 %hi to i64
  %mix0 = xor i64 %tagx, %lox
  %shift = shl i64 %hix, 32
  %mix1 = xor i64 %mix0, %shift
  ret i64 %mix1
}

attributes #0 = { noinline optnone }
"#,
    )
    .expect("native nested aggregate return LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_native_nested_aggregate_return_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

struct Inner {
    uint16_t lo;
    uint16_t hi;
};

struct Outer {
    uint32_t tag;
    struct Inner inner;
};

struct Outer native_make_outer(uint32_t a, uint16_t b, uint16_t c) {
    struct Outer out;
    out.tag = (a ^ 0x5a5a5a5au) + b;
    out.inner.lo = (uint16_t)(b + 0x1234u);
    out.inner.hi = (uint16_t)(c ^ 0xa55au);
    return out;
}

uint64_t vm_call_native_nested_aggregate(uint32_t a, uint16_t b, uint16_t c);

int main(void) {
    uint64_t acc = 0;
    acc ^= vm_call_native_nested_aggregate(0x12345678u, 0x1357u, 0x2468u);
    acc ^= vm_call_native_nested_aggregate(0x9abcdef0u, 0xabcdU, 0x4321U);
    printf("%llu\n", (unsigned long long)acc);
    return 0;
}
"#,
    )
    .expect("native nested aggregate return C harness should be writable");

    let baseline = compile_ir_with_c_harness(
        &ir_source,
        &harness,
        "vm_virtualize_native_nested_aggregate_return_baseline",
    );
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_native_nested_aggregate_return.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_call_native_nested_aggregate");
    let native_return_count = dump.matches("NativeReturn").count();
    assert!(
        dump.contains(": call_native ")
            && native_return_count == 3
            && dump.contains("width: 32")
            && dump.matches("width: 16").count() >= 2,
        "nested aggregate native return should use call_native with three flattened return slots:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(
        &virtualized_ir,
        &harness,
        "vm_virtualize_native_nested_aggregate_return",
    );
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir)
        .expect("virtualized native nested aggregate return IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_call_native_nested_aggregate"));
    assert!(ir.contains(".amice.vm.native_thunk.vm_call_native_nested_aggregate"));
    assert!(ir.contains("@native_make_outer"));
    assert!(ir.contains("amice.vm.native.ret.field"));
}

#[test]
#[serial]
fn test_vm_virtualize_native_call_aggregate_parameter_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_native_aggregate_param.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_native_aggregate_param'
source_filename = "vm_virtualize_native_aggregate_param.ll"

%Inner = type { i16, i16 }
%Arg = type { i8, %Inner, [2 x i32], ptr, float }

@.amice.vm.ann = private unnamed_addr constant [15 x i8] c"+vm_virtualize\00", section "llvm.metadata"
@.amice.vm.file = private unnamed_addr constant [17 x i8] c"vm-native-arg.ll\00", section "llvm.metadata"
@llvm.global.annotations = appending global [1 x { ptr, ptr, ptr, i32, ptr }] [
  { ptr, ptr, ptr, i32, ptr } { ptr @vm_call_native_aggregate_param, ptr @.amice.vm.ann, ptr @.amice.vm.file, i32 5, ptr null }
], section "llvm.metadata"

define i64 @native_mix_aggregate(%Arg %arg, i64 %salt) #0 {
entry:
  %tag = extractvalue %Arg %arg, 0
  %lo = extractvalue %Arg %arg, 1, 0
  %hi = extractvalue %Arg %arg, 1, 1
  %x = extractvalue %Arg %arg, 2, 0
  %y = extractvalue %Arg %arg, 2, 1
  %ptrv = extractvalue %Arg %arg, 3
  %fv = extractvalue %Arg %arg, 4
  %tag64 = zext i8 %tag to i64
  %lo64 = zext i16 %lo to i64
  %hi64 = zext i16 %hi to i64
  %x64 = zext i32 %x to i64
  %y64 = zext i32 %y to i64
  %fi = fptosi float %fv to i32
  %fi64 = sext i32 %fi to i64
  %nonnull = icmp ne ptr %ptrv, null
  %ptrflag = zext i1 %nonnull to i64
  %m0 = shl i64 %lo64, 7
  %m1 = xor i64 %tag64, %m0
  %m2 = shl i64 %hi64, 25
  %m3 = xor i64 %m1, %m2
  %m4 = shl i64 %x64, 32
  %m5 = xor i64 %m3, %m4
  %m6 = add i64 %m5, %y64
  %m7 = xor i64 %m6, %fi64
  %m8 = xor i64 %m7, %ptrflag
  %m9 = add i64 %m8, %salt
  ret i64 %m9
}

define i64 @vm_call_native_aggregate_param(i8 %tag, i16 %lo, i16 %hi, i32 %x, i32 %y, ptr %ptrv, float %fv, i64 %salt) #0 {
entry:
  %a0 = insertvalue %Arg undef, i8 %tag, 0
  %a1 = insertvalue %Arg %a0, i16 %lo, 1, 0
  %a2 = insertvalue %Arg %a1, i16 %hi, 1, 1
  %a3 = insertvalue %Arg %a2, i32 %x, 2, 0
  %a4 = insertvalue %Arg %a3, i32 %y, 2, 1
  %a5 = insertvalue %Arg %a4, ptr %ptrv, 3
  %a6 = insertvalue %Arg %a5, float %fv, 4
  %native = call i64 @native_mix_aggregate(%Arg %a6, i64 %salt)
  %again = extractvalue %Arg %a6, 2, 1
  %again64 = zext i32 %again to i64
  %ret = xor i64 %native, %again64
  ret i64 %ret
}

attributes #0 = { noinline optnone }
"#,
    )
    .expect("native aggregate parameter LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_native_aggregate_param_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_call_native_aggregate_param(
    uint8_t tag,
    uint16_t lo,
    uint16_t hi,
    uint32_t x,
    uint32_t y,
    void *ptrv,
    float fv,
    uint64_t salt);

int main(void) {
    int marker = 0;
    uint64_t acc = 0;
    acc ^= vm_call_native_aggregate_param(7, 0x1234u, 0x4567u, 0x89abcdefu, 0x10203040u, &marker, 19.75f, 12345ULL);
    acc ^= vm_call_native_aggregate_param(19, 0x0102u, 0x0304u, 0x55667788u, 0xaabbccddu, 0, -8.50f, 67890ULL);
    printf("%llu\n", (unsigned long long)acc);
    return 0;
}
"#,
    )
    .expect("native aggregate parameter C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_native_aggregate_param_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = ObfuscationConfig::disabled();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_native_aggregate_param.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_call_native_aggregate_param");
    assert!(
        dump.contains(": call_native "),
        "native aggregate parameter should lower through call_native:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_native_aggregate_param");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir =
        std::fs::read_to_string(virtualized_ir).expect("virtualized native aggregate parameter IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_call_native_aggregate_param"));
    assert!(ir.contains(".amice.vm.native_thunk.vm_call_native_aggregate_param"));
    assert!(ir.contains("amice.vm.native.arg.field"));
    assert!(ir.contains("handler.call_native"));
}

#[test]
#[serial]
fn test_vm_virtualize_indirect_call_aggregate_parameter_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_indirect_aggregate_param.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_indirect_aggregate_param'
source_filename = "vm_virtualize_indirect_aggregate_param.ll"

%Arg = type { i16, [2 x i32] }

@.amice.vm.ann = private unnamed_addr constant [15 x i8] c"+vm_virtualize\00", section "llvm.metadata"
@.amice.vm.file = private unnamed_addr constant [19 x i8] c"vm-indirect-arg.ll\00", section "llvm.metadata"
@llvm.global.annotations = appending global [1 x { ptr, ptr, ptr, i32, ptr }] [
  { ptr, ptr, ptr, i32, ptr } { ptr @vm_indirect_aggregate_param, ptr @.amice.vm.ann, ptr @.amice.vm.file, i32 5, ptr null }
], section "llvm.metadata"

define i64 @native_indirect_aggregate(%Arg %arg, i64 %salt) #0 {
entry:
  %tag = extractvalue %Arg %arg, 0
  %x = extractvalue %Arg %arg, 1, 0
  %y = extractvalue %Arg %arg, 1, 1
  %tag64 = zext i16 %tag to i64
  %x64 = zext i32 %x to i64
  %y64 = zext i32 %y to i64
  %xs = shl i64 %x64, 17
  %ys = shl i64 %y64, 33
  %m0 = xor i64 %tag64, %xs
  %m1 = xor i64 %m0, %ys
  %m2 = add i64 %m1, %salt
  ret i64 %m2
}

define i64 @vm_indirect_aggregate_param(ptr %callee, i16 %tag, i32 %x, i32 %y, i64 %salt) #0 {
entry:
  %a0 = insertvalue %Arg undef, i16 %tag, 0
  %a1 = insertvalue %Arg %a0, i32 %x, 1, 0
  %a2 = insertvalue %Arg %a1, i32 %y, 1, 1
  %native = call i64 %callee(%Arg %a2, i64 %salt)
  %again = extractvalue %Arg %a2, 1, 0
  %again64 = zext i32 %again to i64
  %ret = xor i64 %native, %again64
  ret i64 %ret
}

define i64 @run_indirect_aggregate_param(i16 %tag, i32 %x, i32 %y, i64 %salt) #0 {
entry:
  %r = call i64 @vm_indirect_aggregate_param(ptr @native_indirect_aggregate, i16 %tag, i32 %x, i32 %y, i64 %salt)
  ret i64 %r
}

attributes #0 = { noinline optnone }
"#,
    )
    .expect("indirect aggregate parameter LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_indirect_aggregate_param_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t run_indirect_aggregate_param(uint16_t tag, uint32_t x, uint32_t y, uint64_t salt);

int main(void) {
    uint64_t acc = 0;
    acc ^= run_indirect_aggregate_param(0x1234u, 0x89abcdefu, 0x10203040u, 12345ULL);
    acc ^= run_indirect_aggregate_param(0xabcdU, 0x55667788u, 0xaabbccddu, 67890ULL);
    printf("%llu\n", (unsigned long long)acc);
    return 0;
}
"#,
    )
    .expect("indirect aggregate parameter C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_indirect_aggregate_param_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = ObfuscationConfig::disabled();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_indirect_aggregate_param.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_indirect_aggregate_param");
    assert!(
        dump.contains(": call_native "),
        "indirect aggregate parameter should lower through call_native:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_indirect_aggregate_param");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir)
        .expect("virtualized indirect aggregate parameter IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_indirect_aggregate_param"));
    assert!(ir.contains(".amice.vm.indirect_adapter.vm_indirect_aggregate_param"));
    assert!(ir.contains("amice.vm.native.arg.field"));
    assert!(ir.contains("amice.vm.native.arg.element"));
    assert!(ir.contains("handler.call_native"));
}

#[test]
#[serial]
fn test_vm_virtualize_direct_array_aggregate_return_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_array_aggregate_return.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_array_aggregate_return'
source_filename = "vm_virtualize_array_aggregate_return.ll"

@.amice.vm.ann = private unnamed_addr constant [15 x i8] c"+vm_virtualize\00", section "llvm.metadata"
@.amice.vm.file = private unnamed_addr constant [12 x i8] c"vm-array.ll\00", section "llvm.metadata"
@llvm.global.annotations = appending global [1 x { ptr, ptr, ptr, i32, ptr }] [
  { ptr, ptr, ptr, i32, ptr } { ptr @vm_array_return, ptr @.amice.vm.ann, ptr @.amice.vm.file, i32 5, ptr null }
], section "llvm.metadata"

define [3 x i16] @vm_array_return(i16 %a, i16 %b, i16 %c) #0 {
entry:
  %x = add i16 %a, 7
  %y = xor i16 %b, 4660
  %z = sub i16 %c, %a
  %r0 = insertvalue [3 x i16] undef, i16 %x, 0
  %r1 = insertvalue [3 x i16] %r0, i16 %y, 1
  %r2 = insertvalue [3 x i16] %r1, i16 %z, 2
  ret [3 x i16] %r2
}

define i64 @vm_use_array_return(i16 %a, i16 %b, i16 %c) #0 {
entry:
  %arr = call [3 x i16] @vm_array_return(i16 %a, i16 %b, i16 %c)
  %x = extractvalue [3 x i16] %arr, 0
  %y = extractvalue [3 x i16] %arr, 1
  %z = extractvalue [3 x i16] %arr, 2
  %xx = zext i16 %x to i64
  %yy = zext i16 %y to i64
  %zz = zext i16 %z to i64
  %ys = shl i64 %yy, 21
  %zs = shl i64 %zz, 42
  %m0 = xor i64 %xx, %ys
  %m1 = xor i64 %m0, %zs
  ret i64 %m1
}

attributes #0 = { noinline optnone }
"#,
    )
    .expect("array aggregate return LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_array_aggregate_return_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_use_array_return(uint16_t a, uint16_t b, uint16_t c);

int main(void) {
    uint64_t acc = 0;
    acc ^= vm_use_array_return(0x1234u, 0x5678u, 0x9abcu);
    acc ^= vm_use_array_return(0xfedcu, 0x1357u, 0x2468u);
    printf("%llu\n", (unsigned long long)acc);
    return 0;
}
"#,
    )
    .expect("array aggregate return C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_array_aggregate_return_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = ObfuscationConfig::disabled();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_array_aggregate_return.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_array_return");
    assert!(
        dump.contains(": ret "),
        "direct fixed-array aggregate return should lower through profile ret action:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_array_aggregate_return");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized array aggregate return IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_array_return"));
    assert!(!ir.contains(".amice.vm.bytecode.vm_use_array_return"));
    assert!(ir.contains("amice.vm.ret.element"));
}

#[test]
#[serial]
fn test_vm_virtualize_native_call_array_aggregate_return_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_native_array_return.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_native_array_return'
source_filename = "vm_virtualize_native_array_return.ll"

@.amice.vm.ann = private unnamed_addr constant [15 x i8] c"+vm_virtualize\00", section "llvm.metadata"
@.amice.vm.file = private unnamed_addr constant [12 x i8] c"vm-array.ll\00", section "llvm.metadata"
@llvm.global.annotations = appending global [1 x { ptr, ptr, ptr, i32, ptr }] [
  { ptr, ptr, ptr, i32, ptr } { ptr @vm_call_native_array_return, ptr @.amice.vm.ann, ptr @.amice.vm.file, i32 5, ptr null }
], section "llvm.metadata"

define [2 x i32] @native_make_array(i32 %a, i32 %b) #0 {
entry:
  %x = mul i32 %a, 17
  %y0 = add i32 %b, %a
  %y = xor i32 %y0, 305419896
  %r0 = insertvalue [2 x i32] undef, i32 %x, 0
  %r1 = insertvalue [2 x i32] %r0, i32 %y, 1
  ret [2 x i32] %r1
}

define i64 @vm_call_native_array_return(i32 %a, i32 %b) #0 {
entry:
  %arr = call [2 x i32] @native_make_array(i32 %a, i32 %b)
  %x = extractvalue [2 x i32] %arr, 0
  %y = extractvalue [2 x i32] %arr, 1
  %xx = zext i32 %x to i64
  %yy = zext i32 %y to i64
  %ys = shl i64 %yy, 32
  %m = xor i64 %xx, %ys
  ret i64 %m
}

attributes #0 = { noinline optnone }
"#,
    )
    .expect("native array return LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_native_array_return_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_call_native_array_return(uint32_t a, uint32_t b);

int main(void) {
    uint64_t acc = 0;
    acc ^= vm_call_native_array_return(0x12345678u, 0x13572468u);
    acc ^= vm_call_native_array_return(0x9abcdef0u, 0x24681357u);
    printf("%llu\n", (unsigned long long)acc);
    return 0;
}
"#,
    )
    .expect("native array return C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_native_array_return_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = ObfuscationConfig::disabled();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_native_array_return.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_call_native_array_return");
    let native_return_count = dump.matches("NativeReturn").count();
    assert!(
        dump.contains(": call_native ") && native_return_count == 2 && dump.matches("width: 32").count() >= 2,
        "fixed-array native return should use call_native with two flattened return slots:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_native_array_return");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized native array return IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_call_native_array_return"));
    assert!(ir.contains(".amice.vm.native_thunk.vm_call_native_array_return"));
    assert!(ir.contains("amice.vm.native.ret.element"));
}

#[test]
#[serial]
fn test_vm_virtualize_indirect_call_array_aggregate_return_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_indirect_array_return.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_indirect_array_return'
source_filename = "vm_virtualize_indirect_array_return.ll"

@.amice.vm.ann = private unnamed_addr constant [15 x i8] c"+vm_virtualize\00", section "llvm.metadata"
@.amice.vm.file = private unnamed_addr constant [12 x i8] c"vm-array.ll\00", section "llvm.metadata"
@llvm.global.annotations = appending global [1 x { ptr, ptr, ptr, i32, ptr }] [
  { ptr, ptr, ptr, i32, ptr } { ptr @vm_indirect_array_return, ptr @.amice.vm.ann, ptr @.amice.vm.file, i32 5, ptr null }
], section "llvm.metadata"

define [2 x i16] @native_indirect_array(i16 %x) #0 {
entry:
  %a = add i16 %x, 33
  %b = xor i16 %x, 21930
  %r0 = insertvalue [2 x i16] undef, i16 %a, 0
  %r1 = insertvalue [2 x i16] %r0, i16 %b, 1
  ret [2 x i16] %r1
}

define i64 @vm_indirect_array_return(ptr %callee, i16 %x) #0 {
entry:
  %arr = call [2 x i16] %callee(i16 %x)
  %a = extractvalue [2 x i16] %arr, 0
  %b = extractvalue [2 x i16] %arr, 1
  %aa = zext i16 %a to i64
  %bb = zext i16 %b to i64
  %bs = shl i64 %bb, 32
  %m = xor i64 %aa, %bs
  ret i64 %m
}

define i64 @run_indirect_array_return(i16 %x) #0 {
entry:
  %r = call i64 @vm_indirect_array_return(ptr @native_indirect_array, i16 %x)
  ret i64 %r
}

attributes #0 = { noinline optnone }
"#,
    )
    .expect("indirect array return LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_indirect_array_return_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t run_indirect_array_return(uint16_t x);

int main(void) {
    uint64_t acc = 0;
    acc ^= run_indirect_array_return(0x1234u);
    acc ^= run_indirect_array_return(0x9abcu);
    printf("%llu\n", (unsigned long long)acc);
    return 0;
}
"#,
    )
    .expect("indirect array return C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_indirect_array_return_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = ObfuscationConfig::disabled();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_indirect_array_return.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_indirect_array_return");
    let native_return_count = dump.matches("NativeReturn").count();
    assert!(
        dump.contains(": call_native ") && native_return_count == 2 && dump.matches("width: 16").count() >= 2,
        "fixed-array indirect return should use call_native with two flattened return slots:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_indirect_array_return");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized indirect array return IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_indirect_array_return"));
    assert!(ir.contains(".amice.vm.indirect_adapter.vm_indirect_array_return"));
    assert!(ir.contains("amice.vm.native.ret.element"));
}

#[test]
#[serial]
fn test_vm_virtualize_dynamic_memcpy_memmove_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_dynamic_memcpy_memmove.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_dynamic_memcpy_memmove'
source_filename = "vm_virtualize_dynamic_memcpy_memmove.ll"

declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1 immarg)
declare void @llvm.memmove.p0.p0.i64(ptr, ptr, i64, i1 immarg)

define i32 @vm_dynamic_memcpy(i64 %n) {
entry:
  %bounded = and i64 %n, 7
  %len = add nuw nsw i64 %bounded, 1
  %src = alloca [16 x i8], align 16
  %dst = alloca [16 x i8], align 16
  %src_base = getelementptr inbounds [16 x i8], ptr %src, i64 0, i64 0
  %dst_base = getelementptr inbounds [16 x i8], ptr %dst, i64 0, i64 0
  %src_1 = getelementptr inbounds i8, ptr %src_base, i64 1
  %src_2 = getelementptr inbounds i8, ptr %src_base, i64 2
  %src_3 = getelementptr inbounds i8, ptr %src_base, i64 3
  %src_4 = getelementptr inbounds i8, ptr %src_base, i64 4
  %src_5 = getelementptr inbounds i8, ptr %src_base, i64 5
  %src_6 = getelementptr inbounds i8, ptr %src_base, i64 6
  %src_7 = getelementptr inbounds i8, ptr %src_base, i64 7
  store i8 17, ptr %src_base, align 1
  store i8 34, ptr %src_1, align 1
  store i8 51, ptr %src_2, align 1
  store i8 68, ptr %src_3, align 1
  store i8 85, ptr %src_4, align 1
  store i8 102, ptr %src_5, align 1
  store i8 119, ptr %src_6, align 1
  store i8 -120, ptr %src_7, align 1
  call void @llvm.memcpy.p0.p0.i64(ptr %dst_base, ptr %src_base, i64 %len, i1 false)
  %last_index = add nsw i64 %len, -1
  %last_ptr = getelementptr inbounds i8, ptr %dst_base, i64 %last_index
  %first = load i8, ptr %dst_base, align 1
  %last = load i8, ptr %last_ptr, align 1
  %first32 = zext i8 %first to i32
  %last32 = zext i8 %last to i32
  %len32 = trunc i64 %len to i32
  %a = shl i32 %first32, 16
  %b = shl i32 %last32, 8
  %c = xor i32 %a, %b
  %d = xor i32 %c, %len32
  ret i32 %d
}

define i32 @vm_dynamic_memmove(i64 %n) {
entry:
  %bounded = and i64 %n, 7
  %len = add nuw nsw i64 %bounded, 1
  %buf = alloca [16 x i8], align 16
  %base = getelementptr inbounds [16 x i8], ptr %buf, i64 0, i64 0
  %p1 = getelementptr inbounds i8, ptr %base, i64 1
  %p2 = getelementptr inbounds i8, ptr %base, i64 2
  %p3 = getelementptr inbounds i8, ptr %base, i64 3
  %p4 = getelementptr inbounds i8, ptr %base, i64 4
  %p5 = getelementptr inbounds i8, ptr %base, i64 5
  %p6 = getelementptr inbounds i8, ptr %base, i64 6
  %p7 = getelementptr inbounds i8, ptr %base, i64 7
  store i8 11, ptr %base, align 1
  store i8 22, ptr %p1, align 1
  store i8 33, ptr %p2, align 1
  store i8 44, ptr %p3, align 1
  store i8 55, ptr %p4, align 1
  store i8 66, ptr %p5, align 1
  store i8 77, ptr %p6, align 1
  store i8 88, ptr %p7, align 1
  %dst = getelementptr inbounds i8, ptr %base, i64 2
  call void @llvm.memmove.p0.p0.i64(ptr %dst, ptr %base, i64 %len, i1 false)
  %last_index = add nsw i64 %len, -1
  %last_ptr = getelementptr inbounds i8, ptr %dst, i64 %last_index
  %prefix = load i8, ptr %base, align 1
  %first = load i8, ptr %dst, align 1
  %last = load i8, ptr %last_ptr, align 1
  %prefix32 = zext i8 %prefix to i32
  %first32 = zext i8 %first to i32
  %last32 = zext i8 %last to i32
  %len32 = trunc i64 %len to i32
  %a = shl i32 %prefix32, 16
  %b = shl i32 %first32, 8
  %c = xor i32 %a, %b
  %d = xor i32 %c, %last32
  %e = xor i32 %d, %len32
  ret i32 %e
}
"#,
    )
    .expect("dynamic memcpy/memmove LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_dynamic_memcpy_memmove_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_dynamic_memcpy(uint64_t n);
int32_t vm_dynamic_memmove(uint64_t n);

int main(void) {
    int32_t acc = 0;
    acc = acc * 131 + vm_dynamic_memcpy(0);
    acc = acc * 131 + vm_dynamic_memcpy(6);
    acc = acc * 131 + vm_dynamic_memcpy(31);
    acc = acc * 131 + vm_dynamic_memmove(0);
    acc = acc * 131 + vm_dynamic_memmove(5);
    acc = acc * 131 + vm_dynamic_memmove(63);
    printf("%d\n", acc);
    return 0;
}
"#,
    )
    .expect("dynamic memcpy/memmove C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_dynamic_memcpy_memmove_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (output_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_dynamic_memcpy_memmove.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let memcpy_dump = bytecode_dump_for_function(&stderr, "vm_dynamic_memcpy");
    assert!(
        memcpy_dump.contains(": memcpy_dyn "),
        "dynamic memcpy should lower through profile memcpy_dyn:\n{memcpy_dump}"
    );
    let memmove_dump = bytecode_dump_for_function(&stderr, "vm_dynamic_memmove");
    assert!(
        memmove_dump.contains(": memmove_dyn "),
        "dynamic memmove should lower through profile memmove_dyn:\n{memmove_dump}"
    );

    let virtualized = compile_ir_with_c_harness(&output_ir, &harness, "vm_virtualize_dynamic_memcpy_memmove");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(output_ir).expect("dynamic memcpy/memmove output IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_dynamic_memcpy"));
    assert!(ir.contains(".amice.vm.bytecode.vm_dynamic_memmove"));
    assert!(ir.contains("handler.memcpy_dyn"));
    assert!(ir.contains("handler.memmove_dyn"));
}

#[test]
#[serial]
fn test_vm_virtualize_volatile_memory_intrinsics_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_volatile_memory_intrinsics.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_volatile_memory_intrinsics'
source_filename = "vm_virtualize_volatile_memory_intrinsics.ll"

declare void @llvm.memset.p0.i64(ptr, i8, i64, i1 immarg)
declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1 immarg)
declare void @llvm.memmove.p0.p0.i64(ptr, ptr, i64, i1 immarg)

define i32 @vm_volatile_memcpy(i64 %n) {
entry:
  %bounded = and i64 %n, 7
  %len = add nuw nsw i64 %bounded, 1
  %src = alloca [16 x i8], align 16
  %dst = alloca [16 x i8], align 16
  %src_base = getelementptr inbounds [16 x i8], ptr %src, i64 0, i64 0
  %dst_base = getelementptr inbounds [16 x i8], ptr %dst, i64 0, i64 0
  %src_1 = getelementptr inbounds i8, ptr %src_base, i64 1
  %src_2 = getelementptr inbounds i8, ptr %src_base, i64 2
  %src_3 = getelementptr inbounds i8, ptr %src_base, i64 3
  %src_4 = getelementptr inbounds i8, ptr %src_base, i64 4
  %src_5 = getelementptr inbounds i8, ptr %src_base, i64 5
  %src_6 = getelementptr inbounds i8, ptr %src_base, i64 6
  %src_7 = getelementptr inbounds i8, ptr %src_base, i64 7
  store i8 9, ptr %src_base, align 1
  store i8 18, ptr %src_1, align 1
  store i8 27, ptr %src_2, align 1
  store i8 36, ptr %src_3, align 1
  store i8 45, ptr %src_4, align 1
  store i8 54, ptr %src_5, align 1
  store i8 63, ptr %src_6, align 1
  store i8 72, ptr %src_7, align 1
  call void @llvm.memcpy.p0.p0.i64(ptr %dst_base, ptr %src_base, i64 %len, i1 true)
  %last_index = add nsw i64 %len, -1
  %last_ptr = getelementptr inbounds i8, ptr %dst_base, i64 %last_index
  %first = load i8, ptr %dst_base, align 1
  %last = load i8, ptr %last_ptr, align 1
  %first32 = zext i8 %first to i32
  %last32 = zext i8 %last to i32
  %len32 = trunc i64 %len to i32
  %a = shl i32 %first32, 12
  %b = shl i32 %last32, 4
  %c = xor i32 %a, %b
  %d = xor i32 %c, %len32
  ret i32 %d
}

define i32 @vm_volatile_memmove(i64 %n) {
entry:
  %bounded = and i64 %n, 7
  %len = add nuw nsw i64 %bounded, 1
  %buf = alloca [16 x i8], align 16
  %base = getelementptr inbounds [16 x i8], ptr %buf, i64 0, i64 0
  %p1 = getelementptr inbounds i8, ptr %base, i64 1
  %p2 = getelementptr inbounds i8, ptr %base, i64 2
  %p3 = getelementptr inbounds i8, ptr %base, i64 3
  %p4 = getelementptr inbounds i8, ptr %base, i64 4
  %p5 = getelementptr inbounds i8, ptr %base, i64 5
  %p6 = getelementptr inbounds i8, ptr %base, i64 6
  %p7 = getelementptr inbounds i8, ptr %base, i64 7
  store i8 7, ptr %base, align 1
  store i8 14, ptr %p1, align 1
  store i8 21, ptr %p2, align 1
  store i8 28, ptr %p3, align 1
  store i8 35, ptr %p4, align 1
  store i8 42, ptr %p5, align 1
  store i8 49, ptr %p6, align 1
  store i8 56, ptr %p7, align 1
  %dst = getelementptr inbounds i8, ptr %base, i64 2
  call void @llvm.memmove.p0.p0.i64(ptr %dst, ptr %base, i64 %len, i1 true)
  %last_index = add nsw i64 %len, -1
  %last_ptr = getelementptr inbounds i8, ptr %dst, i64 %last_index
  %prefix = load i8, ptr %base, align 1
  %first = load i8, ptr %dst, align 1
  %last = load i8, ptr %last_ptr, align 1
  %prefix32 = zext i8 %prefix to i32
  %first32 = zext i8 %first to i32
  %last32 = zext i8 %last to i32
  %a = shl i32 %prefix32, 16
  %b = shl i32 %first32, 8
  %c = xor i32 %a, %b
  %d = xor i32 %c, %last32
  ret i32 %d
}

define i32 @vm_volatile_memset(i8 %value) {
entry:
  %buf = alloca [16 x i8], align 16
  %base = getelementptr inbounds [16 x i8], ptr %buf, i64 0, i64 0
  call void @llvm.memset.p0.i64(ptr %base, i8 %value, i64 9, i1 true)
  %p8 = getelementptr inbounds i8, ptr %base, i64 8
  %first = load i8, ptr %base, align 1
  %last = load i8, ptr %p8, align 1
  %first32 = zext i8 %first to i32
  %last32 = zext i8 %last to i32
  %a = shl i32 %first32, 8
  %b = xor i32 %a, %last32
  ret i32 %b
}
"#,
    )
    .expect("volatile memory intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_volatile_memory_intrinsics_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_volatile_memcpy(uint64_t n);
int32_t vm_volatile_memmove(uint64_t n);
int32_t vm_volatile_memset(uint8_t value);

int main(void) {
    int32_t acc = 0;
    acc = acc * 131 + vm_volatile_memcpy(0);
    acc = acc * 131 + vm_volatile_memcpy(6);
    acc = acc * 131 + vm_volatile_memmove(0);
    acc = acc * 131 + vm_volatile_memmove(5);
    acc = acc * 131 + vm_volatile_memset(0x5a);
    printf("%d\n", acc);
    return 0;
}
"#,
    )
    .expect("volatile memory intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(
        &ir_source,
        &harness,
        "vm_virtualize_volatile_memory_intrinsics_baseline",
    );
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (output_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_volatile_memory_intrinsics.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let memcpy_dump = bytecode_dump_for_function(&stderr, "vm_volatile_memcpy");
    assert!(
        memcpy_dump.contains(": volatile_memcpy_dyn "),
        "volatile memcpy should lower through profile volatile_memcpy_dyn:\n{memcpy_dump}"
    );
    let memmove_dump = bytecode_dump_for_function(&stderr, "vm_volatile_memmove");
    assert!(
        memmove_dump.contains(": volatile_memmove_dyn "),
        "volatile memmove should lower through profile volatile_memmove_dyn:\n{memmove_dump}"
    );
    let memset_dump = bytecode_dump_for_function(&stderr, "vm_volatile_memset");
    assert!(
        memset_dump.contains(": volatile_memset_dyn "),
        "volatile memset should lower through profile volatile_memset_dyn:\n{memset_dump}"
    );

    let virtualized = compile_ir_with_c_harness(&output_ir, &harness, "vm_virtualize_volatile_memory_intrinsics");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(output_ir).expect("volatile memory intrinsic output IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_volatile_memcpy"));
    assert!(ir.contains(".amice.vm.bytecode.vm_volatile_memmove"));
    assert!(ir.contains(".amice.vm.bytecode.vm_volatile_memset"));
    assert!(ir.contains("handler.volatile_memcpy_dyn"));
    assert!(ir.contains("handler.volatile_memmove_dyn"));
    assert!(ir.contains("handler.volatile_memset_dyn"));
}

#[test]
#[serial]
fn test_vm_virtualize_atomic_rmw_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_atomic_rmw.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_atomic_rmw'
source_filename = "vm_virtualize_atomic_rmw.ll"

define i32 @vm_atomicrmw_mix(ptr %p, i32 %x) {
entry:
  %old_add = atomicrmw add ptr %p, i32 %x monotonic, align 4
  %old_xor = atomicrmw xor ptr %p, i32 85 acquire, align 4
  %old_or = atomicrmw or ptr %p, i32 7 release, align 4
  %old_and = atomicrmw and ptr %p, i32 255 acq_rel, align 4
  %old_sub = atomicrmw sub ptr %p, i32 3 seq_cst, align 4
  %old_xchg = atomicrmw xchg ptr %p, i32 %x monotonic, align 4
  %old_nand = atomicrmw nand ptr %p, i32 -2 acquire, align 4
  %old_max = atomicrmw max ptr %p, i32 64 release, align 4
  %old_min = atomicrmw min ptr %p, i32 12 acq_rel, align 4
  %old_umax = atomicrmw umax ptr %p, i32 200 seq_cst, align 4
  %old_umin = atomicrmw umin ptr %p, i32 18 monotonic, align 4
  %old_uinc = atomicrmw uinc_wrap ptr %p, i32 21 acquire, align 4
  %old_udec = atomicrmw udec_wrap ptr %p, i32 21 release, align 4
  %old_usub_cond = atomicrmw usub_cond ptr %p, i32 5 acq_rel, align 4
  %old_usub_sat = atomicrmw usub_sat ptr %p, i32 9 seq_cst, align 4
  %a = add i32 %old_add, %old_xor
  %b = xor i32 %a, %old_or
  %c = add i32 %b, %old_and
  %d = xor i32 %c, %old_sub
  %e = add i32 %d, %old_xchg
  %f = xor i32 %e, %old_nand
  %g = add i32 %f, %old_max
  %h = xor i32 %g, %old_min
  %i = add i32 %h, %old_umax
  %j = xor i32 %i, %old_umin
  %k = add i32 %j, %old_uinc
  %l = xor i32 %k, %old_udec
  %m = add i32 %l, %old_usub_cond
  %n = xor i32 %m, %old_usub_sat
  ret i32 %n
}

define i32 @vm_volatile_atomicrmw_mix(ptr %p, i32 %x) {
entry:
  %old_add = atomicrmw volatile add ptr %p, i32 %x monotonic, align 4
  %old_xor = atomicrmw volatile xor ptr %p, i32 165 acquire, align 4
  %old_xchg = atomicrmw volatile xchg ptr %p, i32 51 seq_cst, align 4
  %cur = load atomic volatile i32, ptr %p monotonic, align 4
  %a = add i32 %old_add, %old_xor
  %b = xor i32 %a, %old_xchg
  %c = add i32 %b, %cur
  ret i32 %c
}
"#,
    )
    .expect("atomicrmw LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_atomic_rmw_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_atomicrmw_mix(int32_t *p, int32_t x);
int32_t vm_volatile_atomicrmw_mix(int32_t *p, int32_t x);

int main(void) {
    int32_t value = 0x1234;
    int32_t result = vm_atomicrmw_mix(&value, 0x22);
    int32_t volatile_value = 0x2345;
    int32_t volatile_result = vm_volatile_atomicrmw_mix(&volatile_value, 0x44);
    printf("%d:%d:%d:%d\n", result, value, volatile_result, volatile_value);
    return 0;
}
"#,
    )
    .expect("atomicrmw C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_atomic_rmw_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized_ir = optimize_ir_with_plugin(&ir_source, "vm_virtualize_atomic_rmw.ll", vm_virtualize_config());
    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_atomic_rmw");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized atomicrmw LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_atomicrmw_mix"));
    assert!(ir.contains(".amice.vm.bytecode.vm_volatile_atomicrmw_mix"));
    assert!(ir.contains("handler.atomic_rmw_xchg"));
    assert!(ir.contains("handler.atomic_rmw_add"));
    assert!(ir.contains("handler.atomic_rmw_sub"));
    assert!(ir.contains("handler.atomic_rmw_and"));
    assert!(ir.contains("handler.atomic_rmw_or"));
    assert!(ir.contains("handler.atomic_rmw_xor"));
    assert!(ir.contains("handler.atomic_rmw_nand"));
    assert!(ir.contains("handler.atomic_rmw_max"));
    assert!(ir.contains("handler.atomic_rmw_min"));
    assert!(ir.contains("handler.atomic_rmw_umax"));
    assert!(ir.contains("handler.atomic_rmw_umin"));
    assert!(ir.contains("handler.atomic_rmw_uinc_wrap"));
    assert!(ir.contains("handler.atomic_rmw_udec_wrap"));
    assert!(ir.contains("handler.atomic_rmw_usub_cond"));
    assert!(ir.contains("handler.atomic_rmw_usub_sat"));
    assert!(ir.contains("handler.volatile_atomic_rmw_xchg"));
    assert!(ir.contains("handler.volatile_atomic_rmw_add"));
    assert!(ir.contains("handler.volatile_atomic_rmw_xor"));
}

#[test]
#[serial]
fn test_vm_virtualize_float_atomic_rmw_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_float_atomic_rmw.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_float_atomic_rmw'
source_filename = "vm_virtualize_float_atomic_rmw.ll"

define i32 @vm_float_atomicrmw_mix(ptr %fp, ptr %dp) {
entry:
  %old_fadd = atomicrmw fadd ptr %fp, float 1.250000e+00 monotonic, align 4
  %old_fsub = atomicrmw fsub ptr %fp, float 5.000000e-01 acquire, align 4
  %old_fmax = atomicrmw fmax ptr %fp, float 8.000000e+00 release, align 4
  %old_fmin = atomicrmw fmin ptr %fp, float 6.000000e+00 acq_rel, align 4
  %old_dadd = atomicrmw fadd ptr %dp, double 2.500000e+00 seq_cst, align 8
  %old_dsub = atomicrmw fsub ptr %dp, double 1.250000e+00 monotonic, align 8
  %old_dmax = atomicrmw fmaximum ptr %dp, double 1.600000e+01 acquire, align 8
  %old_dmin = atomicrmw fminimum ptr %dp, double 1.250000e+01 release, align 8
  %fa = fadd float %old_fadd, %old_fsub
  %fb = fadd float %fa, %old_fmax
  %fc = fadd float %fb, %old_fmin
  %fi = fptosi float %fc to i32
  %da = fadd double %old_dadd, %old_dsub
  %db = fadd double %da, %old_dmax
  %dc = fadd double %db, %old_dmin
  %di = fptosi double %dc to i32
  %final_f = load atomic float, ptr %fp monotonic, align 4
  %final_d = load atomic double, ptr %dp monotonic, align 8
  %final_f_scaled = fmul float %final_f, 1.000000e+02
  %final_d_scaled = fmul double %final_d, 1.000000e+02
  %ffi = fptosi float %final_f_scaled to i32
  %fdi = fptosi double %final_d_scaled to i32
  %sum0 = add i32 %fi, %di
  %sum1 = add i32 %sum0, %ffi
  %sum2 = add i32 %sum1, %fdi
  ret i32 %sum2
}
"#,
    )
    .expect("float atomicrmw LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_float_atomic_rmw_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_float_atomicrmw_mix(float *fp, double *dp);

int main(void) {
    float f = 4.0f;
    double d = 10.0;
    int32_t result = vm_float_atomicrmw_mix(&f, &d);
    printf("%d:%d:%d\n", result, (int)(f * 100.0f), (int)(d * 100.0));
    return 0;
}
"#,
    )
    .expect("float atomicrmw C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_float_atomic_rmw_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_float_atomic_rmw.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);

    let dump = bytecode_dump_for_function(&stderr, "vm_float_atomicrmw_mix");
    for expected in [
        ": atomic_rmw_fadd ",
        ": atomic_rmw_fsub ",
        ": atomic_rmw_fmax ",
        ": atomic_rmw_fmin ",
        ": atomic_rmw_fmaximum ",
        ": atomic_rmw_fminimum ",
    ] {
        assert!(
            dump.contains(expected),
            "float atomicrmw should lower through profile handler {expected}:\n{dump}"
        );
    }

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_float_atomic_rmw");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized float atomicrmw LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_float_atomicrmw_mix"));
    assert!(ir.contains("handler.atomic_rmw_fadd"));
    assert!(ir.contains("handler.atomic_rmw_fsub"));
    assert!(ir.contains("handler.atomic_rmw_fmax"));
    assert!(ir.contains("handler.atomic_rmw_fmin"));
    assert!(ir.contains("handler.atomic_rmw_fmaximum"));
    assert!(ir.contains("handler.atomic_rmw_fminimum"));
}

#[test]
#[serial]
fn test_vm_virtualize_cmpxchg_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_cmpxchg.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_cmpxchg'
source_filename = "vm_virtualize_cmpxchg.ll"

define i32 @vm_cmpxchg_mix(ptr %p, i32 %expected, i32 %desired) {
entry:
  %pair1 = cmpxchg ptr %p, i32 %expected, i32 %desired acquire monotonic, align 4
  %old1 = extractvalue { i32, i1 } %pair1, 0
  %ok1 = extractvalue { i32, i1 } %pair1, 1
  %wrong = add i32 %desired, 1
  %pair2 = cmpxchg ptr %p, i32 %wrong, i32 77 seq_cst acquire, align 4
  %old2 = extractvalue { i32, i1 } %pair2, 0
  %ok2 = extractvalue { i32, i1 } %pair2, 1
  %ok1_i32 = zext i1 %ok1 to i32
  %ok2_i32 = zext i1 %ok2 to i32
  %cur = load atomic i32, ptr %p monotonic, align 4
  %a = add i32 %old1, %old2
  %b = add i32 %a, %cur
  %c = add i32 %b, %ok1_i32
  %d = shl i32 %ok2_i32, 8
  %e = add i32 %c, %d
  ret i32 %e
}

define i32 @vm_weak_cmpxchg_mismatch(ptr %p, i32 %wrong_expected, i32 %desired) {
entry:
  %pair = cmpxchg weak ptr %p, i32 %wrong_expected, i32 %desired acquire monotonic, align 4
  %old = extractvalue { i32, i1 } %pair, 0
  %ok = extractvalue { i32, i1 } %pair, 1
  %ok_i32 = zext i1 %ok to i32
  %cur = load atomic i32, ptr %p monotonic, align 4
  %sum = add i32 %old, %cur
  %flag = shl i32 %ok_i32, 8
  %out = add i32 %sum, %flag
  ret i32 %out
}

define i32 @vm_volatile_cmpxchg_mix(ptr %p, i32 %expected, i32 %desired) {
entry:
  %pair = cmpxchg volatile ptr %p, i32 %expected, i32 %desired acquire monotonic, align 4
  %old = extractvalue { i32, i1 } %pair, 0
  %ok = extractvalue { i32, i1 } %pair, 1
  %ok_i32 = zext i1 %ok to i32
  %cur = load atomic volatile i32, ptr %p monotonic, align 4
  %sum = add i32 %old, %cur
  %flag = shl i32 %ok_i32, 9
  %out = add i32 %sum, %flag
  ret i32 %out
}
"#,
    )
    .expect("cmpxchg LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_cmpxchg_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_cmpxchg_mix(int32_t *p, int32_t expected, int32_t desired);
int32_t vm_weak_cmpxchg_mismatch(int32_t *p, int32_t wrong_expected, int32_t desired);
int32_t vm_volatile_cmpxchg_mix(int32_t *p, int32_t expected, int32_t desired);

int main(void) {
    int32_t value = 10;
    int32_t result = vm_cmpxchg_mix(&value, 10, 20);
    int32_t weak_value = 31;
    int32_t weak_result = vm_weak_cmpxchg_mismatch(&weak_value, 32, 44);
    int32_t volatile_value = 70;
    int32_t volatile_result = vm_volatile_cmpxchg_mix(&volatile_value, 70, 91);
    printf("%d:%d %d:%d %d:%d\n", result, value, weak_result, weak_value, volatile_result, volatile_value);
    return 0;
}
"#,
    )
    .expect("cmpxchg C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_cmpxchg_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized_ir = optimize_ir_with_plugin(&ir_source, "vm_virtualize_cmpxchg.ll", vm_virtualize_config());
    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_cmpxchg");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized cmpxchg LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_cmpxchg_mix"));
    assert!(ir.contains(".amice.vm.bytecode.vm_weak_cmpxchg_mismatch"));
    assert!(ir.contains(".amice.vm.bytecode.vm_volatile_cmpxchg_mix"));
    assert!(ir.contains("handler.cmpxchg"));
    assert!(ir.contains("handler.volatile_cmpxchg"));
}

#[test]
#[serial]
fn test_vm_virtualize_unsupported_atomic_syncscope_safely_skip() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_atomic_ops.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_atomic_ops'
source_filename = "vm_virtualize_atomic_ops.ll"

define void @vm_scoped_fence() {
entry:
  fence syncscope("singlethread") seq_cst
  ret void
}
"#,
    )
    .expect("atomic LLVM IR fixture should be writable");

    let (output_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_atomic_ops.ll", vm_virtualize_config());
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);

    let ir = std::fs::read_to_string(output_ir).expect("atomic output IR should be readable");
    assert!(!ir.contains(".amice.vm.bytecode.vm_scoped_fence"));

    assert!(stderr.contains("skip function"));
    assert!(stderr.contains("vm_scoped_fence"));
    assert!(stderr.contains("fence non-default atomic syncscope is not supported by vm_virtualize"));
}

#[test]
#[serial]
fn test_vm_virtualize_unsupported_integer_intrinsics_safely_skip() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_integer_intrinsic_skip.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_integer_intrinsic_skip'
source_filename = "vm_virtualize_integer_intrinsic_skip.ll"

declare i128 @llvm.ctpop.i128(i128)

define i64 @vm_ctpop_i128_skip(i128 %value) {
entry:
  %wide = call i128 @llvm.ctpop.i128(i128 %value)
  %low = trunc i128 %wide to i64
  ret i64 %low
}
"#,
    )
    .expect("unsupported integer intrinsic LLVM IR fixture should be writable");

    let (output_ir, output) = optimize_ir_with_plugin_debug(
        &ir_source,
        "vm_virtualize_integer_intrinsic_skip.ll",
        vm_virtualize_config(),
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);

    let ir = std::fs::read_to_string(output_ir).expect("integer intrinsic skip output IR should be readable");
    assert!(!ir.contains(".amice.vm.bytecode.vm_ctpop_i128_skip"));
    assert!(stderr.contains("skip function"));
    assert!(stderr.contains("vm_ctpop_i128_skip"));
    assert!(stderr.contains("unsupported integer width: 128"));
}

#[test]
#[serial]
fn test_vm_virtualize_poison_flag_integer_intrinsics_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_integer_poison_flags.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_integer_poison_flags'
source_filename = "vm_virtualize_integer_poison_flags.ll"

declare i32 @llvm.ctlz.i32(i32, i1 immarg)
declare i64 @llvm.cttz.i64(i64, i1 immarg)
declare i32 @llvm.abs.i32(i32, i1 immarg)

define i64 @vm_integer_poison_flags(i32 %a, i64 %b, i32 %c) {
entry:
  %a_nz = or i32 %a, 1
  %b_nz = or i64 %b, 8
  %c_safe = srem i32 %c, 1000000
  %lz = call i32 @llvm.ctlz.i32(i32 %a_nz, i1 true)
  %tz = call i64 @llvm.cttz.i64(i64 %b_nz, i1 true)
  %abs = call i32 @llvm.abs.i32(i32 %c_safe, i1 true)
  %lz64 = zext i32 %lz to i64
  %abs64 = sext i32 %abs to i64
  %mix0 = shl i64 %lz64, 32
  %mix1 = xor i64 %mix0, %tz
  %mix2 = add i64 %mix1, %abs64
  ret i64 %mix2
}
"#,
    )
    .expect("poison-flag integer intrinsic LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_integer_poison_flags_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>
int64_t vm_integer_poison_flags(int32_t a, int64_t b, int32_t c);
int main(void) {
    int64_t a = vm_integer_poison_flags(0x10, 0x4000, -123456);
    int64_t b = vm_integer_poison_flags(0x7fffffff, 0x100000000LL, 7654321);
    printf("%lld %lld\n", (long long)a, (long long)b);
    return 0;
}
"#,
    )
    .expect("poison-flag integer intrinsic C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_integer_poison_flags_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_integer_poison_flags.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_integer_poison_flags");
    assert!(
        dump.contains(": ctlz "),
        "ctlz true flag should lower to profile ctlz:\n{dump}"
    );
    assert!(
        dump.contains(": cttz "),
        "cttz true flag should lower to profile cttz:\n{dump}"
    );
    assert!(
        dump.contains(": iabs "),
        "abs true flag should lower to profile iabs:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_integer_poison_flags");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized poison-flag LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_integer_poison_flags"));
    assert!(ir.contains("handler.ctlz"));
    assert!(ir.contains("handler.cttz"));
    assert!(ir.contains("handler.iabs"));
}

#[test]
#[serial]
fn test_vm_virtualize_atomic_load_store_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_atomic_memory.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_atomic_memory'
source_filename = "vm_virtualize_atomic_memory.ll"

define i32 @vm_atomic_load(ptr %p) {
entry:
  %value = load atomic i32, ptr %p monotonic, align 4
  %mixed = add i32 %value, 5
  ret i32 %mixed
}

define void @vm_atomic_store(ptr %p, i32 %value) {
entry:
  %mixed = xor i32 %value, 90
  store atomic i32 %mixed, ptr %p release, align 4
  ret void
}

define i32 @vm_volatile_atomic_roundtrip(ptr %p, i32 %value) {
entry:
  %mixed = add i32 %value, 17
  store atomic volatile i32 %mixed, ptr %p release, align 4
  %loaded = load atomic volatile i32, ptr %p acquire, align 4
  %ret = xor i32 %loaded, 51
  ret i32 %ret
}
"#,
    )
    .expect("atomic memory LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_atomic_memory_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdio.h>

int vm_atomic_load(int *p);
void vm_atomic_store(int *p, int value);
int vm_volatile_atomic_roundtrip(int *p, int value);

int main(void) {
    int cell = 11;
    int a = vm_atomic_load(&cell);
    vm_atomic_store(&cell, 123);
    int volatile_cell = 19;
    int b = vm_volatile_atomic_roundtrip(&volatile_cell, 77);
    printf("%d %d %d %d\n", a, cell, b, volatile_cell);
    return 0;
}
"#,
    )
    .expect("atomic memory C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_atomic_memory_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized_ir = optimize_ir_with_plugin(&ir_source, "vm_virtualize_atomic_memory.ll", vm_virtualize_config());
    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_atomic_memory");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("atomic memory output IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_atomic_load"));
    assert!(ir.contains(".amice.vm.bytecode.vm_atomic_store"));
    assert!(ir.contains(".amice.vm.bytecode.vm_volatile_atomic_roundtrip"));
    assert!(ir.contains("handler.atomic_load"));
    assert!(ir.contains("handler.atomic_store"));
    assert!(ir.contains("handler.volatile_atomic_load"));
    assert!(ir.contains("handler.volatile_atomic_store"));
}

#[test]
#[serial]
fn test_vm_virtualize_atomic_float_load_store_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_atomic_float_memory.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_atomic_float_memory'
source_filename = "vm_virtualize_atomic_float_memory.ll"

define i64 @vm_atomic_float_memory(ptr %pf, ptr %pd, float %fv, double %dv) #0 {
entry:
  store atomic float %fv, ptr %pf release, align 4
  %loaded_f = load atomic float, ptr %pf acquire, align 4
  store atomic double %dv, ptr %pd seq_cst, align 8
  %loaded_d = load atomic double, ptr %pd seq_cst, align 8
  %mixed_f = fadd float %loaded_f, 1.250000e+00
  %mixed_d = fadd double %loaded_d, 2.500000e+00
  %fi = fptosi float %mixed_f to i32
  %di = fptosi double %mixed_d to i64
  %fx = sext i32 %fi to i64
  %ret = xor i64 %fx, %di
  ret i64 %ret
}

attributes #0 = { noinline optnone }
"#,
    )
    .expect("atomic float LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_atomic_float_memory_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_atomic_float_memory(float *pf, double *pd, float fv, double dv);

int main(void) {
    float f = 0.0f;
    double d = 0.0;
    uint64_t acc = 0;
    acc ^= vm_atomic_float_memory(&f, &d, 19.75f, 1234.5);
    acc ^= vm_atomic_float_memory(&f, &d, -8.50f, -77.25);
    printf("%llu %.2f %.2f\n", (unsigned long long)acc, f, d);
    return 0;
}
"#,
    )
    .expect("atomic float C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_atomic_float_memory_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_atomic_float_memory.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_atomic_float_memory");
    assert!(
        dump.contains(": atomic_store ")
            && dump.contains(": atomic_load ")
            && dump.contains("width: 32")
            && dump.contains("width: 64"),
        "float/double atomic load/store should lower through profile atomic handlers:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_atomic_float_memory");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized atomic float IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_atomic_float_memory"));
    assert!(ir.contains("handler.atomic_load"));
    assert!(ir.contains("handler.atomic_store"));
}

#[test]
#[serial]
fn test_vm_virtualize_fence_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_fence.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_fence'
source_filename = "vm_virtualize_fence.ll"

define i32 @vm_fence_mix(ptr %p, i32 %x) {
entry:
  store atomic i32 %x, ptr %p release, align 4
  fence seq_cst
  %value = load atomic i32, ptr %p acquire, align 4
  %mixed = add i32 %value, 7
  ret i32 %mixed
}
"#,
    )
    .expect("fence LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_fence_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_fence_mix(int32_t *p, int32_t x);

int main(void) {
    int32_t value = 5;
    int32_t result = vm_fence_mix(&value, 41);
    printf("%d:%d\n", result, value);
    return 0;
}
"#,
    )
    .expect("fence C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_fence_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized_ir = optimize_ir_with_plugin(&ir_source, "vm_virtualize_fence.ll", vm_virtualize_config());
    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_fence");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized fence LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_fence_mix"));
    assert!(ir.contains("handler.fence"));
}

#[test]
#[serial]
fn test_vm_virtualize_volatile_scalar_memory_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_volatile_memory.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_volatile_memory'
source_filename = "vm_virtualize_volatile_memory.ll"

define i32 @vm_volatile_memory(ptr %p, i32 %value) {
entry:
  %old = load volatile i32, ptr %p, align 4
  %mix = xor i32 %old, %value
  store volatile i32 %mix, ptr %p, align 4
  %new = load volatile i32, ptr %p, align 4
  %sum = add i32 %new, %old
  ret i32 %sum
}
"#,
    )
    .expect("volatile memory LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_volatile_memory_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_volatile_memory(int32_t *p, int32_t value);

int main(void) {
    int32_t cell = 17;
    int32_t result = vm_volatile_memory(&cell, 0x5a5a1234);
    printf("%d:%d\n", result, cell);
    return 0;
}
"#,
    )
    .expect("volatile memory C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_volatile_memory_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (output_ir, output) = optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_volatile_memory.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_volatile_memory");
    assert!(
        dump.contains(": volatile_load "),
        "volatile scalar load should lower through the profile volatile_load instruction:\n{dump}"
    );
    assert!(
        dump.contains(": volatile_store "),
        "volatile scalar store should lower through the profile volatile_store instruction:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&output_ir, &harness, "vm_virtualize_volatile_memory");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(output_ir).expect("volatile memory output IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_volatile_memory"));
    assert!(ir.contains("handler.volatile_load"));
    assert!(ir.contains("handler.volatile_store"));
}

#[test]
#[serial]
fn test_vm_virtualize_indirect_call_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_indirect_call.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_indirect_call'
source_filename = "vm_virtualize_indirect_call.ll"

define i32 @vm_indirect_call(ptr %callee, i32 %x) {
entry:
  %value = call i32 %callee(i32 %x)
  %mixed = xor i32 %value, %x
  ret i32 %mixed
}
"#,
    )
    .expect("indirect call LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_indirect_call_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_indirect_call(int32_t (*callee)(int32_t), int32_t x);

static int32_t add_seven(int32_t x) {
    return x + 7;
}

static int32_t mix_bits(int32_t x) {
    return (x * 33) ^ 0x13572468;
}

int main(void) {
    int32_t acc = 0;
    acc ^= vm_indirect_call(add_seven, 5);
    acc ^= vm_indirect_call(add_seven, -19);
    acc ^= vm_indirect_call(mix_bits, 12345);
    printf("%d\n", acc);
    return 0;
}
"#,
    )
    .expect("indirect call C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_indirect_call_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (output_ir, output) = optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_indirect_call.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_indirect_call");
    assert!(
        dump.contains(": call_native "),
        "indirect call should lower through profile call_native bridge:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&output_ir, &harness, "vm_virtualize_indirect_call");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(output_ir).expect("indirect call output IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_indirect_call"));
    assert!(ir.contains("handler.call_native"));
    assert!(ir.contains(".amice.vm.indirect_adapter.vm_indirect_call"));
}

#[test]
#[serial]
fn test_vm_virtualize_exception_and_indirect_control_flow_safely_skip() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_exception_control.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_exception_control'
source_filename = "vm_virtualize_exception_control.ll"

declare i32 @may_throw(i32)
declare i32 @__gxx_personality_v0(...)
@vm_indirectbr_targets = private global [2 x ptr] [
  ptr blockaddress(@vm_indirectbr, %fallthrough),
  ptr blockaddress(@vm_indirectbr, %other)
]
@llvm.used = appending global [1 x ptr] [ptr @vm_indirectbr_targets], section "llvm.metadata"

define i32 @vm_invoke(i32 %x) personality ptr @__gxx_personality_v0 {
entry:
  %value = invoke i32 @may_throw(i32 %x)
          to label %ok unwind label %lpad

ok:
  ret i32 %value

lpad:
  %lp = landingpad { ptr, i32 }
          cleanup
  ret i32 -1
}

define void @vm_resume() personality ptr @__gxx_personality_v0 {
entry:
  resume { ptr, i32 } undef
}

define void @vm_callbr() #0 {
entry:
  callbr void asm sideeffect "", "!i"()
          to label %fallthrough [label %target]

fallthrough:
  ret void

target:
  ret void
}

define void @vm_indirectbr(ptr %target) #0 {
entry:
  indirectbr ptr %target, [label %fallthrough, label %other]

fallthrough:
  ret void

other:
  ret void
}

define i32 @vm_va_arg(ptr %ap) {
entry:
  %value = va_arg ptr %ap, i32
  ret i32 %value
}

attributes #0 = { noinline optnone }
"#,
    )
    .expect("exception/control-flow LLVM IR fixture should be writable");

    let (output_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_exception_control.ll", vm_virtualize_config());
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);

    let ir = std::fs::read_to_string(output_ir).expect("exception/control-flow output IR should be readable");
    assert!(!ir.contains(".amice.vm.bytecode.vm_invoke"));
    assert!(!ir.contains(".amice.vm.bytecode.vm_resume"));
    assert!(!ir.contains(".amice.vm.bytecode.vm_callbr"));
    assert!(!ir.contains(".amice.vm.bytecode.vm_indirectbr"));
    assert!(!ir.contains(".amice.vm.bytecode.vm_va_arg"));
    assert!(stderr.contains("skip function"));
    assert!(stderr.contains("vm_invoke"));
    assert!(stderr.contains("invoke exception edges are not supported by vm_virtualize"));
    assert!(stderr.contains("vm_resume"));
    assert!(stderr.contains("resume is not supported by vm_virtualize"));
    assert!(stderr.contains("vm_callbr"));
    assert!(stderr.contains("callbr is not supported by vm_virtualize"));
    assert!(stderr.contains("vm_indirectbr"));
    assert!(stderr.contains("indirectbr is not supported by vm_virtualize"));
    assert!(stderr.contains("vm_va_arg"));
    assert!(stderr.contains("va_arg is not supported by vm_virtualize"));
}

#[test]
#[serial]
fn test_vm_virtualize_unreachable_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_unreachable.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_unreachable'
source_filename = "vm_virtualize_unreachable.ll"

define i32 @vm_unreachable_guard(i32 %x) {
entry:
  %ok = icmp sge i32 %x, 0
  br i1 %ok, label %body, label %bad

body:
  %mul = mul i32 %x, 7
  %mix = xor i32 %mul, 12345
  ret i32 %mix

bad:
  unreachable
}
"#,
    )
    .expect("unreachable LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_unreachable_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>
int32_t vm_unreachable_guard(int32_t x);
int main(void) {
    printf("%d %d %d\n",
           vm_unreachable_guard(0),
           vm_unreachable_guard(3),
           vm_unreachable_guard(99));
    return 0;
}
"#,
    )
    .expect("unreachable C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_unreachable_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug_pipeline(&ir_source, "vm_virtualize_unreachable.ll", "default<O0>", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_unreachable_guard");
    assert!(
        dump.contains(": unreachable "),
        "unreachable terminator should lower through the profile unreachable handler:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_unreachable");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized unreachable LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_unreachable_guard"));
    assert!(ir.contains("handler.unreachable"));
    assert!(!ir.contains(".amice.vm.original.vm_unreachable_guard"));
}

#[test]
#[serial]
fn test_vm_virtualize_trap_intrinsic_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_trap.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_trap'
source_filename = "vm_virtualize_trap.ll"

declare void @llvm.trap()

define i32 @vm_trap_guard(i32 %x) {
entry:
  %ok = icmp sge i32 %x, 0
  br i1 %ok, label %body, label %bad

body:
  %mul = mul i32 %x, 11
  %mix = xor i32 %mul, 21930
  ret i32 %mix

bad:
  call void @llvm.trap()
  unreachable
}
"#,
    )
    .expect("trap LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_trap_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_trap_guard(int32_t x);

int main(int argc, char **argv) {
    (void)argv;
    if (argc > 1) {
        return vm_trap_guard(-1);
    }
    printf("%d %d %d\n", vm_trap_guard(0), vm_trap_guard(7), vm_trap_guard(99));
    return 0;
}
"#,
    )
    .expect("trap C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_trap_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) =
        optimize_ir_with_plugin_debug_pipeline(&ir_source, "vm_virtualize_trap.ll", "default<O0>", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_trap_guard");
    assert!(
        dump.contains(": trap "),
        "llvm.trap should lower through the profile trap handler:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_trap");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let baseline_trap = Command::new(&baseline.binary_path)
        .arg("trap")
        .output()
        .expect("baseline trap binary should run");
    let virtualized_trap = Command::new(&virtualized.binary_path)
        .arg("trap")
        .output()
        .expect("virtualized trap binary should run");
    assert!(
        !baseline_trap.status.success(),
        "baseline trap path should terminate abnormally"
    );
    assert!(
        !virtualized_trap.status.success(),
        "virtualized trap path should terminate abnormally"
    );

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized trap LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_trap_guard"));
    assert!(ir.contains("handler.trap"));
    assert!(ir.contains("@llvm.trap"));
    assert!(!ir.contains(".amice.vm.original.vm_trap_guard"));
}

#[test]
#[serial]
fn test_vm_virtualize_debugtrap_and_ubsantrap_intrinsics_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_debugtrap_ubsantrap.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_debugtrap_ubsantrap'
source_filename = "vm_virtualize_debugtrap_ubsantrap.ll"

declare void @llvm.debugtrap()
declare void @llvm.ubsantrap(i8 immarg)

define i32 @vm_debugtrap_guard(i32 %x) {
entry:
  %bad_input = icmp eq i32 %x, -7
  br i1 %bad_input, label %bad, label %body

body:
  %mul = mul i32 %x, 13
  %mix = xor i32 %mul, 324508639
  ret i32 %mix

bad:
  call void @llvm.debugtrap()
  unreachable
}

define i32 @vm_ubsantrap_guard(i32 %x) {
entry:
  %ok = icmp sge i32 %x, 0
  br i1 %ok, label %body, label %bad

body:
  %add = add i32 %x, 41
  %mix = xor i32 %add, -889275714
  ret i32 %mix

bad:
  call void @llvm.ubsantrap(i8 31)
  unreachable
}
"#,
    )
    .expect("debugtrap/ubsantrap LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_debugtrap_ubsantrap_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_debugtrap_guard(int32_t x);
int32_t vm_ubsantrap_guard(int32_t x);

int main(int argc, char **argv) {
    if (argc > 1 && argv[1][0] == 'd') {
        return vm_debugtrap_guard(-7);
    }
    if (argc > 1 && argv[1][0] == 'u') {
        return vm_ubsantrap_guard(-1);
    }
    printf("%d %d\n", vm_debugtrap_guard(5), vm_ubsantrap_guard(8));
    return 0;
}
"#,
    )
    .expect("debugtrap/ubsantrap C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_debugtrap_ubsantrap_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) = optimize_ir_with_plugin_debug_pipeline(
        &ir_source,
        "vm_virtualize_debugtrap_ubsantrap.ll",
        "default<O0>",
        config,
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    for function in ["vm_debugtrap_guard", "vm_ubsantrap_guard"] {
        let dump = bytecode_dump_for_function(&stderr, function);
        assert!(
            dump.contains(": trap "),
            "{function} should lower through the profile trap handler:\n{dump}"
        );
    }

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_debugtrap_ubsantrap");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    for arg in ["d", "u"] {
        let baseline_trap = Command::new(&baseline.binary_path)
            .arg(arg)
            .output()
            .expect("baseline debugtrap/ubsantrap binary should run");
        let virtualized_trap = Command::new(&virtualized.binary_path)
            .arg(arg)
            .output()
            .expect("virtualized debugtrap/ubsantrap binary should run");
        assert!(
            !baseline_trap.status.success(),
            "baseline {arg} path should terminate abnormally"
        );
        assert!(
            !virtualized_trap.status.success(),
            "virtualized {arg} path should terminate abnormally"
        );
    }

    let ir =
        std::fs::read_to_string(virtualized_ir).expect("virtualized debugtrap/ubsantrap LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_debugtrap_guard"));
    assert!(ir.contains(".amice.vm.bytecode.vm_ubsantrap_guard"));
    assert!(ir.contains("handler.trap"));
    assert!(!ir.contains("call void @llvm.debugtrap"));
    assert!(!ir.contains("call void @llvm.ubsantrap"));
}

#[test]
#[serial]
fn test_vm_virtualize_function_annotation_enables_pass_without_env() {
    ensure_plugin_built();

    let source = fixture_path("vm_virtualize", "basic.c", Language::C);
    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_annotation_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let annotation_only_config = ObfuscationConfig::disabled();
    let virtualized = compile_virtualized_binary_with_config(
        &source,
        "vm_virtualize_annotation_only",
        annotation_only_config.clone(),
    );
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir_path =
        compile_virtualized_ir_with_config(&source, "vm_virtualize_annotation_only.ll", annotation_only_config);
    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_mix"));
    assert!(ir.contains(".amice.vm.bytecode.vm_loop"));
    assert!(!ir.contains(".amice.vm.bytecode.main"));
}

#[test]
#[serial]
fn test_vm_virtualize_function_annotation_profile_path_override() {
    ensure_plugin_built();

    let profile_dir = custom_abi_profile_path();
    let source = output_dir().join("vm_virtualize_annotation_profile_path.c");
    std::fs::write(
        &source,
        format!(
            r#"#include <stdio.h>
__attribute__((noinline, annotate("+vm_virtualize,vm_profile_path={}")))
int vm_annotation_profile(int a, int b) {{
    return ((a + b) * 5) - a;
}}
int main(void) {{
    printf("%d\n", vm_annotation_profile(9, 4));
    return 0;
}}
"#,
            profile_dir.display()
        ),
    )
    .expect("annotation profile fixture should be writable");

    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_annotation_profile_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let config = ObfuscationConfig::disabled();
    let virtualized =
        compile_virtualized_binary_with_config(&source, "vm_virtualize_annotation_profile", config.clone());
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir_path = compile_virtualized_ir_with_config(&source, "vm_virtualize_annotation_profile.ll", config);
    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_annotation_profile"));
}

#[test]
#[serial]
fn test_vm_virtualize_profile_path_drives_abi_mapping() {
    ensure_plugin_built();

    let source = fixture_path("vm_virtualize", "basic.c", Language::C);
    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_profile_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_profile_path = Some(custom_abi_profile_path().to_string_lossy().into_owned());
    let virtualized = compile_virtualized_binary_with_config(&source, "vm_virtualize_profile_abi", config);
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();

    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());
}

#[test]
#[serial]
fn test_vm_virtualize_ruoke_profile_path_runs() {
    ensure_plugin_built();

    let source = output_dir().join("vm_virtualize_ruoke.c");
    std::fs::write(
        &source,
        r#"#include <stdio.h>

#define VMP __attribute__((noinline, annotate("+vm_virtualize")))

VMP int vm_ruoke_tiny(int a, int b) {
    int x = (a + b) * 3;
    if (x > 20) {
        x -= 7;
    } else {
        x += 5;
    }
    return x ^ (a & 7);
}

int main(void) {
    printf("%d\n", vm_ruoke_tiny(5, 6) + vm_ruoke_tiny(2, 3));
    return 0;
}
"#,
    )
    .expect("ruoke fixture should be writable");
    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_ruoke_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_profile_path = Some(ruoke_profile_path().to_string_lossy().into_owned());
    let virtualized = compile_virtualized_binary_with_config(&source, "vm_virtualize_ruoke", config.clone());
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir_path = compile_virtualized_ir_with_config(&source, "vm_virtualize_ruoke.ll", config);
    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(ir.contains(".amice.vm.dispatch.vm_ruoke_tiny"));
    assert!(ir.contains(".amice.vm.read_varint.vm_ruoke_tiny"));
    assert!(ir.contains("op3e8"));
    assert_eq!(handler_opcode_count(&ir), 1000);
}

#[test]
#[serial]
fn test_vm_virtualize_profile_semantic_ast_drives_renamed_opcode() {
    ensure_plugin_built();

    let source = output_dir().join("vm_virtualize_semantic_renamed_add.c");
    std::fs::write(
        &source,
        r#"#include <stdio.h>

#define VMP __attribute__((noinline, annotate("+vm_virtualize")))

VMP int vm_add_alias_only(int a, int b) {
    int c = a + b;
    return (c * 3) - a;
}

int main(void) {
    printf("%d\n", vm_add_alias_only(11, 7) + vm_add_alias_only(3, 5));
    return 0;
}
"#,
    )
    .expect("renamed add fixture should be writable");
    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_semantic_rename_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_profile_path = Some(semantic_renamed_add_profile_path().to_string_lossy().into_owned());
    let virtualized =
        compile_virtualized_binary_with_config(&source, "vm_virtualize_semantic_renamed_add", config.clone());
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir_path = compile_virtualized_ir_with_config(&source, "vm_virtualize_semantic_renamed_add.ll", config);
    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_add_alias_only"));
    assert!(ir.contains("handler.add_alias"));
    assert!(!ir.contains("handler.iadd.op"));
}

#[test]
#[serial]
fn test_vm_virtualize_lowering_emit_names_second_same_semantic_instruction() {
    ensure_plugin_built();

    let source = output_dir().join("vm_virtualize_iadd_alt.c");
    std::fs::write(
        &source,
        r#"__attribute__((noinline))
int vm_iadd_alt(int a, int b) {
    return a + b;
}
"#,
    )
    .expect("same-semantic fixture should be writable");

    let mut config = vm_virtualize_config();
    config.vm_profile_path = Some(same_semantic_alt_add_profile_path().to_string_lossy().into_owned());
    config.vm_dump_bytecode = Some(true);

    let (ir_path, output) = compile_virtualized_ir_with_debug_log(&source, "vm_virtualize_iadd_alt.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);

    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_iadd_alt"));
    assert!(stderr.contains("iadd_alt"));
    assert!(!stderr.contains(": iadd "));
}

#[test]
#[serial]
fn test_vm_virtualize_profile_operand_order_drives_bytecode_layout() {
    ensure_plugin_built();

    let source = fixture_path("vm_virtualize", "basic.c", Language::C);
    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_operand_order_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_profile_path = Some(reordered_add_operands_profile_path().to_string_lossy().into_owned());
    let virtualized = compile_virtualized_binary_with_config(&source, "vm_virtualize_operand_order", config.clone());
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir_path = compile_virtualized_ir_with_config(&source, "vm_virtualize_operand_order.ll", config);
    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_mix"));
    assert!(ir.contains("handler.iadd"));
}

#[test]
#[serial]
fn test_vm_virtualize_decoder_profile_drives_runtime_pipeline() {
    ensure_plugin_built();

    let source = fixture_path("vm_virtualize", "basic.c", Language::C);
    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_decoder_profile_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_profile_path = Some(decoder_variant_profile_path().to_string_lossy().into_owned());
    let virtualized = compile_virtualized_binary_with_config(&source, "vm_virtualize_decoder_profile", config.clone());
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir_path = compile_virtualized_ir_with_config(&source, "vm_virtualize_decoder_profile.ll", config);
    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(ir.contains(".amice.vm.read_varint"));
    assert!(ir.contains(".amice.vm.bytecode.vm_mix"));
    assert!(ir.contains("AMICE_VMP_RUNTIME_BYTECODE"));
}

#[test]
#[serial]
fn test_vm_virtualize_lowering_match_drives_rule_selection() {
    ensure_plugin_built();

    let source = fixture_path("vm_virtualize", "basic.c", Language::C);
    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_match_rule_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_profile_path = Some(renamed_add_lowering_rule_profile_path().to_string_lossy().into_owned());
    let virtualized = compile_virtualized_binary_with_config(&source, "vm_virtualize_match_rule", config.clone());
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir_path = compile_virtualized_ir_with_config(&source, "vm_virtualize_match_rule.ll", config);
    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_mix"));
    assert!(ir.contains("handler.iadd"));
}

#[test]
#[serial]
fn test_vm_virtualize_missing_lowering_rule_safely_skips_function() {
    ensure_plugin_built();

    let source = output_dir().join("vm_virtualize_missing_add_lowering.c");
    std::fs::write(
        &source,
        r#"__attribute__((noinline))
int vm_missing_add_lowering(int a, int b) {
    return a + b;
}
"#,
    )
    .expect("missing lowering fixture should be writable");

    let mut config = vm_virtualize_config();
    config.vm_profile_path = Some(missing_add_lowering_profile_path().to_string_lossy().into_owned());
    let (ir_path, output) =
        compile_virtualized_ir_with_debug_log(&source, "vm_virtualize_missing_add_lowering.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);

    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(!ir.contains(".amice.vm.bytecode.vm_missing_add_lowering"));
    assert!(stderr.contains("skip function"));
    assert!(stderr.contains("vm_missing_add_lowering"));
    assert!(stderr.contains("llvm.add.integer"));
}

#[test]
#[serial]
fn test_vm_virtualize_fixed_env_names_without_actions_are_rejected() {
    ensure_plugin_built();

    let source = output_dir().join("vm_virtualize_fixed_env_without_actions.c");
    std::fs::write(
        &source,
        r#"__attribute__((noinline))
int vm_fixed_env_without_actions(int a, int b) {
    return a + b;
}
"#,
    )
    .expect("fixed-env fixture should be writable");

    let mut config = vm_virtualize_config();
    config.vm_profile_path = Some(fixed_env_without_actions_profile_path().to_string_lossy().into_owned());
    let (ir_path, output) =
        compile_virtualized_ir_with_debug_log(&source, "vm_virtualize_fixed_env_without_actions.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);

    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(!ir.contains(".amice.vm.bytecode.vm_fixed_env_without_actions"));
    assert!(stderr.contains("skip function"));
    assert!(stderr.contains("vm_fixed_env_without_actions"));
    assert!(stderr.contains("undefined VM value"));
}

#[test]
#[serial]
fn test_vm_virtualize_lowering_emit_action_drives_instruction_choice() {
    ensure_plugin_built();

    let profile_dir = add_lowering_as_sub_profile_path();
    let source = output_dir().join("vm_virtualize_add_emit_action.c");
    std::fs::write(
        &source,
        format!(
            r#"#include <stdio.h>
__attribute__((noinline, annotate("+vm_virtualize,vm_profile_path={}")))
int vm_add_emit_action(int a, int b) {{
    return a + b;
}}
int main(void) {{
    printf("%d\n", vm_add_emit_action(9, 4));
    return 0;
}}
"#,
            profile_dir.display()
        ),
    )
    .expect("add emit-action fixture should be writable");

    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_add_emit_action_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();
    assert_eq!("13\n", baseline_output.stdout());

    let virtualized =
        compile_virtualized_binary_with_config(&source, "vm_virtualize_add_emit_action", ObfuscationConfig::disabled());
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_ne!("13\n", virtualized_output.stdout());
    assert!(
        matches!(virtualized_output.stdout().as_str(), "5\n" | "-5\n"),
        "rewriting add lowering to isub should produce the operand difference, got {}",
        virtualized_output.stdout()
    );
}

#[test]
#[serial]
fn test_vm_virtualize_materialize_action_drives_operand_binding() {
    ensure_plugin_built();

    let profile_dir = sub_materialize_swapped_profile_path();
    let source = output_dir().join("vm_virtualize_materialize_source.c");
    std::fs::write(
        &source,
        format!(
            r#"#include <stdio.h>
__attribute__((noinline, annotate("+vm_virtualize,vm_profile_path={}")))
int vm_materialize_source(int a, int b) {{
    return a - b;
}}
int main(void) {{
    printf("%d\n", vm_materialize_source(9, 4));
    return 0;
}}
"#,
            profile_dir.display()
        ),
    )
    .expect("materialize-source fixture should be writable");

    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_materialize_source_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();
    assert_eq!("5\n", baseline_output.stdout());

    let virtualized = compile_virtualized_binary_with_config(
        &source,
        "vm_virtualize_materialize_source",
        ObfuscationConfig::disabled(),
    );
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!("-5\n", virtualized_output.stdout());
}

#[test]
#[serial]
fn test_vm_virtualize_bind_action_drives_subsequent_ssa_use() {
    ensure_plugin_built();

    let profile_dir = add_bind_to_lhs_profile_path();
    let source = output_dir().join("vm_virtualize_bind_action.c");
    std::fs::write(
        &source,
        format!(
            r#"#include <stdio.h>
__attribute__((noinline, annotate("+vm_virtualize,vm_profile_path={}")))
int vm_bind_action(int a, int b) {{
    int sum = a + b;
    return sum * 3;
}}
int main(void) {{
    printf("%d\n", vm_bind_action(9, 4));
    return 0;
}}
"#,
            profile_dir.display()
        ),
    )
    .expect("bind-action fixture should be writable");

    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_bind_action_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();
    assert_eq!("39\n", baseline_output.stdout());

    let virtualized =
        compile_virtualized_binary_with_config(&source, "vm_virtualize_bind_action", ObfuscationConfig::disabled());
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_ne!(baseline_output.stdout(), virtualized_output.stdout());
    assert!(
        matches!(virtualized_output.stdout().as_str(), "27\n" | "12\n"),
        "rebinding add to one materialized operand should feed that operand into the multiply, got {}",
        virtualized_output.stdout()
    );
}

#[test]
#[serial]
fn test_vm_virtualize_vreg_action_drives_result_register_variable() {
    ensure_plugin_built();

    let profile_dir = add_vreg_renamed_profile_path();
    let source = output_dir().join("vm_virtualize_vreg_action.c");
    std::fs::write(
        &source,
        format!(
            r#"#include <stdio.h>
__attribute__((noinline, annotate("+vm_virtualize,vm_profile_path={}")))
int vm_vreg_action(int a, int b) {{
    return a + b;
}}
int main(void) {{
    printf("%d\n", vm_vreg_action(9, 4));
    return 0;
}}
"#,
            profile_dir.display()
        ),
    )
    .expect("vreg-action fixture should be writable");

    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_vreg_action_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();
    assert_eq!("13\n", baseline_output.stdout());

    let virtualized =
        compile_virtualized_binary_with_config(&source, "vm_virtualize_vreg_action", ObfuscationConfig::disabled());
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir_path =
        compile_virtualized_ir_with_config(&source, "vm_virtualize_vreg_action.ll", ObfuscationConfig::disabled());
    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_vreg_action"));
}

#[test]
#[serial]
fn test_vm_virtualize_call_native_emit_action_drives_record() {
    ensure_plugin_built();

    let profile_dir = call_native_callee_literal_profile_path();
    let source = output_dir().join("vm_virtualize_call_native_emit_action.c");
    std::fs::write(
        &source,
        format!(
            r#"#include <stdio.h>
__attribute__((noinline))
int native_add11(int x) {{
    volatile int salt = 11;
    return x + salt;
}}
__attribute__((noinline, annotate("+vm_virtualize,vm_profile_path={}")))
int vm_call_native_emit_action(int x) {{
    return native_add11(x);
}}
int main(void) {{
    printf("%d\n", vm_call_native_emit_action(9));
    return 0;
}}
"#,
            profile_dir.display()
        ),
    )
    .expect("call-native emit-action fixture should be writable");

    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_call_native_emit_action_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();
    assert_eq!("20\n", baseline_output.stdout());

    let virtualized = compile_virtualized_binary_with_config(
        &source,
        "vm_virtualize_call_native_emit_action",
        ObfuscationConfig::disabled(),
    );
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(
        "0\n",
        virtualized_output.stdout(),
        "rewriting call_native callee operand in lowering.vm should affect the emitted VM call record"
    );
}

#[test]
#[serial]
fn test_vm_virtualize_call_native_writes_final_return_slot_without_copy() {
    ensure_plugin_built();

    let source = output_dir().join("vm_virtualize_call_native_direct_ret_slot.c");
    std::fs::write(
        &source,
        r#"#include <stdio.h>
__attribute__((noinline))
int native_value(void) {
    volatile int value = 41;
    return value + 1;
}
__attribute__((noinline, annotate("+vm_virtualize")))
int vm_native_ret_slot(void) {
    return native_value();
}
int main(void) {
    printf("%d\n", vm_native_ret_slot());
    return 0;
}
"#,
    )
    .expect("native return-slot fixture should be writable");

    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_call_native_direct_ret_slot_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();
    assert_eq!("42\n", baseline_output.stdout());

    let mut config = ObfuscationConfig::disabled();
    config.vm_dump_bytecode = Some(true);
    let virtualized =
        compile_virtualized_binary_with_config(&source, "vm_virtualize_call_native_direct_ret_slot", config.clone());
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let (ir_path, output) =
        compile_virtualized_ir_with_debug_log(&source, "vm_virtualize_call_native_direct_ret_slot.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_native_ret_slot");
    assert!(dump.contains(": call_native "));
    assert!(dump.contains(": ret "));
    assert!(
        !dump.contains(": mov "),
        "call_native should write the final return slot directly, dump was:\n{dump}"
    );

    let ir = std::fs::read_to_string(ir_path).expect("native return-slot output IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_native_ret_slot"));
    assert!(ir.contains("handler.call_native"));
}

#[test]
#[serial]
fn test_vm_virtualize_direct_varargs_native_call_matches_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_varargs_native.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_varargs_native'
source_filename = "vm_virtualize_varargs_native.ll"

@fmt = private unnamed_addr constant [13 x i8] c"%d:%lld:%.1f\00"

declare i32 @snprintf(ptr, i64, ptr, ...)

define i64 @vm_varargs_native(ptr %buf, i32 %x, i64 %salt, double %scale) #0 {
entry:
  %written = call i32 (ptr, i64, ptr, ...) @snprintf(ptr %buf, i64 96, ptr @fmt, i32 %x, i64 %salt, double %scale)
  %w64 = sext i32 %written to i64
  %p0 = getelementptr i8, ptr %buf, i64 0
  %b0 = load i8, ptr %p0, align 1
  %p1 = getelementptr i8, ptr %buf, i64 1
  %b1 = load i8, ptr %p1, align 1
  %p2 = getelementptr i8, ptr %buf, i64 2
  %b2 = load i8, ptr %p2, align 1
  %p3 = getelementptr i8, ptr %buf, i64 3
  %b3 = load i8, ptr %p3, align 1
  %p4 = getelementptr i8, ptr %buf, i64 4
  %b4 = load i8, ptr %p4, align 1
  %p5 = getelementptr i8, ptr %buf, i64 5
  %b5 = load i8, ptr %p5, align 1
  %p6 = getelementptr i8, ptr %buf, i64 6
  %b6 = load i8, ptr %p6, align 1
  %p7 = getelementptr i8, ptr %buf, i64 7
  %b7 = load i8, ptr %p7, align 1
  %z0 = zext i8 %b0 to i64
  %z1 = zext i8 %b1 to i64
  %z2 = zext i8 %b2 to i64
  %z3 = zext i8 %b3 to i64
  %z4 = zext i8 %b4 to i64
  %z5 = zext i8 %b5 to i64
  %z6 = zext i8 %b6 to i64
  %z7 = zext i8 %b7 to i64
  %r0 = add i64 %w64, %z0
  %r1 = shl i64 %z1, 8
  %r2 = xor i64 %r0, %r1
  %r3 = shl i64 %z2, 16
  %r4 = xor i64 %r2, %r3
  %r5 = shl i64 %z3, 24
  %r6 = add i64 %r4, %r5
  %r7 = shl i64 %z4, 32
  %r8 = xor i64 %r6, %r7
  %r9 = shl i64 %z5, 40
  %r10 = add i64 %r8, %r9
  %r11 = shl i64 %z6, 48
  %r12 = xor i64 %r10, %r11
  %r13 = shl i64 %z7, 56
  %r14 = add i64 %r12, %r13
  %ret = xor i64 %r14, %salt
  ret i64 %ret
}

attributes #0 = { noinline optnone }
"#,
    )
    .expect("varargs native LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_varargs_native_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

uint64_t vm_varargs_native(char *buf, int32_t x, uint64_t salt, double scale);

int main(void) {
    char first[96];
    char second[96];
    uint64_t a = vm_varargs_native(first, 37, 123456ULL, 4.5);
    uint64_t b = vm_varargs_native(second, -8, 987654321ULL, -2.25);
    printf("%llu:%s\n%llu:%s\n", (unsigned long long)a, first, (unsigned long long)b, second);
    return 0;
}
"#,
    )
    .expect("varargs native C harness should be writable");

    let baseline = compile_ir_with_c_harness(&ir_source, &harness, "vm_virtualize_varargs_native_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_dump_bytecode = Some(true);
    let (virtualized_ir, output) = optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_varargs_native.ll", config);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);
    let dump = bytecode_dump_for_function(&stderr, "vm_varargs_native");
    assert!(
        dump.contains(": call_native "),
        "direct varargs native call should lower through call_native:\n{dump}"
    );

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_varargs_native");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized varargs native IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_varargs_native"));
    assert!(ir.contains(".amice.vm.native_thunk.vm_varargs_native"));
    assert!(ir.contains("handler.call_native"));
}

#[test]
#[serial]
fn test_vm_virtualize_ir_contains_runtime_and_bytecode() {
    ensure_plugin_built();

    let ir_path = compile_virtualized_ir(
        &fixture_path("vm_virtualize", "basic.c", Language::C),
        "vm_virtualize_basic.ll",
    );

    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(ir.contains(".amice.vm.dispatch"));
    assert!(ir.contains(".amice.vm.read_varint"));
    assert!(ir.contains(".amice.vm.read_const"));
    assert!(!ir.contains(".amice.vm.dispatch.vm_mix"));
    assert!(!ir.contains(".amice.vm.read_varint.vm_mix"));
    assert!(ir.contains("alloca [65 x <16 x i8>]"));
    assert!(ir.contains(".amice.vm.bytecode."));
    assert!(ir.contains(".amice.vm.bytecode.vm_branch"));
    assert!(ir.contains(".amice.vm.bytecode.vm_loop"));
    assert!(ir.contains(".amice.vm.bytecode.vm_switch"));
    assert!(ir.contains(".amice.vm.bytecode.vm_memory"));
    assert!(ir.contains(".amice.vm.bytecode.vm_void_pointer"));
    assert!(ir.contains(".amice.vm.bytecode.vm_ptr_roundtrip"));
    assert!(ir.contains(".amice.vm.bytecode.vm_dynamic_gep2"));
    assert!(ir.contains(".amice.vm.bytecode.vm_reg_reuse_chain"));
    assert!(ir.contains(".amice.vm.bytecode.vm_multiblock_reuse"));
    assert!(ir.contains(".amice.vm.bytecode.vm_const_pool"));
    assert!(ir.contains(".amice.vm.bytecode.vm_sret_big"));
    assert!(
        ir.lines()
            .find(|line| line.starts_with("define ") && line.contains("@vm_sret_big("))
            .is_some_and(|line| line.contains("sret(")),
        "vm_sret_big wrapper must preserve the sret ABI attribute"
    );
    assert!(ir.contains(".amice.vm.bytecode.vm_pair"));
    assert!(ir.contains(".amice.vm.bytecode.vm_safe_skip_call"));
    assert!(ir.contains(".amice.vm.bytecode.vm_native_pair"));
    assert!(ir.contains(".amice.vm.bytecode.vm_native_sret"));
    assert!(ir.contains(".amice.vm.bytecode.vm_native_float"));
    assert!(ir.contains(".amice.vm.bytecode.vm_float32_mix"));
    assert!(ir.contains(".amice.vm.bytecode.vm_float64_mix"));
    assert!(ir.contains(".amice.vm.bytecode.vm_float_cast_mix"));
    assert!(ir.contains(".amice.vm.native_table.vm_safe_skip_call"));
    assert!(ir.contains(".amice.vm.native_thunk.vm_safe_skip_call"));
    assert!(ir.contains(".amice.vm.native_table.vm_native_pair"));
    assert!(ir.contains(".amice.vm.native_thunk.vm_native_pair"));
    assert!(ir.contains(".amice.vm.native_table.vm_native_sret"));
    assert!(ir.contains(".amice.vm.native_thunk.vm_native_sret"));
    assert!(ir.contains(".amice.vm.native_table.vm_native_float"));
    assert!(ir.contains(".amice.vm.native_thunk.vm_native_float"));
    assert!(
        ir.lines().any(|line| line.contains("call fastcc void @native_big(")
            && line.contains("sret(")
            && line.contains("amice.vm.native.arg.ptr")),
        "native sret thunk call must preserve callee ABI attributes"
    );
    assert!(ir.contains("handler.iadd.op"));
    assert!(ir.contains("handler.fadd"));
    assert!(ir.contains("handler.fneg"));
    assert!(ir.contains("handler.sitofp"));
    assert!(ir.contains("handler.uitofp"));
    assert!(ir.contains("handler.fptosi"));
    assert!(ir.contains("handler.fptoui"));
    assert!(ir.contains("handler.fptrunc"));
    assert!(ir.contains("handler.fpext"));
    assert!(ir.contains("handler.fcmp"));
    assert!(ir.contains(".split.entry"));
    assert!(ir.contains(".split.body"));
    assert!(ir.contains("handler.vm_call"));
    assert!(ir.contains("handler.vm_ret"));
    assert!(ir.contains("AMICE_VMP_RUNTIME_BYTECODE"));
}

#[test]
#[serial]
fn test_vm_virtualize_handler_clone_profile_clones_runtime_per_function() {
    ensure_plugin_built();

    let mut config = vm_virtualize_config();
    config.vm_profile_path = Some(handler_clone_profile_path().to_string_lossy().into_owned());
    let ir_path = compile_virtualized_ir_with_config(
        &fixture_path("vm_virtualize", "basic.c", Language::C),
        "vm_virtualize_handler_clone.ll",
        config,
    );

    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(ir.contains(".amice.vm.dispatch.vm_mix"));
    assert!(ir.contains(".amice.vm.dispatch.vm_loop"));
    assert!(ir.contains(".amice.vm.read_varint.vm_mix"));
    assert!(ir.contains(".amice.vm.read_varint.vm_loop"));
}

#[test]
#[serial]
fn test_vm_virtualize_module_bytecode_profile_uses_shared_global() {
    ensure_plugin_built();

    let source = fixture_path("vm_virtualize", "basic.c", Language::C);
    let baseline = CppCompileBuilder::new(&source, "vm_virtualize_module_bytecode_baseline")
        .optimization("O1")
        .without_plugin()
        .compile();
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let mut config = vm_virtualize_config();
    config.vm_profile_path = Some(module_bytecode_profile_path().to_string_lossy().into_owned());
    let virtualized = compile_virtualized_binary_with_config(&source, "vm_virtualize_module_bytecode", config.clone());
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir_path = compile_virtualized_ir_with_config(&source, "vm_virtualize_module_bytecode.ll", config);
    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(ir.contains(".amice.vm.bytecode.module"));
    assert!(!ir.contains(".amice.vm.bytecode.vm_mix"));
    assert!(!ir.contains(".amice.vm.bytecode.vm_loop"));
}

#[test]
#[serial]
fn test_vm_virtualize_unsupported_function_logs_debug_skip() {
    let source = fixture_path("vm_virtualize", "basic.c", Language::C);
    let (ir_path, output) =
        compile_virtualized_ir_with_debug_log(&source, "vm_virtualize_debug_skip.ll", vm_virtualize_config());
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);

    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(!ir.contains(".amice.vm.bytecode.vm_vector_skip"));
    assert!(stderr.contains("skip function"));
    assert!(stderr.contains("vm_vector_skip"));
}

#[test]
#[serial]
fn test_vm_virtualize_rust_fixture_matches_baseline() {
    let rust_source = rust_fixture_project_path("vm_virtualize").join("simple.rs");
    let baseline_ir = compile_rust_fixture_to_ir(&rust_source, "vm_virtualize_rust_baseline.input.ll");
    let virtualized_ir = optimize_ir_with_plugin(
        &baseline_ir,
        "vm_virtualize_rust_virtualized.ll",
        vm_virtualize_config(),
    );

    let harness = output_dir().join("vm_virtualize_rust_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdio.h>
int vm_rust_mix(int a, int b);
int main(void) {
    printf("%d\n", vm_rust_mix(11, 4));
    return 0;
}
"#,
    )
    .expect("Rust VMP harness should be writable");

    let baseline = compile_ir_with_c_harness(&baseline_ir, &harness, "vm_virtualize_rust_baseline");
    baseline.assert_success();
    let baseline_output = baseline.run();
    baseline_output.assert_success();

    let virtualized = compile_ir_with_c_harness(&virtualized_ir, &harness, "vm_virtualize_rust_virtualized");
    virtualized.assert_success();
    let virtualized_output = virtualized.run();
    virtualized_output.assert_success();
    assert_eq!(baseline_output.stdout(), virtualized_output.stdout());

    let ir = std::fs::read_to_string(virtualized_ir).expect("virtualized Rust LLVM IR should be readable");
    assert!(ir.contains(".amice.vm.bytecode.vm_rust_mix"));
}
