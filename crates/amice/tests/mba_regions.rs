//! Differential tests against separately compiled original LLVM IR.
mod common;

use common::{ObfuscationConfig, detect_llvm_config, ensure_plugin_built, output_dir, plugin_path};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

fn tool(name: &str) -> PathBuf {
    let name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    detect_llvm_config()
        .map(|c| PathBuf::from(c.prefix).join("bin").join(&name))
        .unwrap_or_else(|| name.into())
}

fn run(cmd: &mut Command) -> String {
    let out = cmd.output().expect("failed to start LLVM tool");
    assert!(
        out.status.success(),
        "{cmd:?}\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn transform(input: &Path, output: &Path, fp: bool, limit: u32, added: u32) {
    transform_pipeline(input, output, fp, limit, added, "default<O0>");
}

fn transform_pipeline(input: &Path, output: &Path, fp: bool, limit: u32, added: u32, pipeline: &str) {
    transform_modes(input, output, fp, limit, added, pipeline, true, true);
}

fn transform_modes(
    input: &Path,
    output: &Path,
    fp: bool,
    limit: u32,
    added: u32,
    pipeline: &str,
    expand: bool,
    guard: bool,
) {
    let mut cmd = Command::new(tool("opt"));
    ObfuscationConfig {
        mba: Some(true),
        mba_float_regions: Some(fp),
        mba_pre_expand: Some(expand),
        mba_opaque_guard: Some(guard),
        mba_max_instructions: Some(limit),
        mba_max_added_instructions: Some(added),
        ..ObfuscationConfig::disabled()
    }
    .apply_to_command(&mut cmd);
    cmd.env_remove("AMICE_CONFIG_PATH")
        .env("AMICE_PASS_ORDER", "Mba")
        .arg(format!("--load-pass-plugin={}", plugin_path().display()))
        .arg(format!("-passes={pipeline}"))
        .args(["-verify-each", "-S"])
        .arg(input)
        .arg("-o")
        .arg(output);
    run(&mut cmd);
}

fn fixture(triple: &str) -> (String, Vec<String>) {
    fixture_widths(triple, 1..=128)
}

fn fixture_widths(triple: &str, widths: impl IntoIterator<Item = u32>) -> (String, Vec<String>) {
    let mut ir = format!(
        "target triple = \"{triple}\"\n{}
",
        include_str!("c/fixtures/mba/mba_regions.ll")
    );
    let mut names = vec![
        "region_branch",
        "region_loop",
        "region_condition",
        "region_switch",
        "undef_and",
        "undef_or",
        "strict_integer",
        "soft_integer",
        "region_mutual_loop",
        "region_poison_select",
        "region_poison_rhs",
        "region_poison_condition",
        "region_undef_mask",
        "region_flags",
        "region_memory_intrinsic",
        "region_irreducible",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    for width in widths {
        for op in ["add", "sub", "and", "or", "xor"] {
            for half in 0..if width > 64 { 2 } else { 1 } {
                let name = format!("op_{op}_{width}_{half}");
                names.push(name.clone());
                ir += &format!("define i64 @{name}(i64 %xl, i64 %yl, i64 %xh, i64 %yh) noinline {{\n");
                if width > 64 {
                    ir += &format!(
                        "%a = zext i64 %xl to i{width}\n%b = zext i64 %yl to i{width}\n%c = zext i64 %xh to i{width}\n%d = zext i64 %yh to i{width}\n%e = shl i{width} %c, 64\n%f = shl i{width} %d, 64\n%x = or i{width} %a, %e\n%y = or i{width} %b, %f\n"
                    );
                } else if width < 64 {
                    ir += &format!("%x = trunc i64 %xl to i{width}\n%y = trunc i64 %yl to i{width}\n");
                }
                let (x, y) = if width == 64 { ("xl", "yl") } else { ("x", "y") };
                ir += &format!("%v = {op} i{width} %{x}, %{y}\n");
                if half == 1 {
                    ir += &format!("%h = lshr i{width} %v, 64\n");
                }
                let v = if half == 1 { "h" } else { "v" };
                if width < 64 {
                    ir += &format!("%r = zext i{width} %{v} to i64\nret i64 %r\n}}\n");
                } else if width > 64 {
                    ir += &format!("%r = trunc i{width} %{v} to i64\nret i64 %r\n}}\n");
                } else {
                    ir += "ret i64 %v\n}\n";
                }
            }
        }
    }
    (ir, names)
}

fn driver(names: &[String]) -> String {
    let mut c = String::from(
        "#include <stdint.h>\n#include <stdio.h>\n#include <fenv.h>\ntypedef uint64_t (*Fn)(uint64_t,uint64_t,uint64_t,uint64_t);\n",
    );
    for name in names {
        c += &format!(
            "extern uint64_t {name}(uint64_t,uint64_t,uint64_t,uint64_t);\nextern uint64_t reference_{name}(uint64_t,uint64_t,uint64_t,uint64_t);\n"
        );
    }
    c += "struct Pair { Fn actual, expected; const char *name; unsigned width; };\nstatic struct Pair pairs[] = {\n";
    for name in names {
        let width = name
            .strip_prefix("op_")
            .and_then(|v| v.split('_').nth(1))
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0);
        c += &format!("{{{name},reference_{name},\"{name}\",{width}}},\n");
    }
    c += r#"};
static uint64_t state = UINT64_C(0xd1342543de82ef95);
static uint64_t random_word(void) { state ^= state << 13; state ^= state >> 7; state ^= state << 17; return state; }
int main(void) {
  const uint64_t edge[] = {0,1,2,127,128,255,256,65535,65536,0x7fffffff,0x80000000,0xffffffff,0x100000000ULL,0x7fffffffffffffffULL,0x8000000000000000ULL,UINT64_MAX};
  const int modes[] = { FE_TONEAREST, FE_UPWARD, FE_DOWNWARD, FE_TOWARDZERO };
  unsigned long checks = 0;
  for (unsigned m=0; m<4; ++m) {
    if (fesetround(modes[m])) return 2;
    for (unsigned p=0; p<sizeof(pairs)/sizeof(pairs[0]); ++p) {
      unsigned width=pairs[p].width;
      unsigned cases=width>0 && width<=8 ? (1u<<(2*width)) : 1536;
      for (unsigned n=0; n<cases; ++n) {
        uint64_t x=n<256?edge[n/16]:random_word(), y=n<256?edge[n%16]:random_word();
        uint64_t z=n<256?~x:random_word(), w=n<256?~y:random_word();
        if (width>0 && width<=8) { x=n&((1u<<width)-1); y=n>>width; z=w=0; }
        else if (width>0 && n>=256 && n<256+4*width) {
          unsigned bit=(n-256)/4;
          x=bit<64?UINT64_C(1)<<bit:0; z=bit>=64?UINT64_C(1)<<(bit-64):0;
          if (n&1) { z-=x==0; --x; }
          y=(n&2)?UINT64_MAX:1; w=(n&2)?UINT64_MAX:0;
        }
        uint64_t expected=pairs[p].expected(x,y,z,w);
        feclearexcept(FE_ALL_EXCEPT);
        int sticky=(n%257)==0 ? FE_DIVBYZERO : 0;
        if (sticky && feraiseexcept(sticky)) return 3;
        uint64_t actual=pairs[p].actual(x,y,z,w);
        if (actual!=expected || fetestexcept(FE_ALL_EXCEPT)!=sticky || fegetround()!=modes[m]) {
          fprintf(stderr,"%s mode=%u case=%u expected=%llx actual=%llx\n",pairs[p].name,m,n,(unsigned long long)expected,(unsigned long long)actual); return 1;
        }
        ++checks;
      }
    }
  }
  printf("%lu comparisons passed\n",checks);
  return 0;
}
"#;
    c
}

fn body<'a>(ir: &'a str, name: &str) -> &'a str {
    ir.split_once(&format!("@{name}("))
        .unwrap()
        .1
        .split_once("\n}")
        .unwrap()
        .0
}

