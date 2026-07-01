//! AMICE VMP 指令级虚拟化集成测试。

mod common;

use common::{
    CompileResult, CppCompileBuilder, Language, LlvmConfig, ObfuscationConfig, clang_compiler_path, detect_llvm_config,
    ensure_plugin_built, fixture_path, output_dir, plugin_path, rust_fixture_project_path, sanitize_ir_for_llvm21,
};
use serial_test::serial;
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
        .arg("-passes=default<O1>")
        .arg("-S")
        .arg(input_ir)
        .arg("-o")
        .arg(&output_ir);
    config.apply_to_command(&mut opt_command);

    let output = command_output(&mut opt_command);
    (output_ir, output)
}

fn compile_ir_with_c_harness(ir_path: &Path, c_source: &Path, output_name: &str) -> CompileResult {
    let binary_path = output_dir().join(output_name);
    let output = command_output(
        Command::new(clang_compiler_path(false))
            .env("CCACHE_DISABLE", "1")
            .arg(ir_path)
            .arg(c_source)
            .arg("-o")
            .arg(&binary_path),
    );

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
fn test_vm_virtualize_integer_intrinsics_match_baseline() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_integer_intrinsics.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_integer_intrinsics'
source_filename = "vm_virtualize_integer_intrinsics.ll"

declare i32 @llvm.ctpop.i32(i32)
declare i64 @llvm.ctpop.i64(i64)
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
  %swap32 = call i32 @llvm.bswap.i32(i32 %a)
  %rev32 = call i32 @llvm.bitreverse.i32(i32 %a)
  %fshl32 = call i32 @llvm.fshl.i32(i32 %a, i32 %swap32, i32 %pop32)
  %fshr32 = call i32 @llvm.fshr.i32(i32 %swap32, i32 %a, i32 %pop32)
  %pop64 = call i64 @llvm.ctpop.i64(i64 %b)
  %swap64 = call i64 @llvm.bswap.i64(i64 %b)
  %rev64 = call i64 @llvm.bitreverse.i64(i64 %b)
  %fshl64 = call i64 @llvm.fshl.i64(i64 %b, i64 %swap64, i64 %pop64)
  %fshr64 = call i64 @llvm.fshr.i64(i64 %swap64, i64 %b, i64 %pop64)
  %pop32x = zext i32 %pop32 to i64
  %swap32x = zext i32 %swap32 to i64
  %rev32x = zext i32 %rev32 to i64
  %fshl32x = zext i32 %fshl32 to i64
  %fshr32x = zext i32 %fshr32 to i64
  %a0 = xor i64 %pop32x, %swap32x
  %a1 = add i64 %a0, %rev32x
  %a2 = xor i64 %a1, %pop64
  %a3 = add i64 %a2, %swap64
  %a4 = xor i64 %a3, %rev64
  %a5 = add i64 %a4, %fshl32x
  %a6 = xor i64 %a5, %fshr32x
  %a7 = add i64 %a6, %fshl64
  %a8 = xor i64 %a7, %fshr64
  ret i64 %a8
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

int main(void) {
    uint64_t result = vm_integer_intrinsics(0x12345678u, 0x0123456789abcdefULL);
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
    assert!(ir.contains("handler.ctpop"));
    assert!(ir.contains("handler.bswap"));
    assert!(ir.contains("handler.bitreverse"));
    assert!(ir.contains("handler.fshl"));
    assert!(ir.contains("handler.fshr"));
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
fn test_vm_virtualize_dynamic_memcpy_safely_skips() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_dynamic_memcpy.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_dynamic_memcpy'
source_filename = "vm_virtualize_dynamic_memcpy.ll"

declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1 immarg)

define void @vm_dynamic_memcpy(ptr %dst, ptr %src, i64 %n) {
entry:
  call void @llvm.memcpy.p0.p0.i64(ptr %dst, ptr %src, i64 %n, i1 false)
  ret void
}
"#,
    )
    .expect("dynamic memcpy LLVM IR fixture should be writable");

    let (output_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_dynamic_memcpy.ll", vm_virtualize_config());
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);

    let ir = std::fs::read_to_string(output_ir).expect("dynamic memcpy output IR should be readable");
    assert!(!ir.contains(".amice.vm.bytecode.vm_dynamic_memcpy"));
    assert!(stderr.contains("skip function"));
    assert!(stderr.contains("vm_dynamic_memcpy"));
    assert!(stderr.contains("memory intrinsic length must be a compile-time constant"));
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
  ret i32 %j
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

