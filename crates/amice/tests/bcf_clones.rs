//! Whole-function cloning, forced inlining, false-path execution and preservation.
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

fn run(command: &mut Command) -> String {
    let output = command.output().expect("start test command");
    assert!(
        output.status.success(),
        "{command:?}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn transform(input: &Path, output: &Path, seed: Option<u64>, clone: bool) {
    let mut command = Command::new(tool("opt"));
    ObfuscationConfig::disabled().apply_to_command(&mut command);
    command
        .env_remove("AMICE_CONFIG_PATH")
        .env("AMICE_PASS_ORDER", "BogusControlFlow")
        .env("AMICE_BOGUS_CONTROL_FLOW", "true")
        .env("AMICE_BOGUS_CONTROL_FLOW_CLONE", clone.to_string())
        .env("AMICE_BOGUS_CONTROL_FLOW_PROB", "100")
        .env("AMICE_BOGUS_CONTROL_FLOW_MAX_REGIONS", "2")
        .env("AMICE_BOGUS_CONTROL_FLOW_MAX_REGION_INSTRUCTIONS", "8")
        .env_remove("AMICE_BOGUS_CONTROL_FLOW_SEED")
        .arg(format!("--load-pass-plugin={}", plugin_path().display()))
        .args(["-passes=default<O0>", "-verify-each", "-S"])
        .arg(input)
        .arg("-o")
        .arg(output);
    if let Some(seed) = seed {
        command.env("AMICE_BOGUS_CONTROL_FLOW_SEED", seed.to_string());
    }
    run(&mut command);
}

fn body<'a>(text: &'a str, name: &str) -> &'a str {
    text.split(&format!("@{name}("))
        .nth(1)
        .unwrap()
        .split("\n}")
        .next()
        .unwrap()
}

#[test]
fn whole_function_is_inlined_without_size_limit_and_cloning_is_idempotent() {
    ensure_plugin_built();
    let dir = output_dir().join("bcf-whole-clone");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("original.ll");
    let output = dir.join("cloned.ll");
    let mut source = String::from("declare void @record(i32, i32)\ndefine void @large(i32 %x) noinline {\n");
    for tag in 0..512 {
        source += &format!("call void @record(i32 {tag}, i32 %x)\n");
    }
    source += "ret void\n}\n";
    std::fs::write(&input, source).unwrap();
    transform(&input, &output, Some(42), true);
    let text = std::fs::read_to_string(&output).unwrap();
    assert!(text.contains("\"amice.bcf.clone\""));
    assert!(text.contains("bcf.condition"));
    assert!(!text.contains("@__amice_bcf_"), "temporary functions must not escape");
    let copied = body(&text, "large");
    assert_eq!(copied.matches("call void @record(").count(), 1024);
    for tag in 0..512 {
        assert_eq!(copied.matches(&format!("@record(i32 {tag},")).count(), 2);
    }
    transform(&input, &output, Some(42), true);
    assert_eq!(text, std::fs::read_to_string(&output).unwrap());
    let twice = dir.join("twice.ll");
    transform(&output, &twice, Some(42), true);
    let twice = std::fs::read_to_string(twice).unwrap();
    assert_eq!(body(&text, "large"), body(&twice, "large"));
    transform(&input, &output, Some(42), false);
    let text = std::fs::read_to_string(output).unwrap();
    assert_eq!(body(&text, "large").matches("call void @record(").count(), 512);
    assert!(!text.contains("bcf.condition"));
}

#[test]
fn unsupported_clones_retain_region_rewriting_without_temporary_definitions() {
    ensure_plugin_built();
    let dir = output_dir().join("bcf-clone-boundaries");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.ll");
    let output = dir.join("output.ll");
    let source = include_str!("c/fixtures/control_flow/bcf_boundaries.ll").to_owned()
        + r#"
declare void @unique_call() noduplicate
define i32 @varargs(i32 %x, ...) {
 %a = add i32 %x, 17
 %b = xor i32 %a, %x
 ret i32 %b
}
define i32 @no_duplicate(i32 %x) {
 call void @unique_call()
 %a = add i32 %x, 17
 %b = xor i32 %a, %x
 ret i32 %b
}
"#;
    std::fs::write(&input, source).unwrap();
    transform(&input, &output, Some(42), true);
    let text = std::fs::read_to_string(output).unwrap();
    assert!(!text.contains("@__amice_bcf_"));
    for name in ["eh_function", "address_taken", "varargs", "no_duplicate"] {
        assert!(!body(&text, name).contains("bcf.condition"), "{name}");
    }
    for name in ["varargs", "no_duplicate"] {
        assert!(body(&text, name).contains("bcf.digits"));
    }
    for name in ["wide", "vector", "disconnected", "store_boundary", "irreducible"] {
        assert!(body(&text, name).contains("bcf.condition"), "{name}");
    }
    assert_eq!(text.matches("call void @unique_call()").count(), 1);
}