#[test]
fn regions_match_original_before_and_after_o2() {
    ensure_plugin_built();
    let dir = output_dir().join("mba-regions");
    std::fs::create_dir_all(&dir).unwrap();
    let triple = run(Command::new(tool("clang")).arg("-dumpmachine")).trim().to_string();
    let supported = triple.starts_with("x86_64-") || triple.starts_with("aarch64-");
    let (source, names) = fixture(&triple);
    let input = dir.join("original.ll");
    let reference = dir.join("reference.ll");
    let main = dir.join("driver.c");
    std::fs::write(&input, &source).unwrap();
    let mut reference_ir = source.clone();
    for name in &names {
        reference_ir = reference_ir.replace(&format!("@{name}("), &format!("@reference_{name}("));
    }
    std::fs::write(&reference, reference_ir).unwrap();
    std::fs::write(&main, driver(&names)).unwrap();
    for fp in [true, false] {
        let transformed = dir.join(format!("transformed-{fp}.ll"));
        transform(&input, &transformed, fp, 128, 2048);
        let ir = std::fs::read_to_string(&transformed).unwrap();
        assert!(!body(&ir, "strict_integer").contains("double"));
        assert!(!body(&ir, "soft_integer").contains("double"));
        if fp && supported {
            assert!(body(&ir, "region_loop").contains("phi i64"));
            assert!(body(&ir, "region_loop").contains("fadd double"));
            assert!(!body(&ir, "region_loop").contains("phi i32"));
            assert!(body(&ir, "region_branch").contains("select i1"));
        } else {
            assert!(!ir.contains("double"));
        }
        let repeated = dir.join(format!("repeated-{fp}.ll"));
        transform(&transformed, &repeated, fp, 128, 2048);
        let again = std::fs::read_to_string(repeated).unwrap();
        // opt changes only the ModuleID when reading a different file.
        assert_eq!(
            ir.lines().skip(1).collect::<Vec<_>>(),
            again.lines().skip(1).collect::<Vec<_>>()
        );
        for level in ["O0", "O2"] {
            let optimized = dir.join(format!("{fp}-{level}.ll"));
            run(Command::new(tool("opt"))
                .arg(format!("-passes=default<{level}>,default<{level}>"))
                .args(["-verify-each", "-S"])
                .arg(&transformed)
                .arg("-o")
                .arg(&optimized));
            if fp && supported {
                let optimized_ir = std::fs::read_to_string(&optimized).unwrap();
                assert!(
                    body(&optimized_ir, "region_loop").contains("fadd double"),
                    "FP region vanished after {level}"
                );
            }
            let exe = dir.join(format!("check-{fp}-{level}{}", std::env::consts::EXE_SUFFIX));
            let mut cmd = Command::new(tool("clang"));
            cmd.arg(format!("-{level}"))
                .args([&optimized, &reference, &main])
                .arg("-o")
                .arg(&exe);
            if !cfg!(windows) {
                cmd.arg("-lm");
            }
            run(&mut cmd);
            print!("{}", run(&mut Command::new(exe)));
        }
    }
    for (limit, added) in [(0, 2048), (128, 0), (128, 1)] {
        let out = dir.join(format!("disabled-{limit}-{added}.ll"));
        transform(&input, &out, true, limit, added);
        let ir = std::fs::read_to_string(out).unwrap();
        assert!(!ir.contains("amice.mba.done"));
        assert!(!ir.contains("mba."));
    }
}

