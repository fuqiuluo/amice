//! Region semantics, compiler survival, budgets, PHIs, poison and idempotence.
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
    let out = cmd.output().expect("start LLVM tool");
    assert!(
        out.status.success(),
        "{cmd:?}\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}
fn transform(input: &Path, output: &Path, regions: u32, size: u32, prob: u32, seed: u64, pipeline: &str) {
    transform_with_seed(input, output, regions, size, prob, Some(seed), pipeline);
}

fn transform_with_seed(
    input: &Path,
    output: &Path,
    regions: u32,
    size: u32,
    prob: u32,
    seed: Option<u64>,
    pipeline: &str,
) {
    let mut cmd = Command::new(tool("opt"));
    ObfuscationConfig::disabled().apply_to_command(&mut cmd);
    cmd.env_remove("AMICE_CONFIG_PATH")
        .env("AMICE_PASS_ORDER", "BogusControlFlow")
        .env("AMICE_BOGUS_CONTROL_FLOW", "true")
        .env("AMICE_BOGUS_CONTROL_FLOW_CLONE", "false")
        .env("AMICE_BOGUS_CONTROL_FLOW_PROB", prob.to_string())
        .env("AMICE_BOGUS_CONTROL_FLOW_MAX_REGIONS", regions.to_string())
        .env("AMICE_BOGUS_CONTROL_FLOW_MAX_REGION_INSTRUCTIONS", size.to_string())
        .env_remove("AMICE_BOGUS_CONTROL_FLOW_SEED")
        .env_remove("AMICE_BOGUS_CONTROL_FLOW_MODE")
        .env_remove("AMICE_BOGUS_CONTROL_FLOW_LOOPS")
        .arg(format!("--load-pass-plugin={}", plugin_path().display()))
        .arg(format!("-passes={pipeline}"))
        .args(["-verify-each", "-S"])
        .arg(input)
        .arg("-o")
        .arg(output);
    if let Some(seed) = seed {
        cmd.env("AMICE_BOGUS_CONTROL_FLOW_SEED", seed.to_string());
    }
    run(&mut cmd);
}

#[test]
fn bcf_default_seed_is_random_and_explicit_seed_is_reproducible() {
    ensure_plugin_built();
    let dir = output_dir().join("bcf-random-seed");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.ll");
    let output = dir.join("output.ll");
    std::fs::write(
        &input,
        "define i32 @f(i32 %x, i32 %y) { %a = add i32 %x, %y\n %b = xor i32 %a, %x\n ret i32 %b }",
    )
    .unwrap();
    let mut samples = Vec::new();
    for _ in 0..4 {
        transform_with_seed(&input, &output, 1, 8, 100, None, "default<O0>");
        let text = std::fs::read_to_string(&output).unwrap();
        assert!(text.contains("bcf.digits"));
        samples.push(text);
    }
    assert!(samples.windows(2).any(|pair| pair[0] != pair[1]));
    for seed in [0, 42, u64::MAX] {
        transform(&input, &output, 1, 8, 100, seed, "default<O0>");
        let first = std::fs::read_to_string(&output).unwrap();
        transform(&input, &output, 1, 8, 100, seed, "default<O0>");
        assert_eq!(first, std::fs::read_to_string(&output).unwrap());
    }
}
fn body<'a>(ir: &'a str, name: &str) -> &'a str {
    ir.split(&format!("@{name}("))
        .nth(1)
        .unwrap()
        .split("\n}")
        .next()
        .unwrap()
}

fn fixture() -> (String, Vec<(String, u32)>) {
    let mut ir = String::new();
    let mut names = Vec::new();
    for w in [8, 16, 32, 64] {
        for a in ["add", "sub", "and", "or", "xor"] {
            for b in ["add", "sub", "and", "or", "xor"] {
                let name = format!("region_{w}_{a}_{b}");
                names.push((name.clone(), w));
                ir += &format!("define i64 @{name}(i64 %xx, i64 %yy) noinline {{\n");
                let (x, y) = if w == 64 {
                    ("xx", "yy")
                } else {
                    ir += &format!("%x = trunc i64 %xx to i{w}\n%y = trunc i64 %yy to i{w}\n");
                    ("x", "y")
                };
                ir += &format!("%a = {a} i{w} %{x}, %{y}\n%b = {b} i{w} %a, %{x}\n");
                if w < 64 {
                    ir += &format!("%out = zext i{w} %b to i64\nret i64 %out\n}}\n");
                } else {
                    ir += "ret i64 %b\n}\n";
                }
            }
        }
    }
    ir += include_str!("c/fixtures/control_flow/bcf_regions.ll");
    for name in [
        "multiple_outputs",
        "multiple_phis",
        "masked_poison",
        "masked_undef",
        "masked_overflow",
        "branched_dag",
        "original_loop",
        "multi_region",
    ] {
        names.push((name.to_owned(), 64));
    }
    (ir, names)
}