#[test]
fn aliases_preserve_call_restrictions_and_unknown_signature_types_do_not_panic() {
    ensure_plugin_built();
    let dir = output_dir().join("bcf-clone-signatures");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.ll");
    let output = dir.join("output.ll");
    let mut source = String::new();
    for attribute in ["noduplicate", "convergent", "returns_twice"] {
        source += &format!(
            "@{attribute}_alias = alias void (), ptr @{attribute}_callee\n\
             define void @{attribute}_callee() {attribute} {{ ret void }}\n\
             define i32 @{attribute}_caller(i32 %x) {{\n\
             call void @{attribute}_alias()\n\
             %a = add i32 %x, 17\n%b = xor i32 %a, %x\nret i32 %b\n}}\n"
        );
    }
    if detect_llvm_config().is_some_and(|config| common::llvm_major_from_feature(&config.feature) >= 16) {
        let ty = "target(\"spirv.Image\", void, 1, 0, 0, 0, 0, 0, 0)";
        source += &format!(
            "define {ty} @extension_result({ty} %image) {{ ret {ty} %image }}\n\
             define i32 @extension_argument({ty} %image, i32 %x) {{\n\
             %a = add i32 %x, 17\n%b = xor i32 %a, %x\nret i32 %b\n}}\n"
        );
    }
    std::fs::write(&input, source).unwrap();
    transform(&input, &output, Some(42), true);
    let text = std::fs::read_to_string(output).unwrap();
    assert!(!text.contains("@__amice_bcf_"));
    assert!(!text.contains("bcf.condition"));
    for attribute in ["noduplicate", "convergent", "returns_twice"] {
        assert_eq!(text.matches(&format!("call void @{attribute}_alias()")).count(), 1);
        assert!(body(&text, &format!("{attribute}_caller")).contains("bcf.digits"));
    }
    if text.contains("@extension_argument") {
        assert!(body(&text, "extension_argument").contains("bcf.digits"));
    }
}

#[test]
fn entry_block_intrinsics_remain_in_the_entry_for_both_bcf_paths() {
    ensure_plugin_built();
    let dir = output_dir().join("bcf-entry-intrinsics");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.ll");
    let output = dir.join("output.ll");
    std::fs::write(
        &input,
        r#"
declare void @llvm.localescape(...)
declare void @llvm.gcroot(ptr, ptr)
define i32 @escaped(i32 %x) {
 %p = alloca i32
 %a = add i32 %x, 17
 %b = xor i32 %a, %x
 call void (...) @llvm.localescape(ptr %p)
 ret i32 %b
}
define i32 @rooted(i32 %x) gc "shadow-stack" {
 %p = alloca ptr
 %a = add i32 %x, 17
 %b = xor i32 %a, %x
 call void @llvm.gcroot(ptr %p, ptr null)
 ret i32 %b
}
"#,
    )
    .unwrap();
    for clone in [false, true] {
        transform(&input, &output, Some(42), clone);
        let text = std::fs::read_to_string(&output).unwrap();
        assert!(!text.contains("bcf."));
        assert_eq!(text.matches("call void (...) @llvm.localescape(").count(), 1);
        assert_eq!(text.matches("call void @llvm.gcroot(").count(), 1);
    }
}