#[test]
fn function_overrides_and_exception_boundaries() {
    ensure_plugin_built();
    let dir = output_dir().join("mba-region-boundaries");
    std::fs::create_dir_all(&dir).unwrap();
    let mut ir = String::from("target triple = \"x86_64-unknown-linux-gnu\"\n");
    let annotations = [
        ("local_integer", "+mba -mba_float_regions"),
        ("local_zero", "+mba mba_max_instructions=0"),
        ("local_one", "+mba mba_max_instructions=1"),
        ("local_cost", "+mba mba_max_added_instructions=1"),
        ("local_disabled", "-mba"),
    ];
    for (name, annotation) in annotations {
        ir += &format!(
            "define i32 @{name}(i32 %x, i32 %y) {{\n%a = add i32 %x, %y\n%b = sub i32 %a, %y\nret i32 %b\n}}\n@ann_{name} = private constant [{} x i8] c\"{annotation}\\00\", section \"llvm.metadata\"\n",
            annotation.len() + 1
        );
    }
    ir += "@llvm.global.annotations = appending global [5 x {ptr,ptr,ptr,i32,ptr}] [\n";
    ir += &annotations
        .iter()
        .map(|(name, _)| format!("{{ptr,ptr,ptr,i32,ptr}} {{ptr @{name},ptr @ann_{name},ptr null,i32 0,ptr null}}"))
        .collect::<Vec<_>>()
        .join(",\n");
    ir += r#"]
declare i32 @personality(...)
declare i32 @may_throw()
define i32 @eh_boundary(i32 %x, i32 %y) personality ptr @personality {
entry:
  %a = add i32 %x, %y
  %call = invoke i32 @may_throw() to label %normal unwind label %exception
normal:
  %q = phi i32 [ %call, %entry ]
  %r = add i32 %q, %y
  ret i32 %r
exception:
  %p = phi i32 [ %a, %entry ]
  %pad = landingpad {ptr,i32} cleanup
  %s = sub i32 %p, %y
  ret i32 %s
}

define i32 @no_fp(i32 %x, i32 %y) noimplicitfloat {
  %a = add i32 %x, %y
  ret i32 %a
}
define i32 @disabled_feature(i32 %x, i32 %y) "target-features"="-sse2" {
  %a = add i32 %x, %y
  ret i32 %a
}
"#;
    let input = dir.join("input.ll");
    let output = dir.join("output.ll");
    std::fs::write(&input, ir).unwrap();
    transform(&input, &output, true, 128, 2048);
    let out = std::fs::read_to_string(&output).unwrap();
    for name in [
        "local_integer",
        "local_zero",
        "local_cost",
        "local_disabled",
        "no_fp",
        "disabled_feature",
    ] {
        assert!(!body(&out, name).contains("double"), "{name} ignored its configuration");
    }
    for name in ["local_zero", "local_cost", "local_disabled"] {
        assert!(body(&out, name).contains("%a = add i32"), "{name} was rewritten");
    }
    assert!(body(&out, "local_one").contains("fadd double"));
    assert!(body(&out, "local_one").contains("%b = sub i32"));
    assert!(body(&out, "eh_boundary").contains("%q = phi i32"));
    assert!(body(&out, "eh_boundary").contains("%p = phi i32"));
    run(Command::new(tool("opt"))
        .args(["-passes=default<O2>", "-verify-each", "-disable-output"])
        .arg(output));
}