fn driver(names: &[(String, u32)]) -> String {
    let mut c = String::from("#include <stdint.h>\n#include <stdio.h>\ntypedef uint64_t (*Fn)(uint64_t,uint64_t);\n");
    for (name, _) in names {
        c += &format!(
            "extern uint64_t {name}(uint64_t,uint64_t);\nextern uint64_t reference_{name}(uint64_t,uint64_t);\n"
        );
    }
    c += "struct Pair {Fn actual,expected; unsigned width; const char *name;};\nstatic struct Pair pairs[]={\n";
    for (name, w) in names {
        c += &format!("{{{name},reference_{name},{w},\"{name}\"}},\n");
    }
    c += r#"};
static uint64_t state=0xd1342543de82ef95ULL;
static uint64_t random_word(void){state^=state<<13;state^=state>>7;state^=state<<17;return state;}
int main(void){
 const uint64_t edge[]={0,1,2,15,16,127,128,255,256,65535,65536,0x7fffffffULL,0x80000000ULL,0xffffffffULL,0x8000000000000000ULL,~0ULL};
 unsigned long checks=0;
 for(unsigned p=0;p<sizeof(pairs)/sizeof(pairs[0]);p++){
  unsigned count=pairs[p].width==8?65536:4096;
  for(unsigned n=0;n<count;n++){
   uint64_t x=n<256?edge[n/16]:random_word(), y=n<256?edge[n%16]:random_word();
   if(pairs[p].width==8){x=n&255;y=n>>8;}
   uint64_t want=pairs[p].expected(x,y), got=pairs[p].actual(x,y);
   if(got!=want){fprintf(stderr,"%s x=%llx y=%llx got=%llx expected=%llx\n",pairs[p].name,(unsigned long long)x,(unsigned long long)y,(unsigned long long)got,(unsigned long long)want);return 1;}
   checks++;
  }
 }
 printf("PASS BCF %lu comparisons\n",checks);return 0;
}
"#;
    c
}

#[test]
fn bcf_region_differential_and_optimization() {
    ensure_plugin_built();
    let dir = output_dir().join("bcf-regions");
    std::fs::create_dir_all(&dir).unwrap();
    let (ir, names) = fixture();
    let input = dir.join("original.ll");
    let reference = dir.join("reference.ll");
    let main = dir.join("driver.c");
    std::fs::write(&input, &ir).unwrap();
    let mut ref_ir = ir.clone();
    for (name, _) in &names {
        ref_ir = ref_ir.replace(&format!("@{name}("), &format!("@reference_{name}("));
    }
    std::fs::write(&reference, ref_ir).unwrap();
    std::fs::write(&main, driver(&names)).unwrap();
    for seed in [0, 42, 0xffff_ffff] {
        let transformed = dir.join(format!("seed-{seed}.ll"));
        transform(&input, &transformed, 16, 8, 100, seed, "default<O0>");
        let text = std::fs::read_to_string(&transformed).unwrap();
        for w in [8, 16, 32, 64] {
            assert!(body(&text, &format!("region_{w}_add_sub")).contains("bcf.digits"));
        }
        assert!(!body(&text, "original_loop").contains("bcf.digits"));
        assert!(body(&text, "masked_poison").contains("freeze i64 poison"));
        assert!(body(&text, "multi_region").matches("bcf.position").count() > 1);
        assert!(!text.contains("load volatile") && !text.contains("unreachable"));
        let repeated = dir.join(format!("repeated-{seed}.ll"));
        transform(&transformed, &repeated, 16, 8, 100, seed, "default<O0>");
        assert_eq!(
            text.lines().skip(1).collect::<Vec<_>>(),
            std::fs::read_to_string(repeated)
                .unwrap()
                .lines()
                .skip(1)
                .collect::<Vec<_>>()
        );
        for level in ["O0", "O2", "O3"] {
            let optimized = dir.join(format!("seed-{seed}-{level}.ll"));
            run(Command::new(tool("opt"))
                .arg(format!("-passes=default<{level}>,default<{level}>"))
                .args(["-verify-each", "-S"])
                .arg(&transformed)
                .arg("-o")
                .arg(&optimized));
            let optimized_ir = std::fs::read_to_string(&optimized).unwrap();
            assert!(
                body(&optimized_ir, "region_32_add_xor").contains("br i1"),
                "region loop vanished after {level}"
            );
            let exe = dir.join(format!("check-{seed}-{level}{}", std::env::consts::EXE_SUFFIX));
            run(Command::new(tool("clang"))
                .arg(format!("-{level}"))
                .args([&optimized, &reference, &main])
                .arg("-o")
                .arg(&exe));
            print!("{}", run(&mut Command::new(exe)));
        }
    }
    for (regions, size, prob) in [(0, 8, 100), (2, 0, 100), (2, 1, 100), (2, 8, 0)] {
        let path = dir.join(format!("disabled-{regions}-{size}-{prob}.ll"));
        transform(&input, &path, regions, size, prob, 0, "default<O0>");
        assert!(!std::fs::read_to_string(path).unwrap().contains("bcf.digits"));
    }
    // Exercise the real late insertion point, not just O0 plus offline passes.
    for (name, pipeline) in [("late-O2", "default<O2>"), ("lto-O2", "lto<O2>")] {
        let late = dir.join(format!("{name}.ll"));
        transform(&input, &late, 2, 8, 100, 42, pipeline);
        assert!(body(&std::fs::read_to_string(&late).unwrap(), "region_32_add_xor").contains("bcf.digits"));
        let exe = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        run(Command::new(tool("clang"))
            .arg("-O2")
            .args([&late, &reference, &main])
            .arg("-o")
            .arg(&exe));
        print!("{}", run(&mut Command::new(exe)));
    }
}