#[test]
fn cloned_allocations_do_not_reserve_a_second_frame_on_the_real_path() {
    ensure_plugin_built();
    let dir = output_dir().join("bcf-clone-stack");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.ll");
    let output = dir.join("output.ll");
    std::fs::write(
        &input,
        r#"
declare void @observe(ptr)
define void @stack_copy() {
 %p = alloca [4096 x i8], align 16
 call void @observe(ptr %p)
 ret void
}
define i32 @byval_copy(ptr byval([4096 x i8]) %p) {
 call void @observe(ptr %p)
 ret i32 17
}
"#,
    )
    .unwrap();
    transform(&input, &output, Some(42), true);
    let text = std::fs::read_to_string(&output).unwrap();
    for name in ["stack_copy", "byval_copy"] {
        let function = body(&text, name);
        let entry = function.split("\n\n").next().unwrap();
        assert!(!entry.contains("alloca"), "fake allocations escaped into entry: {name}");
        let fake = function.split("bcf.fake:").nth(1).unwrap();
        assert!(fake.split("\n\n").next().unwrap().contains("alloca"), "{name}");
        assert_eq!(
            function.matches("call void @observe(").count(),
            2,
            "the complete copy must remain"
        );
    }
    if cfg!(target_os = "linux") {
        let fixture = common::tests_root().join("c/fixtures/control_flow/bcf_stack.c");
        for level in ["-O0", "-O2"] {
            let original = dir.join(format!("{level}.ll"));
            run(Command::new(tool("clang"))
                .args([level, "-S", "-emit-llvm"])
                .arg(&fixture)
                .arg("-o")
                .arg(&original));
            transform(&original, &output, Some(42), true);
            for postopt in ["-O0", "-O2"] {
                let binary = dir.join(format!("stack{level}{postopt}"));
                run(Command::new(tool("clang"))
                    .arg(postopt)
                    .arg(&output)
                    .arg("-o")
                    .arg(&binary));
                assert!(run(&mut Command::new(binary)).contains("PASS bounded stack"));
            }
        }
    }
}

#[test]
fn inline_assembly_is_not_duplicated_and_regions_still_apply() {
    ensure_plugin_built();
    let dir = output_dir().join("bcf-clone-inline-asm");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.ll");
    let output = dir.join("output.ll");
    std::fs::write(
        &input,
        r#"
define i32 @assembly_label(i32 %x) noinline {
 call void asm sideeffect "bcf_unique_label:", ""()
 %a = add i32 %x, 17
 %b = xor i32 %a, %x
 ret i32 %b
}
"#,
    )
    .unwrap();
    transform(&input, &output, Some(42), true);
    let text = std::fs::read_to_string(&output).unwrap();
    assert!(!text.contains("bcf.condition"));
    assert!(text.contains("bcf.digits"));
    assert_eq!(text.matches("bcf_unique_label:").count(), 1);
    // The IR verifier cannot detect duplicate assembler labels; run codegen too.
    run(Command::new(tool("clang"))
        .args(["-O0", "-c"])
        .arg(output)
        .arg("-o")
        .arg(dir.join("output.o")));
}

#[test]
fn matching_comdats_link_across_translation_units_with_different_seeds() {
    ensure_plugin_built();
    let dir = output_dir().join("bcf-matching-comdat");
    std::fs::create_dir_all(&dir).unwrap();
    for kind in ["exactmatch", "samesize"] {
        for clone in [false, true] {
            let mut objects = Vec::new();
            for seed in [0, 1] {
                let input = dir.join(format!("{kind}-{clone}-{seed}.ll"));
                let output = input.with_extension("bcf.ll");
                let object = input.with_extension("obj");
                std::fs::write(
                    &input,
                    format!(
                        r#"
target triple = "x86_64-pc-windows-msvc"
$f = comdat {kind}
define linkonce_odr i32 @f(i32 %x) comdat {{
 %a = add i32 %x, 17
 %b = xor i32 %a, %x
 ret i32 %b
}}
define i32 @caller{seed}(i32 %x) {{
 %r = call i32 @f(i32 %x)
 ret i32 %r
}}
"#
                    ),
                )
                .unwrap();
                transform(&input, &output, Some(seed), clone);
                let text = std::fs::read_to_string(&output).unwrap();
                assert!(!body(&text, "f").contains("bcf."));
                if cfg!(target_os = "linux") {
                    run(Command::new(tool("clang"))
                        .args(["--target=x86_64-pc-windows-msvc", "-O0", "-c"])
                        .arg(output)
                        .arg("-o")
                        .arg(&object));
                    objects.push(object);
                }
            }
            if cfg!(target_os = "linux") {
                run(Command::new(tool("lld-link"))
                    .args([
                        "/dll",
                        "/noentry",
                        "/nodefaultlib",
                        "/export:caller0",
                        "/export:caller1",
                    ])
                    .arg(format!("/out:{}", dir.join(format!("{kind}-{clone}.dll")).display()))
                    .args(objects));
            }
        }
    }
}