#[test]
fn budgets_cut_regions_without_breaking_dominance() {
    ensure_plugin_built();
    let dir = output_dir().join("mba-budgets");
    std::fs::create_dir_all(&dir).unwrap();
    // Reverse block order forces recursive emission. The last 512 candidates
    // form a region whose first input is outside the selected budget.
    let mut ir = String::from(
        "target triple = \"x86_64-unknown-linux-gnu\"\ndefine i32 @chain(i32 %x) {\nentry:\n  br label %b0\n",
    );
    for n in (0..520).rev() {
        let prev = if n == 0 { "x".to_string() } else { format!("v{}", n - 1) };
        ir += &format!("b{n}:\n  %v{n} = add i32 %{prev}, 1\n");
        ir += &if n == 519 {
            format!("  ret i32 %v{n}\n")
        } else {
            format!("  br label %b{}\n", n + 1)
        };
    }
    ir += "}\n";
    let input = dir.join("input.ll");
    std::fs::write(&input, &ir).unwrap();
    let count = |s: &str| {
        s.lines()
            .filter(|l| l.starts_with("  ") && !l.trim_start().starts_with(';'))
            .count()
    };
    for fp in [true, false] {
        for (limit, added) in [(1, 20), (7, 140), (128, 400), (512, 10240), (u32::MAX, u32::MAX)] {
            let output = dir.join(format!("{fp}-{limit}-{added}.ll"));
            transform(&input, &output, fp, limit, added);
            let result = std::fs::read_to_string(&output).unwrap();
            let rewritten = 520
                - body(&result, "chain")
                    .lines()
                    .filter(|l| l.contains(" = add i32") && l.trim_start().starts_with("%v"))
                    .count();
            let capacity = if fp { added.saturating_sub(12) / 96 } else { added / 20 };
            assert_eq!(rewritten, (limit.min(512).min(capacity)) as usize);
            assert!(count(body(&result, "chain")) <= count(body(&ir, "chain")) + added as usize);
            run(Command::new(tool("opt"))
                .args(["-passes=default<O3>", "-verify-each", "-disable-output"])
                .arg(&output));
        }
    }
    // Budget cuts inside actual loops exercise encode/decode edges at phis.
    let input = dir.join("loop.ll");
    std::fs::write(
        &input,
        format!(
            "target triple = \"x86_64-unknown-linux-gnu\"\n{}",
            include_str!("c/fixtures/mba/mba_regions.ll")
        ),
    )
    .unwrap();
    for limit in [1, 2, 3, 4, 7, 13, 31] {
        let output = dir.join(format!("loop-{limit}.ll"));
        transform(&input, &output, true, limit, 2048);
        run(Command::new(tool("opt"))
            .args(["-passes=default<O3>", "-verify-each", "-disable-output"])
            .arg(output));
    }
}

#[test]
fn target_features_and_unsupported_types() {
    ensure_plugin_built();
    let dir = output_dir().join("mba-targets");
    std::fs::create_dir_all(&dir).unwrap();
    let functions = r#"
define i32 @scalar(i32 %x, i32 %y) { %a = add i32 %x, %y
  ret i32 %a
}
define <4 x i32> @vector(<4 x i32> %x, <4 x i32> %y) { %a = add <4 x i32> %x, %y
  ret <4 x i32> %a
}
define i129 @wide(i129 %x, i129 %y) { %a = add i129 %x, %y
  ret i129 %a
}
define i32 @unsupported(i32 %x, i32 %y) { %a = mul i32 %x, %y
  %b = shl i32 %a, 3
  ret i32 %b
}
"#;
    for (n, (triple, fp)) in [
        ("x86_64-pc-windows-msvc", true),
        ("aarch64-linux-android24", true),
        ("x86_64-unknown-linux-gnu", true),
        ("i686-unknown-linux-gnu", false),
        ("armv7-linux-gnueabihf", false),
        ("riscv64-unknown-linux-gnu", false),
        ("wasm32-unknown-unknown", false),
        ("aarch64_be-unknown-linux-gnu", false),
        ("", false),
    ]
    .into_iter()
    .enumerate()
    {
        let input = dir.join(format!("target-{n}.ll"));
        let output = dir.join(format!("out-{n}.ll"));
        std::fs::write(&input, format!("target triple = \"{triple}\"\n{functions}")).unwrap();
        transform(&input, &output, true, 128, 2048);
        let result = std::fs::read_to_string(&output).unwrap();
        assert_eq!(body(&result, "scalar").contains("double"), fp, "{triple}");
        for name in ["vector", "wide", "unsupported"] {
            assert!(!body(&result, name).contains("mba."));
        }
        if !triple.is_empty() {
            run(Command::new(tool("llc"))
                .args(["-verify-machineinstrs", "-filetype=obj"])
                .arg(&output)
                .arg("-o")
                .arg(dir.join(format!("target-{n}.o"))));
        }
    }
    for (n, (triple, attr)) in [
        ("x86_64-pc-windows-msvc", "\"target-features\"=\"-sse,-sse2\""),
        ("aarch64-linux-android24", "\"target-features\"=\"-fp-armv8,-neon\""),
        ("x86_64-unknown-linux-gnu", "\"use-soft-float\"=\"true\""),
        ("aarch64-linux-android24", "noimplicitfloat"),
        ("x86_64-unknown-linux-gnu", "strictfp"),
    ]
    .into_iter()
    .enumerate()
    {
        let input = dir.join(format!("feature-{n}.ll"));
        let output = dir.join(format!("feature-out-{n}.ll"));
        std::fs::write(&input,format!("target triple = \"{triple}\"\ndefine i32 @scalar(i32 %x,i32 %y) {attr} {{\n%a = add i32 %x,%y\nret i32 %a\n}}\n")).unwrap();
        transform(&input, &output, true, 128, 2048);
        assert!(!std::fs::read_to_string(&output).unwrap().contains("double"));
        run(Command::new(tool("llc"))
            .args(["-verify-machineinstrs", "-filetype=obj"])
            .arg(&output)
            .arg("-o")
            .arg(dir.join(format!("feature-{n}.o"))));
    }
}

