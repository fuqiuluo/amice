//! AMICE VMP 指令级虚拟化集成测试。

mod common;

use common::{
    CompileResult, CppCompileBuilder, Language, LlvmConfig, ObfuscationConfig, clang_compiler_path, detect_llvm_config,
    ensure_plugin_built, fixture_path, output_dir, plugin_path, rust_fixture_project_path,
};
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
        .replacen("opcode alias [0x10, 0x2c, 0x5a, 0x6d, 0x7a]", "opcode alias [0xb1]", 1);
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
opcode alias [0xb2] # iadd_alt 使用独立操作码 0xb2
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
fn test_vm_virtualize_profile_semantic_ast_drives_renamed_opcode() {
    ensure_plugin_built();

    let source = fixture_path("vm_virtualize", "basic.c", Language::C);
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
    assert!(ir.contains("handler.add_alias"));
    assert!(!ir.contains("handler.iadd"));
}

#[test]
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
        "9\n",
        virtualized_output.stdout(),
        "rewriting call_native callee operand in lowering.vm should affect the emitted VM call record"
    );
}

#[test]
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
    assert!(!ir.contains(".amice.vm.bytecode.vm_safe_skip_float"));
    assert!(ir.contains(".amice.vm.native_table.vm_safe_skip_call"));
    assert!(ir.contains(".amice.vm.native_thunk.vm_safe_skip_call"));
    assert!(ir.contains(".amice.vm.native_table.vm_native_pair"));
    assert!(ir.contains(".amice.vm.native_thunk.vm_native_pair"));
    assert!(ir.contains(".amice.vm.native_table.vm_native_sret"));
    assert!(ir.contains(".amice.vm.native_thunk.vm_native_sret"));
    assert!(
        ir.lines().any(|line| line.contains("call fastcc void @native_big(")
            && line.contains("sret(")
            && line.contains("amice.vm.native.arg.ptr")),
        "native sret thunk call must preserve callee ABI attributes"
    );
    assert!(ir.contains("handler.iadd.op"));
    assert!(ir.contains(".split.entry"));
    assert!(ir.contains(".split.body"));
    assert!(ir.contains("handler.vm_call"));
    assert!(ir.contains("handler.vm_ret"));
    assert!(ir.contains("AMICE_VMP_RUNTIME_BYTECODE"));
}

#[test]
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
fn test_vm_virtualize_unsupported_function_logs_debug_skip() {
    let source = fixture_path("vm_virtualize", "basic.c", Language::C);
    let (ir_path, output) =
        compile_virtualized_ir_with_debug_log(&source, "vm_virtualize_debug_skip.ll", vm_virtualize_config());
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_success(output);

    let ir = std::fs::read_to_string(ir_path).expect("LLVM IR output should be readable");
    assert!(!ir.contains(".amice.vm.bytecode.vm_safe_skip_float"));
    assert!(stderr.contains("skip function"));
    assert!(stderr.contains("vm_safe_skip_float"));
}

#[test]
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