int main(void) {
    int32_t value = 0x1234;
    int32_t result = vm_atomicrmw_mix(&value, 0x22);
    printf("%d:%d\n", result, value);
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
"#,
    )
    .expect("cmpxchg LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_cmpxchg_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdint.h>
#include <stdio.h>

int32_t vm_cmpxchg_mix(int32_t *p, int32_t expected, int32_t desired);

int main(void) {
    int32_t value = 10;
    int32_t result = vm_cmpxchg_mix(&value, 10, 20);
    printf("%d:%d\n", result, value);
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
    assert!(ir.contains("handler.cmpxchg"));
}

#[test]
#[serial]
fn test_vm_virtualize_unsupported_atomic_ops_safely_skip() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_atomic_ops.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_atomic_ops'
source_filename = "vm_virtualize_atomic_ops.ll"

define float @vm_atomicrmw_fadd(ptr %p, float %x) {
entry:
  %old = atomicrmw fadd ptr %p, float %x monotonic, align 4
  ret float %old
}

define { i32, i1 } @vm_weak_cmpxchg(ptr %p, i32 %expected, i32 %desired) {
entry:
  %pair = cmpxchg weak ptr %p, i32 %expected, i32 %desired acquire monotonic, align 4
  ret { i32, i1 } %pair
}

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
    assert!(!ir.contains(".amice.vm.bytecode.vm_atomicrmw_fadd"));
    assert!(!ir.contains(".amice.vm.bytecode.vm_weak_cmpxchg"));
    assert!(!ir.contains(".amice.vm.bytecode.vm_scoped_fence"));

    assert!(stderr.contains("skip function"));
    assert!(stderr.contains("vm_atomicrmw_fadd"));
    assert!(stderr.contains("atomicrmw operation"));
    assert!(stderr.contains("vm_weak_cmpxchg"));
    assert!(stderr.contains("weak cmpxchg is not supported by vm_virtualize"));
    assert!(stderr.contains("vm_scoped_fence"));
    assert!(stderr.contains("fence non-default atomic syncscope is not supported by vm_virtualize"));
}

#[test]
#[serial]
fn test_vm_virtualize_unsupported_integer_intrinsic_width_safely_skip() {
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
"#,
    )
    .expect("atomic memory LLVM IR fixture should be writable");

    let harness = output_dir().join("vm_virtualize_atomic_memory_harness.c");
    std::fs::write(
        &harness,
        r#"#include <stdio.h>

int vm_atomic_load(int *p);
void vm_atomic_store(int *p, int value);

int main(void) {
    int cell = 11;
    int a = vm_atomic_load(&cell);
    vm_atomic_store(&cell, 123);
    printf("%d %d\n", a, cell);
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
fn test_vm_virtualize_volatile_memory_safely_skip() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_volatile_memory.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_volatile_memory'
source_filename = "vm_virtualize_volatile_memory.ll"

define i32 @vm_volatile_load(ptr %p) {
entry:
  %value = load volatile i32, ptr %p, align 4
  ret i32 %value
}

define void @vm_volatile_store(ptr %p, i32 %value) {
entry:
  store volatile i32 %value, ptr %p, align 4
  ret void
}
"#,
    )
    .expect("volatile memory LLVM IR fixture should be writable");

    let (output_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_volatile_memory.ll", vm_virtualize_config());
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);

    let ir = std::fs::read_to_string(output_ir).expect("volatile memory output IR should be readable");
    assert!(!ir.contains(".amice.vm.bytecode.vm_volatile_load"));
    assert!(!ir.contains(".amice.vm.bytecode.vm_volatile_store"));

    assert!(stderr.contains("skip function"));
    assert!(stderr.contains("vm_volatile_load"));
    assert!(stderr.contains("load volatile memory access is not supported by vm_virtualize"));
    assert!(stderr.contains("vm_volatile_store"));
    assert!(stderr.contains("store volatile memory access is not supported by vm_virtualize"));
}

#[test]
#[serial]
fn test_vm_virtualize_indirect_call_safely_skips() {
    ensure_plugin_built();

    let ir_source = output_dir().join("vm_virtualize_indirect_call.input.ll");
    std::fs::write(
        &ir_source,
        r#"; ModuleID = 'vm_virtualize_indirect_call'
source_filename = "vm_virtualize_indirect_call.ll"

define i32 @vm_indirect_call(ptr %callee, i32 %x) {
entry:
  %value = call i32 %callee(i32 %x)
  ret i32 %value
}
"#,
    )
    .expect("indirect call LLVM IR fixture should be writable");

    let (output_ir, output) =
        optimize_ir_with_plugin_debug(&ir_source, "vm_virtualize_indirect_call.ll", vm_virtualize_config());
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);

    let ir = std::fs::read_to_string(output_ir).expect("indirect call output IR should be readable");
    assert!(!ir.contains(".amice.vm.bytecode.vm_indirect_call"));
    assert!(stderr.contains("skip function"));
    assert!(stderr.contains("vm_indirect_call"));
    assert!(stderr.contains("indirect calls are not supported by vm_virtualize"));
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

define void @vm_unreachable() {
entry:
  unreachable
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
    assert!(!ir.contains(".amice.vm.bytecode.vm_unreachable"));
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
    assert!(stderr.contains("vm_unreachable"));
    assert!(stderr.contains("unreachable terminator is not supported by vm_virtualize"));
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