#[test]
fn preoptimized_ir_and_lto_preserve_results() {
    ensure_plugin_built();
    let dir = output_dir().join("mba-pipelines");
    std::fs::create_dir_all(&dir).unwrap();
    let triple = run(Command::new(tool("clang")).arg("-dumpmachine")).trim().to_string();
    let (source, names) = fixture_widths(&triple, [1, 8, 16, 32, 64, 128]);
    let input = dir.join("input.ll");
    let reference = dir.join("reference.ll");
    let main = dir.join("main.c");
    // Rename all definitions to keep the two modules independent.
    let all_names = names.clone();
    let mut baseline = source.clone();
    for name in all_names {
        baseline = baseline.replace(&format!("@{name}("), &format!("@reference_{name}("));
    }
    std::fs::write(&input, source).unwrap();
    std::fs::write(&reference, baseline).unwrap();
    std::fs::write(&main, driver(&names)).unwrap();
    for (n, pipeline) in [
        "default<O1>",
        "default<O2>",
        "default<O3>",
        "default<Os>",
        "default<Oz>",
        "default<O2>,lto<O2>",
    ]
    .into_iter()
    .enumerate()
    {
        let output = dir.join(format!("pipeline-{n}.ll"));
        transform_pipeline(&input, &output, true, 128, 2048, pipeline);
        assert!(std::fs::read_to_string(&output).unwrap().contains("amice.mba.done"));
        let exe = dir.join(format!("check-{n}{}", std::env::consts::EXE_SUFFIX));
        let mut cmd = Command::new(tool("clang"));
        cmd.args(["-O3", "-flto", "-fuse-ld=lld"])
            .args([&output, &reference, &main])
            .arg("-o")
            .arg(&exe);
        if !cfg!(windows) {
            cmd.arg("-lm");
        }
        run(&mut cmd);
        print!("{}", run(&mut Command::new(exe)));
    }
}

#[test]
fn exception_unwinding_and_cleanup_match_baseline() {
    use common::{CppCompileBuilder, Language, fixture_path};
    ensure_plugin_built();
    let source = fixture_path("mba", "mba_exceptions.cpp", Language::Cpp);
    for level in ["O0", "O2", "O3"] {
        let reference = CppCompileBuilder::new(&source, &format!("mba-exception-reference-{level}"))
            .without_plugin()
            .optimization(level)
            .compile();
        reference.assert_success();
        let baseline = reference.run();
        baseline.assert_success();
        for fp in [true, false] {
            let result = CppCompileBuilder::new(&source, &format!("mba-exception-{fp}-{level}"))
                .config(ObfuscationConfig {
                    mba: Some(true),
                    mba_float_regions: Some(fp),
                    ..ObfuscationConfig::disabled()
                })
                .optimization(level)
                .compile();
            result.assert_success();
            let execution = result.run();
            execution.assert_success();
            assert_eq!(execution.stdout(), baseline.stdout());
        }
    }
}

#[test]
fn annotation_storage_boundaries_do_not_crash_or_read_filenames() {
    ensure_plugin_built();
    let dir = output_dir().join("mba-annotations");
    std::fs::create_dir_all(&dir).unwrap();
    let cases = [
        ("sectionless", "+mba -mba_float_regions", false),
        ("unterminated", "+mba -mba_float_regions", false),
        ("embedded", "+mba -mba_float_regions\0 -mba", false),
        ("filename", "+mba", true),
        ("empty", "", true),
        ("zero", "\0", true),
        ("utf8", "+mba \u{fffd}", true),
    ];
    let mut source = String::from(
        "target triple = \"x86_64-unknown-linux-gnu\"\n@source_filename = private constant [19 x i8] c\"-mba_float_regions\\00\", section \"llvm.metadata\"\n",
    );
    for (name, annotation, _) in cases {
        let mut bytes = annotation.as_bytes().to_vec();
        if name == "utf8" {
            bytes = b"+mba \xff".to_vec();
        }
        if !["unterminated", "empty", "zero"].contains(&name) {
            bytes.push(0);
        }
        let encoded = bytes.iter().map(|b| format!("\\{b:02X}")).collect::<String>();
        source += &format!(
            "@text_{name} = private constant [{} x i8] c\"{encoded}\"\ndefine i32 @{name}(i32 %x,i32 %y) {{\n%a = add i32 %x,%y\nret i32 %a\n}}\n",
            bytes.len()
        );
    }
    source += &format!(
        "@llvm.global.annotations = appending global [{} x {{ptr,ptr,ptr,i32,ptr}}] [\n",
        cases.len()
    );
    source += &cases
        .iter()
        .map(|(name, _, _)| {
            format!("{{ptr,ptr,ptr,i32,ptr}} {{ptr @{name},ptr @text_{name},ptr @source_filename,i32 0,ptr null}}")
        })
        .collect::<Vec<_>>()
        .join(",\n");
    source += "]\n";
    let input = dir.join("input.ll");
    let output = dir.join("output.ll");
    std::fs::write(&input, source).unwrap();
    transform(&input, &output, true, 128, 2048);
    let result = std::fs::read_to_string(output).unwrap();
    for (name, _, fp) in cases {
        assert_eq!(body(&result, name).contains("double"), fp, "{name}");
    }
}