const SOURCE: &str = r#"
define i32 @mix(i32 %x, i32 %y) noinline {
 %a = add i32 %x, %y
 %b = xor i32 %a, %x
 %c = sub i32 %b, %y
 ret i32 %c
}
define i64 @wide(i64 %x, i64 %y) noinline {
 %a = add i64 %x, %y
 %b = xor i64 %a, %x
 %c = sub i64 %b, %y
 ret i64 %c
}
define void @memory(ptr %p, i32 %x) noinline {
 %a = add i32 %x, 923
 %b = xor i32 %a, %x
 store volatile i32 %b, ptr %p
 ret void
}
define i32 @stack(i32 %x) noinline {
 %slot = alloca i32
 store volatile i32 %x, ptr %slot
 %v = load volatile i32, ptr %slot
 %a = add i32 %v, 3
 %b = xor i32 %a, %x
 ret i32 %b
}
define i32 @loop(i32 %x) noinline {
 %n = and i32 %x, 15
 br label %again
again:
 %i = phi i32 [ 0, %0 ], [ %next, %again ]
 %v = phi i32 [ %x, %0 ], [ %a, %again ]
 %a = add i32 %v, %i
 %next = add i32 %i, 1
 %done = icmp eq i32 %i, %n
 br i1 %done, label %exit, label %again
exit:
 ret i32 %a
}
define ptr @pointer(ptr %p) noinline {
 ret ptr %p
}
define double @floating(double %x) noinline {
 %v = fadd double %x, 2.0
 ret double %v
}
define { i64, i64 } @aggregate(i64 %x) noinline {
 %a = insertvalue { i64, i64 } poison, i64 %x, 0
 %b = insertvalue { i64, i64 } %a, i64 17, 1
 ret { i64, i64 } %b
}
define i32 @recursive(i32 %x) noinline {
 %done = icmp eq i32 %x, 0
 br i1 %done, label %exit, label %recurse
recurse:
 %next = sub i32 %x, 1
 %r = call i32 @recursive(i32 %next)
 ret i32 %r
exit:
 ret i32 42
}
"#;

const NAMES: &[&str] = &[
    "mix",
    "wide",
    "memory",
    "stack",
    "loop",
    "pointer",
    "floating",
    "aggregate",
    "recursive",
];

const DRIVER: &str = r#"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
extern uint32_t mix(uint32_t,uint32_t), reference_mix(uint32_t,uint32_t);
extern uint64_t wide(uint64_t,uint64_t), reference_wide(uint64_t,uint64_t);
extern void memory(uint32_t*,uint32_t), reference_memory(uint32_t*,uint32_t);
extern uint32_t stack(uint32_t), reference_stack(uint32_t), loop(uint32_t), reference_loop(uint32_t);
extern void *pointer(void*), *reference_pointer(void*);
extern double floating(double), reference_floating(double);
typedef struct { uint64_t a, b; } Pair;
extern Pair aggregate(uint64_t), reference_aggregate(uint64_t);
extern uint32_t recursive(uint32_t), reference_recursive(uint32_t);
void unexpected_fake(void) { fputs("executed false BCF path\n", stderr); abort(); }
int main(void) {
 uint64_t state=0xd1342543de82ef95ULL;
 for(unsigned i=0;i<100000;++i) {
  state^=state<<13; state^=state>>7; state^=state<<17;
  uint64_t x=i<256?i:state, y=~state;
  if(mix(x,y)!=reference_mix(x,y) || wide(x,y)!=reference_wide(x,y)) return 1;
  uint32_t a=0,b=0; memory(&a,x); reference_memory(&b,x); if(a!=b) return 2;
  if(stack(x)!=reference_stack(x) || loop(x)!=reference_loop(x)) return 3;
  if(pointer(&a)!=&a || pointer(0)!=0) return 4;
  double z=(double)(int32_t)x; if(floating(z)!=reference_floating(z)) return 5;
  Pair p=aggregate(x),q=reference_aggregate(x); if(p.a!=q.a || p.b!=q.b) return 6;
  if(recursive(x&15)!=reference_recursive(x&15)) return 7;
 }
 puts("PASS BCF clones 100000 inputs; false paths unexecuted"); return 0;
}
"#;