fn loops(ir: &str) -> usize {
    ir.lines()
        .filter(|line| line.starts_with("bcf.digits") && line.contains(':'))
        .count()
}

#[test]
fn bcf_region_boundaries_budgets_and_annotations() {
    ensure_plugin_built();
    let dir = output_dir().join("bcf-boundaries");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.ll");
    let output = dir.join("output.ll");
    std::fs::write(&input, include_str!("c/fixtures/control_flow/bcf_boundaries.ll")).unwrap();
    transform(&input, &output, 100, 100, 100, 0, "default<O0>");
    assert_eq!(loops(&std::fs::read_to_string(&output).unwrap()), 0);

    let mut chain = String::from("define i64 @chain(i64 %x, i64 %y) {\n%r0 = add i64 %x, %y\n");
    for i in 1..40 {
        chain += &format!("%r{i} = xor i64 %r{}, %y\n", i - 1);
    }
    chain += "ret i64 %r39\n}\n";
    std::fs::write(&input, chain).unwrap();
    for (regions, size, count) in [(1, 2, 1), (100, 2, 16), (100, 100, 3)] {
        transform(&input, &output, regions, size, 100, 0, "default<O0>");
        assert_eq!(loops(&std::fs::read_to_string(&output).unwrap()), count);
    }
    transform(&input, &output, 1, 8, 100, 42, "default<O0>");
    let seeded = std::fs::read_to_string(&output).unwrap();
    transform(&input, &output, 1, 8, 100, 42, "default<O0>");
    assert_eq!(seeded, std::fs::read_to_string(&output).unwrap());
    transform(&input, &output, 1, 8, 100, 43, "default<O0>");
    assert_ne!(seeded, std::fs::read_to_string(&output).unwrap());

    let c = dir.join("annotations.c");
    std::fs::write(&c, r#"
#define FN(name, annotation) __attribute__((noinline,annotate(annotation))) unsigned name(unsigned x,unsigned y){return ((x+y)^x)+y;}
FN(disabled, "-bcf")
FN(zero, "+bcf bcf_max_regions=0")
FN(enabled, "+bcf bcf_prob=100 bcf_max_regions=1 bcf_seed=18446744073709551615")
FN(legacy, "+bcf bcf_prob=100 bcf_mode=basic bcf_loops=0")
"#).unwrap();
    run(Command::new(tool("clang"))
        .args(["-O1", "-S", "-emit-llvm"])
        .arg(&c)
        .arg("-o")
        .arg(&input));
    transform(&input, &output, 2, 8, 0, 0, "default<O0>");
    let text = std::fs::read_to_string(output).unwrap();
    assert_eq!(loops(body(&text, "disabled")), 0);
    assert_eq!(loops(body(&text, "zero")), 0);
    assert_eq!(loops(body(&text, "enabled")), 1);
    assert_eq!(loops(body(&text, "legacy")), 1);
}

#[test]
fn bcf_config_file_migration_is_diagnosed() {
    ensure_plugin_built();
    let dir = output_dir().join("bcf-config");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.ll");
    let output = dir.join("output.ll");
    let config = dir.join("config.json");
    std::fs::write(
        &input,
        "define i32 @f(i32 %x, i32 %y) { %a = add i32 %x, %y\n %b = xor i32 %a, %x\n ret i32 %b }",
    )
    .unwrap();
    for old in [false, true] {
        std::fs::write(
            &config,
            if old {
                r#"{"bogus_control_flow":{"enable":true,"mode":"basic"}}"#
            } else {
                r#"{"bogus_control_flow":{"enable":true,"probability":100,"max_regions":1}}"#
            },
        )
        .unwrap();
        let mut cmd = Command::new(tool("opt"));
        ObfuscationConfig::disabled().apply_to_command(&mut cmd);
        cmd.env_remove("AMICE_BOGUS_CONTROL_FLOW")
            .env_remove("AMICE_BOGUS_CONTROL_FLOW_MODE")
            .env_remove("AMICE_BOGUS_CONTROL_FLOW_LOOPS")
            .env_remove("AMICE_BOGUS_CONTROL_FLOW_PROB")
            .env_remove("AMICE_BOGUS_CONTROL_FLOW_MAX_REGIONS")
            .env_remove("AMICE_BOGUS_CONTROL_FLOW_MAX_REGION_INSTRUCTIONS")
            .env_remove("AMICE_BOGUS_CONTROL_FLOW_SEED")
            .env("AMICE_CONFIG_PATH", &config)
            .env("AMICE_BOGUS_CONTROL_FLOW_CLONE", "false")
            .env("AMICE_PASS_ORDER", "BogusControlFlow")
            .env("RUST_LOG", "error")
            .arg(format!("--load-pass-plugin={}", plugin_path().display()))
            .args(["-passes=default<O0>", "-verify-each", "-S"])
            .arg(&input)
            .arg("-o")
            .arg(&output);
        let result = cmd.output().unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        assert_eq!(loops(&std::fs::read_to_string(&output).unwrap()), usize::from(!old));
        if old {
            let errors = String::from_utf8_lossy(&result.stderr);
            assert!(
                errors.contains("unknown field `mode`") && errors.contains("falling back"),
                "{errors}"
            );
        }
    }
}

#[test]
fn bcf_composes_with_mba() {
    ensure_plugin_built();
    let dir = output_dir().join("bcf-mba");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.ll");
    let output = dir.join("output.ll");
    let reference = dir.join("reference.ll");
    let main = dir.join("driver.c");
    let ir = "define i64 @pair(i64 %x, i64 %y) noinline { %a = add i64 %x, %y\n %b = xor i64 %a, %x\n %c = sub i64 %b, %y\n ret i64 %c }";
    std::fs::write(&input, ir).unwrap();
    std::fs::write(&reference, ir.replace("@pair(", "@reference_pair(")).unwrap();
    std::fs::write(&main, driver(&[("pair".into(), 64)])).unwrap();
    let mut cmd = Command::new(tool("opt"));
    ObfuscationConfig::disabled().apply_to_command(&mut cmd);
    cmd.env_remove("AMICE_CONFIG_PATH")
        .env("AMICE_PASS_ORDER", "BogusControlFlow,Mba")
        .env("AMICE_BOGUS_CONTROL_FLOW", "true")
        .env("AMICE_BOGUS_CONTROL_FLOW_PROB", "100")
        .env("AMICE_BOGUS_CONTROL_FLOW_MAX_REGIONS", "1")
        .env("AMICE_BOGUS_CONTROL_FLOW_MAX_REGION_INSTRUCTIONS", "8")
        .env("AMICE_BOGUS_CONTROL_FLOW_SEED", "42")
        .env("AMICE_MBA", "true")
        .env("AMICE_MBA_MAX_INSTRUCTIONS", "16")
        .env("AMICE_MBA_MAX_ADDED_INSTRUCTIONS", "256")
        .arg(format!("--load-pass-plugin={}", plugin_path().display()))
        .args(["-passes=default<O0>", "-verify-each", "-S"])
        .arg(&input)
        .arg("-o")
        .arg(&output);
    run(&mut cmd);
    let text = std::fs::read_to_string(&output).unwrap();
    assert!(text.contains("bcf.digits") && text.contains("mba."));
    let exe = dir.join(format!("check{}", std::env::consts::EXE_SUFFIX));
    run(Command::new(tool("clang"))
        .arg("-O2")
        .args([&output, &reference, &main])
        .arg("-o")
        .arg(&exe));
    print!("{}", run(&mut Command::new(exe)));
}