#[test]
fn windows_funclets_and_inline_asm_outputs() {
    use common::{CppCompileBuilder, Language, fixture_path};
    ensure_plugin_built();
    let dir = output_dir().join("mba-special-control");
    std::fs::create_dir_all(&dir).unwrap();
    let input = fixture_path("mba", "mba_funclets.ll", Language::C);
    for fp in [true, false] {
        let output = dir.join(format!("funclets-{fp}.ll"));
        transform(&input, &output, fp, 128, 2048);
        run(Command::new(tool("llc"))
            .args(["-verify-machineinstrs", "-filetype=obj"])
            .arg(&output)
            .arg("-o")
            .arg(dir.join(format!("funclets-{fp}.obj"))));
        run(Command::new(tool("opt"))
            .args(["-passes=default<O2>", "-verify-each", "-disable-output"])
            .arg(output));
    }
    let source = fixture_path("mba", "mba_callbr.c", Language::C);
    let ir = dir.join("callbr.ll");
    run(Command::new(tool("clang"))
        .args(["-O1", "-S", "-emit-llvm"])
        .arg(&source)
        .arg("-o")
        .arg(&ir));
    assert!(std::fs::read_to_string(&ir).unwrap().contains("callbr"));
    for fp in [true, false] {
        let output = dir.join(format!("callbr-{fp}.ll"));
        transform(&ir, &output, fp, 128, 2048);
        for level in ["O0", "O2"] {
            let result = CppCompileBuilder::new(&output, &format!("mba-callbr-{fp}-{level}"))
                .without_plugin()
                .optimization(level)
                .compile();
            result.assert_success();
            result.run().assert_success();
        }
    }
}