#[test]
fn clones_preserve_results_and_false_paths_stay_unexecuted_after_optimization() {
    ensure_plugin_built();
    let dir = output_dir().join("bcf-clone-semantics");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("original.ll");
    let reference = dir.join("reference.ll");
    let driver = dir.join("driver.c");
    std::fs::write(&input, SOURCE).unwrap();
    let mut renamed = SOURCE.to_owned();
    for name in NAMES {
        renamed = renamed.replace(&format!("@{name}("), &format!("@reference_{name}("));
    }
    std::fs::write(&reference, renamed).unwrap();
    std::fs::write(&driver, DRIVER).unwrap();
    let mut samples = Vec::new();
    for (index, seed) in [Some(0), Some(42), Some(u64::MAX), None, None].into_iter().enumerate() {
        let transformed = dir.join(format!("seed-{index}.ll"));
        transform(&input, &transformed, seed, true);
        let text = std::fs::read_to_string(&transformed).unwrap();
        assert!(!text.contains("@__amice_bcf_"));
        for name in &NAMES[..8] {
            assert!(body(&text, name).contains("bcf.condition"), "{name} was not cloned");
        }
        assert!(!body(&text, "recursive").contains("bcf.condition"));
        assert!(body(&text, "mix").contains("bcf.argument"));
        assert!(
            body(&text, "mix")
                .lines()
                .filter(|line| line.starts_with("bcf.digits") && line.contains(':'))
                .count()
                >= 2,
            "original region and guard must both be present"
        );
        if seed.is_none() {
            samples.push(text.clone());
        }
        // Instrument the false entry before any cleanup. This catches an
        // incorrect predicate even when forwarded arguments give equal results.
        let instrumented = text
            .lines()
            .map(|line| {
                if line.starts_with("bcf.fake:") {
                    format!("{line}\n  call void @unexpected_fake()")
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\ndeclare void @unexpected_fake()\n";
        assert_eq!(instrumented.matches("call void @unexpected_fake()").count(), 8);
        let checked = dir.join(format!("checked-{index}.ll"));
        std::fs::write(&checked, instrumented).unwrap();
        for (level, pipeline) in [
            ("O2", "default<O2>"),
            ("O3", "default<O3>"),
            ("LTO", "default<O2>,lto<O2>,default<O2>"),
        ] {
            // Check survival without the probe: instrumentation must not be
            // what keeps the copied path alive during optimization.
            let uninstrumented = dir.join(format!("plain-{index}-{level}.ll"));
            run(Command::new(tool("opt"))
                .args(["-verify-each", "-S"])
                .arg(format!("-passes={pipeline}"))
                .arg(&transformed)
                .arg("-o")
                .arg(&uninstrumented));
            let plain = std::fs::read_to_string(uninstrumented).unwrap();
            for name in ["mix", "wide", "memory", "stack"] {
                assert!(
                    body(&plain, name).contains("bcf.fake"),
                    "copied {name} path was removed by {pipeline}"
                );
            }
            let optimized = dir.join(format!("{index}-{level}.ll"));
            run(Command::new(tool("opt"))
                .args(["-verify-each", "-S"])
                .arg(format!("-passes={pipeline}"))
                .arg(&checked)
                .arg("-o")
                .arg(&optimized));
            let exe = dir.join(format!("check-{index}-{level}{}", std::env::consts::EXE_SUFFIX));
            run(Command::new(tool("clang"))
                .arg("-O2")
                .arg(&optimized)
                .arg(&reference)
                .arg(&driver)
                .arg("-o")
                .arg(&exe));
            assert!(run(&mut Command::new(&exe)).contains("PASS BCF clones"));
        }
    }
    assert_ne!(samples[0], samples[1]);
}

#[test]
fn cloning_preserves_abi_debug_info_lifetimes_and_function_overrides() {
    ensure_plugin_built();
    let dir = output_dir().join("bcf-clone-abi");
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("original.c");
    let ir = dir.join("original.ll");
    let transformed = dir.join("cloned.ll");
    let reference = dir.join("reference.ll");
    let driver = dir.join("driver.c");
    std::fs::write(
        &source,
        r#"
typedef struct { unsigned long long a,b,c,d; } Large;
extern unsigned observe(unsigned*,unsigned);
__attribute__((noinline)) unsigned lifetime(unsigned x) {
 unsigned a[4]={x,x+1,x+2,x+3}; return observe(a,4);
}
__attribute__((noinline)) unsigned dynamic(unsigned x) {
 volatile unsigned a[(x&15)+1]; a[x&15]=x; return a[x&15]+1;
}
__attribute__((noinline)) Large aggregate_abi(Large x, unsigned y) {
 x.a+=y; x.b^=x.a; x.c-=x.b; x.d+=x.c; return x;
}
__attribute__((noinline)) int signed_arithmetic(int x) { return (x+17)^x; }
__attribute__((noinline,annotate("+bcf -bcf_clone"))) unsigned regions_only(unsigned x) {
 return (x+17)^x;
}
__attribute__((noinline,annotate("-bcf"))) unsigned disabled(unsigned x) { return (x+17)^x; }
"#,
    )
    .unwrap();
    run(Command::new(tool("clang"))
        .args(["-O1", "-g", "-S", "-emit-llvm"])
        .arg(&source)
        .arg("-o")
        .arg(&ir));
    transform(&ir, &transformed, Some(42), true);
    let text = std::fs::read_to_string(&transformed).unwrap();
    assert!(!text.contains("@__amice_bcf_"));
    for name in ["lifetime", "dynamic", "aggregate_abi", "signed_arithmetic"] {
        assert!(body(&text, name).contains("bcf.condition"), "{name}");
    }
    assert!(
        !body(&text, "signed_arithmetic").contains("bcf.argument"),
        "nsw preconditions must not be perturbed"
    );
    assert!(body(&text, "regions_only").contains("bcf.digits"));
    assert!(!body(&text, "regions_only").contains("bcf.condition"));
    assert!(!body(&text, "disabled").contains("bcf."));
    let mut original = std::fs::read_to_string(&ir).unwrap();
    for name in [
        "lifetime",
        "dynamic",
        "aggregate_abi",
        "signed_arithmetic",
        "regions_only",
        "disabled",
    ] {
        original = original.replace(&format!("@{name}"), &format!("@reference_{name}"));
    }
    std::fs::write(&reference, original).unwrap();
    std::fs::write(
        &driver,
        r#"
#include <stdint.h>
#include <stdio.h>
typedef struct { unsigned long long a,b,c,d; } Large;
extern unsigned lifetime(unsigned),reference_lifetime(unsigned),dynamic(unsigned),reference_dynamic(unsigned);
extern Large aggregate_abi(Large,unsigned),reference_aggregate_abi(Large,unsigned);
extern int signed_arithmetic(int),reference_signed_arithmetic(int);
extern unsigned regions_only(unsigned),reference_regions_only(unsigned),disabled(unsigned),reference_disabled(unsigned);
static unsigned observed;
unsigned observe(unsigned *p,unsigned n) { ++observed; unsigned x=0; for(unsigned i=0;i<n;++i)x=(x+p[i])^i; return x; }
int main(void) {
 for(unsigned i=0;i<10000;++i) {
  unsigned x=i*317; observed=0;
  if(lifetime(x)!=reference_lifetime(x) || observed!=2) return 1;
  if(dynamic(x)!=reference_dynamic(x)) return 2;
  Large v={x,~(uint64_t)x,x*13,x^357};
  Large a=aggregate_abi(v,i),b=reference_aggregate_abi(v,i);
  if(a.a!=b.a || a.b!=b.b || a.c!=b.c || a.d!=b.d) return 3;
  if(signed_arithmetic(i)!=reference_signed_arithmetic(i)) return 4;
  if(regions_only(x)!=reference_regions_only(x) || disabled(x)!=reference_disabled(x)) return 5;
 }
 puts("PASS BCF ABI 10000 inputs"); return 0;
}
"#,
    )
    .unwrap();
    let exe = dir.join(format!("abi{}", std::env::consts::EXE_SUFFIX));
    run(Command::new(tool("clang"))
        .arg("-O2")
        .arg(&transformed)
        .arg(&reference)
        .arg(&driver)
        .arg("-o")
        .arg(&exe));
    assert!(run(&mut Command::new(exe)).contains("PASS BCF ABI"));
}