#[test]
fn composite_modes_seed_extremes_and_threads() {
    ensure_plugin_built();
    let dir = output_dir().join("mba-composite");
    std::fs::create_dir_all(&dir).unwrap();
    let triple = run(Command::new(tool("clang")).arg("-dumpmachine")).trim().to_string();
    let (source, names) = fixture_widths(&triple, [1, 8, 16, 32]);
    let input = dir.join("input.ll");
    let reference = dir.join("reference.ll");
    let main = dir.join("main.c");
    let mut baseline = source.clone();
    for name in &names {
        baseline = baseline.replace(&format!("@{name}("), &format!("@reference_{name}("));
    }
    std::fs::write(&input, source).unwrap();
    std::fs::write(&reference, baseline).unwrap();
    std::fs::write(&main, driver(&names)).unwrap();
    for expand in [false, true] {
        for guard in [false, true] {
            let output = dir.join(format!("{expand}-{guard}.ll"));
            transform_modes(&input, &output, true, 128, 8192, "default<O0>", expand, guard);
            let ir = std::fs::read_to_string(&output).unwrap();
            assert_eq!(ir.contains("@.amice.mba.seed"), guard);
            assert_eq!(
                body(&ir, "region_loop").matches("load volatile i32").count(),
                usize::from(guard)
            );
            let add = body(&ir, "op_add_32_0");
            let fp_ops = add.matches("fadd double").count() + add.matches("fsub double").count();
            assert_eq!(fp_ops, if expand { 4 } else { 2 });
            // Every seed value is valid; the initializer is not an invariant
            // that the arithmetic is allowed to assume.
            let seeds: &[u32] = if guard { &[0, 0x1ffff, u32::MAX] } else { &[0] };
            for &seed in seeds {
                let changed = ir
                    .lines()
                    .map(|line| {
                        if line.starts_with("@.amice.mba.seed") {
                            format!(
                                "{} = private global i32 {seed}, align 4",
                                line.split_once(" = ").unwrap().0
                            )
                        } else {
                            line.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let mutated = dir.join(format!("{expand}-{guard}-{seed}.ll"));
                let optimized = dir.join(format!("{expand}-{guard}-{seed}-o3.ll"));
                std::fs::write(&mutated, changed).unwrap();
                run(Command::new(tool("opt"))
                    .args(["-passes=default<O3>,lto<O2>", "-verify-each", "-S"])
                    .arg(&mutated)
                    .arg("-o")
                    .arg(&optimized));
                let optimized_ir = std::fs::read_to_string(&optimized).unwrap();
                assert!(body(&optimized_ir, "region_loop").contains("fadd double"));
                assert_eq!(
                    body(&optimized_ir, "region_loop").matches("load volatile i32").count(),
                    usize::from(guard)
                );
                let exe = dir.join(format!("check-{expand}-{guard}-{seed}{}", std::env::consts::EXE_SUFFIX));
                let mut cmd = Command::new(tool("clang"));
                cmd.args(["-O3", "-flto", "-fuse-ld=lld"])
                    .args([&optimized, &reference, &main])
                    .arg("-o")
                    .arg(&exe);
                if !cfg!(windows) {
                    cmd.arg("-lm");
                }
                run(&mut cmd);
                print!("{}", run(&mut Command::new(exe)));
            }
        }
    }
    #[cfg(unix)]
    {
        let main = dir.join("threads.c");
        std::fs::write(
            &main,
            r#"
#include <stdint.h>
#include <pthread.h>
extern uint64_t region_mutual_loop(uint64_t,uint64_t,uint64_t,uint64_t);
extern uint64_t reference_region_mutual_loop(uint64_t,uint64_t,uint64_t,uint64_t);
static void *worker(void *arg) {
  uint64_t s=(uintptr_t)arg+1;
  for(unsigned i=0;i<4096;++i) {
    s^=s<<13;s^=s>>7;s^=s<<17;
    if(region_mutual_loop(s,~s,i,0)!=reference_region_mutual_loop(s,~s,i,0)) return (void*)1;
  }
  return 0;
}
int main(void) {
  pthread_t threads[4];
  for(uintptr_t i=0;i<4;++i) if(pthread_create(&threads[i],0,worker,(void*)i)) return 2;
  for(unsigned i=0;i<4;++i) {void *result;if(pthread_join(threads[i],&result)||result) return 1;}
  return 0;
}
"#,
        )
        .unwrap();
        let exe = dir.join("threads");
        run(Command::new(tool("clang"))
            .args(["-O3", "-pthread"])
            .arg(dir.join("true-true-4294967295-o3.ll"))
            .args([&reference, &main])
            .arg("-o")
            .arg(&exe));
        run(&mut Command::new(exe));
    }
}

#[test]
fn carriers_follow_live_data_across_all_scales() {
    ensure_plugin_built();
    let dir = output_dir().join("mba-live-carriers");
    std::fs::create_dir_all(&dir).unwrap();
    let triple = run(Command::new(tool("clang")).arg("-dumpmachine")).trim().to_string();
    let input = dir.join("probe.ll");
    let output = dir.join("transformed.ll");
    std::fs::write(
        &input,
        format!(
            "target triple = \"{triple}\"\ndefine i64 @probe(i32 %x, i32 %y) noinline {{\n%v = add i32 %x, %y\n%r = zext i32 %v to i64\nret i64 %r\n}}\n"
        ),
    ).unwrap();
    // Expose the actual generated carrier to check representation behavior,
    // not just the final payload. This does not alter the production emitter.
    transform_modes(&input, &output, true, 128, 2048, "default<O0>", false, true);
    let ir = std::fs::read_to_string(&output).unwrap();
    let carrier = body(&ir, "probe")
        .lines()
        .find(|line| line.contains(" = trunc i64 ") && line.contains(" to i32"))
        .unwrap()
        .split_whitespace()
        .nth(4)
        .unwrap();
    let exposed = ir
        .lines()
        .map(|line| {
            if line.starts_with("@.amice.mba.seed") {
                "@test_seed = global i32 0, align 4".to_string()
            } else if line.trim_start().starts_with("ret i64 ") {
                format!("  ret i64 {carrier}")
            } else {
                line.replace("@.amice.mba.seed", "@test_seed")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let probe = dir.join("exposed.ll");
    std::fs::write(&probe, exposed).unwrap();
    let main = dir.join("main.c");
    std::fs::write(
        &main,
        r#"
#include <stdint.h>
#include <stdio.h>
#include <fenv.h>
extern volatile uint32_t test_seed;
extern uint64_t probe(uint32_t, uint32_t);
int main(void) {
  const int modes[]={FE_TONEAREST,FE_UPWARD,FE_DOWNWARD,FE_TOWARDZERO};
  const uint32_t lows[]={0,1,0x1fffe,0x1ffff};
  unsigned checks=0;
  for(unsigned m=0;m<4;++m) {
    if(fesetround(modes[m])) return 1;
    for(unsigned e=0;e<256;++e) for(unsigned l=0;l<4;++l) {
      test_seed=(e<<17)|lows[l]|0xfe000000u;
      uint64_t previous=0;
      for(uint32_t y=0;y<256;++y) {
        const uint32_t x=0xfffffff0u;
        feclearexcept(FE_ALL_EXCEPT);
        if(feraiseexcept(FE_DIVBYZERO)) return 2;
        uint64_t value=probe(x,y), header=value&~UINT64_C(0xffffffff);
        uint64_t k=(header>>33)&0x7ffff;
        if((uint32_t)value!=x+y || (header>>52)!=895+e || k<1 || k>0x20000 ||
           (y && header==previous) || fetestexcept(FE_ALL_EXCEPT)!=FE_DIVBYZERO ||
           fegetround()!=modes[m]) {
          fprintf(stderr,"carrier failure mode=%u exponent=%u low=%u y=%u\n",m,e,l,y);
          return 3;
        }
        previous=header;
        ++checks;
      }
    }
  }
  printf("%u live carrier checks passed\n",checks);
  return 0;
}
"#,
    )
    .unwrap();
    for pipeline in ["default<O0>", "default<O3>,lto<O2>"] {
        let optimized = dir.join(format!(
            "probe-{}.ll",
            if pipeline.contains("O3") { "o3" } else { "o0" }
        ));
        run(Command::new(tool("opt"))
            .args(["-verify-each", "-S"])
            .arg(format!("-passes={pipeline}"))
            .arg(&probe)
            .arg("-o")
            .arg(&optimized));
        let exe = optimized.with_extension(std::env::consts::EXE_EXTENSION);
        let mut cmd = Command::new(tool("clang"));
        cmd.args(["-O2", "-flto", "-fuse-ld=lld"])
            .args([&optimized, &main])
            .arg("-o")
            .arg(&exe);
        if !cfg!(windows) {
            cmd.arg("-lm");
        }
        run(&mut cmd);
        print!("{}", run(&mut Command::new(exe)));
    }
    // The full expanded loop must also survive optimization without a shared
    // floating literal. Integer masks and shift counts are expected constants.
    let input = dir.join("loop.ll");
    std::fs::write(&input, fixture_widths(&triple, [32]).0).unwrap();
    transform(&input, &output, true, 128, 8192);
    let optimized = dir.join("loop-o3.ll");
    run(Command::new(tool("opt"))
        .args(["-passes=default<O3>,lto<O2>", "-verify-each", "-S"])
        .arg(&output)
        .arg("-o")
        .arg(&optimized));
    for file in [&output, &optimized] {
        let ir = std::fs::read_to_string(file).unwrap();
        let body = body(&ir, "region_mutual_loop");
        assert!(body.contains("phi i64"));
        assert_eq!(body.matches("load volatile i32").count(), 1);
        let mut operations = 0;
        for line in body.lines() {
            if let Some((_, operands)) = line
                .split_once("fadd double ")
                .or_else(|| line.split_once("fsub double "))
            {
                let (a, b) = operands.split_once(',').unwrap();
                assert!(
                    a.trim().starts_with('%') && b.trim().starts_with('%'),
                    "constant FP operand: {line}"
                );
                operations += 1;
            }
        }
        assert!(operations > 4);
    }
}

#[test]
fn volatile_guard_invalidates_caller_contracts_and_obeys_annotations() {
    ensure_plugin_built();
    let dir = output_dir().join("mba-memory-contracts");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.ll");
    let output = dir.join("output.ll");
    let mut ir = String::from(
        r#"
target triple = "x86_64-unknown-linux-gnu"
@work_alias = alias i32 (i32,i32), ptr @work
define i32 @empty_phi() {
  %p = phi i32
  %a = and i32 %p, 0
  %b = add i32 %a, 42
  ret i32 %b
}
define i32 @work(i32 %x,i32 %y) #0 {
  %a = add i32 %x,%y
  ret i32 %a
}
define void @caller() #0 {
  %a = call i32 @work_alias(i32 3,i32 7) #1
  ret void
}
define void @outer() #0 {
  call void @caller() #1
  ret void
}
define void @indirect(ptr %f) #0 {
  %a = call i32 %f(i32 3,i32 7) #1
  ret void
}
attributes #0 = { noinline memory(none) nosync speculatable nounwind willreturn }
attributes #1 = { memory(none) nosync nounwind willreturn }
"#,
    );
    let annotations = [("no_guard", "-mba_opaque_guard"), ("no_expand", "-mba_pre_expand")];
    for (name, text) in annotations {
        ir += &format!(
            "@ann_{name} = private constant [{} x i8] c\"{text}\\00\"\ndefine i32 @{name}(i32 %x,i32 %y) {{\n%a = add i32 %x,%y\nret i32 %a\n}}\n",
            text.len() + 1
        );
    }
    ir += "@llvm.global.annotations = appending global [2 x {ptr,ptr,ptr,i32,ptr}] [\n";
    ir += &annotations
        .iter()
        .map(|(name, _)| format!("{{ptr,ptr,ptr,i32,ptr}} {{ptr @{name},ptr @ann_{name},ptr null,i32 0,ptr null}}"))
        .collect::<Vec<_>>()
        .join(",\n");
    ir += "]\n";
    std::fs::write(&input, ir).unwrap();
    transform(&input, &output, true, 128, 2048);
    let result = std::fs::read_to_string(&output).unwrap();
    for attr in ["memory(none)", "speculatable", "nosync"] {
        assert!(!result.contains(attr), "stale {attr}");
    }
    assert!(!body(&result, "no_guard").contains("load volatile"));
    assert!(body(&result, "empty_phi").contains("%p = phi i32"));
    let plain = body(&result, "no_expand");
    assert!(plain.contains("load volatile"));
    assert_eq!(
        plain.matches("fadd double").count() + plain.matches("fsub double").count(),
        2
    );
    let optimized = dir.join("o3.ll");
    run(Command::new(tool("opt"))
        .args(["-passes=default<O3>,lto<O2>", "-verify-each", "-S"])
        .arg(&output)
        .arg("-o")
        .arg(&optimized));
    let result = std::fs::read_to_string(optimized).unwrap();
    for name in ["caller", "outer", "indirect"] {
        assert!(body(&result, name).contains("call "), "{name} lost observable access");
    }
    // A non-transforming function must not gain a global or volatile access.
    for (limit, added) in [(0, 2048), (128, 0), (128, 12), (128, 35), (128, 107)] {
        let disabled = dir.join(format!("disabled-{limit}-{added}.ll"));
        // Force both options globally; function overrides can only reduce costs.
        let simple = dir.join("simple.ll");
        std::fs::write(
            &simple,
            "target triple = \"x86_64-linux-gnu\"\ndefine i32 @f(i32 %x,i32 %y) {\n%a = add i32 %x,%y\nret i32 %a\n}\n",
        )
        .unwrap();
        transform(&simple, &disabled, true, limit, added);
        let result = std::fs::read_to_string(disabled).unwrap();
        assert!(!result.contains("@.amice.mba.seed"));
        assert!(!result.contains("amice.mba.done"));
    }
}
