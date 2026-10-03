//! SSA preservation, generated index programs, optimization, and BCF ordering.
mod common;

use common::{ObfuscationConfig, build_amice, detect_llvm_config, output_dir, plugin_path};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::Once,
    thread,
    time::{Duration, Instant},
};

const SOURCE: &str = include_str!("c/fixtures/control_flow/vmf_distributed.ll");
const FUNCTIONS: &[&str] = &[
    "diamond",
    "swap_loop",
    "duplicate_switch",
    "duplicate_branch",
    "wide_switch",
    "aggregate_phi",
    "recursive",
];

fn ensure_plugin_built() {
    // `cargo test` can rebuild this harness without updating the cdylib
    // loaded by opt. Let Cargo check freshness once per test process.
    static BUILD: Once = Once::new();
    BUILD.call_once(build_amice);
    assert!(plugin_path().is_file(), "fresh plugin is missing");
}

#[derive(Debug)]
enum Execution {
    Completed(Output),
    TimedOut(Output),
}

fn execute_bounded(command: &mut Command, limit: Duration) -> Execution {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start executable");
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let deadline = Instant::now() + limit;
    thread::scope(|scope| {
        // Drain both pipes while waiting; a verbose failing fixture must not
        // block on a full stdout/stderr pipe before the deadline is checked.
        let stdout = scope.spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).expect("read stdout");
            bytes
        });
        let stderr = scope.spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).expect("read stderr");
            bytes
        });
        let (status, timed_out) = loop {
            if let Some(status) = child.try_wait().expect("poll executable") {
                break (status, false);
            }
            if Instant::now() >= deadline {
                child.kill().expect("kill timed-out executable");
                break (child.wait().expect("reap timed-out executable"), true);
            }
            thread::sleep(Duration::from_millis(10));
        };
        let output = Output {
            status,
            stdout: stdout.join().unwrap(),
            stderr: stderr.join().unwrap(),
        };
        if timed_out {
            Execution::TimedOut(output)
        } else {
            Execution::Completed(output)
        }
    })
}

#[test]
fn execution_fixture() {
    let Ok(mode) = std::env::var("AMICE_VMF_EXECUTION_FIXTURE") else {
        return;
    };
    std::io::stdout().write_all(&vec![b'o'; 131_072]).unwrap();
    std::io::stderr().write_all(&vec![b'e'; 131_072]).unwrap();
    std::io::stdout().flush().unwrap();
    std::io::stderr().flush().unwrap();
    if mode == "hang" {
        loop {
            thread::sleep(Duration::from_millis(100));
        }
    }
}

#[test]
fn execution_deadline_reaps_hung_program_and_preserves_output() {
    for mode in ["finish", "hang"] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "execution_fixture", "--nocapture"])
            .env("AMICE_VMF_EXECUTION_FIXTURE", mode);
        let outcome = execute_bounded(&mut command, Duration::from_secs(2));
        let output = match (mode, outcome) {
            ("finish", Execution::Completed(output)) => {
                assert!(output.status.success());
                output
            },
            ("hang", Execution::TimedOut(output)) => {
                assert!(!output.status.success());
                output
            },
            (_, outcome) => panic!("unexpected fixture outcome: {mode} {outcome:?}"),
        };
        assert!(output.stdout.len() >= 131_072);
        assert!(output.stderr.len() >= 131_072);
    }
}

fn tool(name: &str) -> PathBuf {
    let name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    detect_llvm_config()
        .map(|cfg| PathBuf::from(cfg.prefix).join("bin").join(&name))
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

fn command(input: &Path, output: &Path, seed: u64, max_ops: u32, pipeline: &str) -> Command {
    command_with_variants(input, output, seed, max_ops, pipeline, 3)
}

fn command_with_variants(
    input: &Path,
    output: &Path,
    seed: u64,
    max_ops: u32,
    pipeline: &str,
    program_variants: u32,
) -> Command {
    let mut command = Command::new(tool("opt"));
    ObfuscationConfig::disabled().apply_to_command(&mut command);
    command
        .env_remove("AMICE_CONFIG_PATH")
        .env_remove("AMICE_PASS_PRIORITY_OVERRIDE")
        .env("AMICE_PASS_ORDER", "VmFlatten")
        .env("AMICE_VM_FLATTEN", "true")
        .env("AMICE_VM_FLATTEN_DISTRIBUTED", "true")
        .env("AMICE_VM_FLATTEN_SEED", seed.to_string())
        .env("AMICE_VM_FLATTEN_MAX_OPS", max_ops.to_string())
        .env("AMICE_VM_FLATTEN_PROGRAM_VARIANTS", program_variants.to_string())
        .env("AMICE_BOGUS_CONTROL_FLOW_SEED", "42")
        .env("AMICE_BOGUS_CONTROL_FLOW_CLONE", "true")
        .env("AMICE_BOGUS_CONTROL_FLOW_PROB", "100")
        .env("AMICE_BOGUS_CONTROL_FLOW_MAX_REGIONS", "2")
        .env("AMICE_BOGUS_CONTROL_FLOW_MAX_REGION_INSTRUCTIONS", "8")
        .arg(format!("--load-pass-plugin={}", plugin_path().display()))
        .arg(format!("-passes={pipeline}"))
        .args(["-verify-each", "-S"])
        .arg(input)
        .arg("-o")
        .arg(output);
    command
}

fn fixture(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    fixture_from(dir, SOURCE, FUNCTIONS)
}

fn fixture_from(dir: &Path, source: &str, functions: &[&str]) -> (PathBuf, PathBuf, PathBuf) {
    std::fs::create_dir_all(dir).unwrap();
    let original = dir.join("original.ll");
    let reference = dir.join("reference.ll");
    let driver = dir.join("driver.c");
    std::fs::write(&original, source).unwrap();
    let mut renamed = source.to_owned();
    let mut c = String::from("#include <stdint.h>\n#include <stdio.h>\ntypedef uint64_t(*Fn)(uint64_t,uint64_t);\n");
    for name in functions {
        renamed = renamed.replace(&format!("@{name}("), &format!("@reference_{name}("));
        c += &format!("extern uint64_t {name}(uint64_t,uint64_t), reference_{name}(uint64_t,uint64_t);\n");
    }
    c += "struct Pair {Fn actual, expected; const char *name;};\nstatic struct Pair pairs[]={\n";
    for name in functions {
        c += &format!("{{{name},reference_{name},\"{name}\"}},\n");
    }
    c += r#"};
static uint64_t state=0xd1342543de82ef95ULL;
static uint64_t next_word(void){state^=state<<13;state^=state>>7;state^=state<<17;return state;}
int main(void){
 const uint64_t edge[]={0,1,2,3,15,16,31,32,127,255,256,65535,0x7fffffffULL,0x80000000ULL,0x8000000000000000ULL,~0ULL};
 unsigned checks=0;
 for(unsigned p=0;p<sizeof(pairs)/sizeof(pairs[0]);p++){
  for(unsigned n=0;n<4352;n++){
   uint64_t x=n<256?edge[n/16]:next_word(), y=n<256?edge[n%16]:next_word();
   uint64_t want=pairs[p].expected(x,y), got=pairs[p].actual(x,y);
   if(got!=want){fprintf(stderr,"%s x=%llx y=%llx got=%llx expected=%llx\n",pairs[p].name,(unsigned long long)x,(unsigned long long)y,(unsigned long long)got,(unsigned long long)want);return 1;}
   checks++;
  }
 }
 printf("PASS VMF %u comparisons\n",checks);return 0;
}
"#;
    std::fs::write(reference.clone(), renamed).unwrap();
    std::fs::write(driver.clone(), c).unwrap();
    (original, reference, driver)
}

fn check_execution(ir: &Path, reference: &Path, driver: &Path, level: &str) {
    let executable = ir.with_extension(std::env::consts::EXE_EXTENSION);
    run(Command::new(tool("clang"))
        .arg(format!("-{level}"))
        .args([ir, reference, driver])
        .arg("-o")
        .arg(&executable));
    let output = match execute_bounded(&mut Command::new(&executable), Duration::from_secs(120)) {
        Execution::Completed(output) => output,
        Execution::TimedOut(output) => panic!(
            "{} exceeded 120 seconds\n{}\n{}",
            executable.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    };
    assert!(
        output.status.success(),
        "{}: {}\n{}\n{}",
        executable.display(),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    print!("{}", String::from_utf8(output.stdout).unwrap());
}

fn body<'a>(ir: &'a str, name: &str) -> &'a str {
    let start = ir.find(&format!("@{name}(")).expect("function in IR");
    &ir[start..start + ir[start..].find("\n}").unwrap()]
}

fn without_printed_predecessors(ir: &str) -> Vec<&str> {
    // LLVM may reorder predecessor-use lists when parsing textual IR. They
    // only affect these printer comments, not the instruction operands.
    ir.lines()
        .skip(1)
        .map(|line| {
            if line.starts_with("source_filename = ") {
                "source_filename = <normalized>"
            } else {
                line.split_once("; preds =").map_or(line, |(code, _)| code.trim_end())
            }
        })
        .collect()
}

fn self_loop_blocks(ir: &str) -> usize {
    let lines: Vec<_> = ir.lines().collect();
    let mut count = 0;
    for (index, line) in lines.iter().enumerate() {
        let Some((label, _)) = line.trim().split_once(':') else {
            continue;
        };
        if !label.starts_with('v') {
            continue;
        }
        for next in lines.iter().skip(index + 1).take(16) {
            let next = next.trim();
            if next.starts_with("br ") {
                if next == format!("br label %{label}") {
                    count += 1;
                }
                break;
            }
        }
    }
    count
}

fn is_vm_dispatcher_line(line: &str) -> bool {
    line.contains("indirectbr")
        && line
            .split("label %")
            .nth(1)
            .and_then(|destination| destination.split([',', ']']).next())
            .is_some_and(|destination| destination.starts_with('v'))
}

fn indirect_destination_sets(ir: &str) -> Vec<BTreeSet<String>> {
    ir.lines()
        .filter(|line| is_vm_dispatcher_line(line))
        .filter_map(|line| line.split_once("[").map(|(_, destinations)| destinations))
        .filter(|destinations| destinations.contains("label %"))
        .map(|destinations| {
            destinations
                .split(']')
                .next()
                .unwrap_or_default()
                .split(',')
                .filter_map(|destination| destination.trim().strip_prefix("label %"))
                .map(str::to_owned)
                .collect()
        })
        .collect()
}

fn vm_dispatcher_lines(ir: &str) -> Vec<&str> {
    ir.lines().filter(|line| is_vm_dispatcher_line(line)).collect()
}

fn decoder_variant_groups(ir: &str) -> Vec<BTreeSet<u32>> {
    let definitions: HashMap<_, _> = ir.lines().filter_map(|line| line.trim().split_once(" = ")).collect();
    // Restrict comparisons to the backward slice of the actual jump target.
    // Gateway authentication has its own four-way codec selector, whose
    // result is stored in the token slot rather than used as a jump address.
    let mut pending: Vec<_> = ir
        .lines()
        .filter_map(|line| line.trim().strip_prefix("indirectbr ptr "))
        .filter_map(|rest| rest.split(',').next())
        .collect();
    let mut address_dependencies = HashSet::new();
    while let Some(value) = pending.pop() {
        if !address_dependencies.insert(value) {
            continue;
        }
        if let Some(expression) = definitions.get(value) {
            pending.extend(
                expression
                    .split_whitespace()
                    .map(|token| token.trim_matches(|character| matches!(character, ',' | '(' | ')')))
                    .filter(|token| definitions.contains_key(token)),
            );
        }
    }
    let mut groups: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    for line in ir.lines() {
        let Some((value, expression)) = line.trim().split_once(" = ") else {
            continue;
        };
        let Some((_, expression)) = expression.split_once("icmp eq") else {
            continue;
        };
        let mut fields = expression.split_whitespace();
        let Some(_type_name) = fields.next() else {
            continue;
        };
        let Some(operand) = fields.next().and_then(|value| value.strip_suffix(',')) else {
            continue;
        };
        let Some(constant) = fields.next().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        // Decoder-family comparisons select between SSA candidates. The
        // record-field permutation also compares the variant, but selects
        // integer constants; exclude that structurally similar shape.
        let selects_ssa_candidate = definitions.iter().any(|(result, candidate)| {
            address_dependencies.contains(result)
                && candidate.contains(&format!("select i1 {value},"))
                && candidate.contains("i64 %")
        });
        if operand.starts_with("%v") && (1..=3).contains(&constant) && selects_ssa_candidate {
            groups.entry(operand.to_owned()).or_default().insert(constant);
        }
    }
    groups.into_values().filter(|constants| constants.len() == 3).collect()
}

fn gateway_token_record_dependencies(ir: &str) -> usize {
    let lines: Vec<_> = ir.lines().collect();
    let definitions: HashMap<_, _> = lines
        .iter()
        .filter_map(|line| {
            let (value, expression) = line.trim().split_once(" = ")?;
            value.starts_with('%').then_some((value, expression))
        })
        .collect();
    let record_loads: HashSet<_> = definitions
        .iter()
        .filter_map(|(value, expression)| {
            if !expression.contains("load volatile i64") {
                return None;
            }
            let pointer = expression
                .split("ptr ")
                .nth(1)
                .and_then(|rest| {
                    rest.split(|character: char| character.is_whitespace() || character == ',')
                        .next()
                })
                .unwrap_or_default();
            definitions
                .get(pointer)
                .is_some_and(|gep| gep.contains("getelementptr inbounds [") && gep.contains(" x ["))
                .then_some(*value)
        })
        .collect();

    fn depends_on_record<'a>(
        root: &'a str,
        definitions: &HashMap<&'a str, &'a str>,
        record_loads: &HashSet<&'a str>,
    ) -> bool {
        let mut pending = vec![root];
        let mut seen = HashSet::new();
        while let Some(value) = pending.pop() {
            if !seen.insert(value) {
                continue;
            }
            if record_loads.contains(value) {
                return true;
            }
            let Some(expression) = definitions.get(value) else {
                continue;
            };
            for token in expression.split_whitespace() {
                let token = token.trim_matches(|character| matches!(character, ',' | ')' | '('));
                if token.starts_with('%') && definitions.contains_key(token) {
                    pending.push(token);
                }
            }
        }
        false
    }

    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| is_vm_dispatcher_line(line))
        .filter(|(index, _)| {
            let start = lines[..*index]
                .iter()
                .rposition(|line| line.trim_end().ends_with(':'))
                .unwrap_or(0);
            lines[start..*index].iter().any(|line| {
                let Some(rest) = line.trim().strip_prefix("store volatile i64 ") else {
                    return false;
                };
                let value = rest.split(',').next().unwrap_or_default().trim();
                depends_on_record(value, &definitions, &record_loads)
            })
        })
        .count()
}

fn state_timing_counts(ir: &str) -> (usize, usize) {
    let lines: Vec<_> = ir.lines().collect();
    let mut before_target = 0;
    let mut after_target = 0;
    let mut function_start = 0;
    while function_start < lines.len() {
        if !lines[function_start].starts_with("define ") {
            function_start += 1;
            continue;
        }
        let mut function_end = function_start + 1;
        while function_end < lines.len() && lines[function_end].trim() != "}" {
            function_end += 1;
        }
        let body = &lines[function_start..function_end.min(lines.len())];
        // The target table and the authenticated gateway scratch slot also
        // use pointer-width allocas in the entry block. Names, insertion order,
        // and raw store counts are therefore insufficient to identify VM state.
        // Select the candidate whose stores exhibit both lowering orders
        // relative to target reconstruction; key/anchor and scratch candidates
        // do not have that paired shape.
        let candidates: Vec<_> = body
            .iter()
            .filter_map(|line| line.find(" = alloca i64").map(|end| line[..end].trim().to_owned()))
            .collect();
        let mut selected_timing = None;
        for candidate in candidates {
            let state_marker = format!("ptr {candidate},");
            let stores: Vec<_> = body
                .iter()
                .enumerate()
                .filter_map(|(index, line)| {
                    (line.contains("store volatile i64") && line.contains(&state_marker)).then_some(index)
                })
                .collect();
            let targets: Vec<_> = body
                .iter()
                .enumerate()
                .filter_map(|(index, line)| {
                    // The continuation address vault is initialized in the
                    // entry block before any dispatcher. Those pointer
                    // reconstructions are unrelated to the transfer's
                    // before/after state timing and must not shift the pair.
                    let belongs_to_transfer = body[..index].iter().any(|candidate| is_vm_dispatcher_line(candidate));
                    (belongs_to_transfer && line.contains(" = inttoptr i64 ")).then_some(index)
                })
                .collect();
            let mut before = 0;
            let mut after = 0;
            for (target, store) in targets.iter().zip(stores.into_iter().skip(1)) {
                if store < *target {
                    before += 1;
                } else {
                    after += 1;
                }
            }
            let better = selected_timing.is_none_or(|(current_before, current_after)| {
                (before > 0 && after > 0, before + after)
                    > (current_before > 0 && current_after > 0, current_before + current_after)
            });
            if better {
                selected_timing = Some((before, after));
            }
        }
        if let Some((before, after)) = selected_timing {
            before_target += before;
            after_target += after;
        }
        function_start = function_end.saturating_add(1);
    }
    (before_target, after_target)
}

#[test]
fn distributed_preserves_ssa_and_execution_after_optimization() {
    ensure_plugin_built();
    let dir = output_dir().join("vmf-distributed");
    let (original, reference, driver) = fixture(&dir);
    let mut previous = None;
    for (seed, max_ops) in [(0, 8), (1, 8), (42, 1), (u64::MAX, 32)] {
        let transformed = dir.join(format!("seed-{seed}.ll"));
        run(&mut command(&original, &transformed, seed, max_ops, "default<O0>"));
        let text = std::fs::read_to_string(&transformed).unwrap();
        assert!(text.contains("indirectbr") && text.contains("load volatile"));
        assert!(!text.contains("vm_flatten_opcodes") && !text.contains(".vm_run"));
        assert!(!text.contains(".amice.vmf."));
        for original_label in ["entry:", "left:", "right:", "join:", "loop:", "exit:", "other:"] {
            assert!(
                !text.lines().any(|line| line.trim() == original_label),
                "semantic block label leaked: {original_label}"
            );
        }
        assert!(!text.contains("amice.vmf.done"));
        for marker in [
            "vmf.state",
            "vmf.hash",
            "vmf.target",
            "vmf.address",
            "vmf.sbox",
            "vmf.bytecode",
        ] {
            assert!(!text.contains(marker), "fixed VM marker leaked: {marker}");
        }
        assert!(!text.contains("vmf.witness"));
        let blockaddress_count = text.matches("blockaddress").count();
        assert!(
            blockaddress_count < 85,
            "target address materialization repeated too many blockaddresses: {blockaddress_count}"
        );
        let ptrtoint_count = text.matches(" = ptrtoint ptr ").count();
        let helper_target_conversions = text.matches("ptrtoint (ptr blockaddress").count();
        assert!(
            ptrtoint_count >= FUNCTIONS.len() && helper_target_conversions >= blockaddress_count,
            "expected frame nonce and helper target conversions: ptrtoint={ptrtoint_count}, helper_target_conversions={helper_target_conversions}, blockaddresses={blockaddress_count}, functions={}",
            FUNCTIONS.len()
        );
        let scalar_state_lanes = body(&text, "diamond")
            .lines()
            .filter(|line| line.contains(" = alloca i64"))
            .count();
        assert!(
            scalar_state_lanes >= 6,
            "expected three state lanes plus keyed gateway slots, found {scalar_state_lanes} scalar i64 allocas"
        );
        let (before_target, after_target) = state_timing_counts(&text);
        assert!(
            before_target + after_target > 0,
            "expected VM state timing lowering, got before={before_target} after={after_target}"
        );
        let indirect_lines = vm_dispatcher_lines(&text);
        if seed == 0 && max_ops == 8 {
            let per_operation_variant_selectors = text
                .lines()
                .filter(|line| line.contains(" = urem i32 ") && line.contains(", 3"))
                .count();
            assert!(
                per_operation_variant_selectors >= indirect_lines.len() * 2,
                "expected selectors to advance per operation in both inverse and forward paths: selectors={}, dispatchers={}",
                per_operation_variant_selectors,
                indirect_lines.len()
            );
        }
        let continuation_lines = text
            .lines()
            .filter(|line| line.contains("indirectbr") && !is_vm_dispatcher_line(line))
            .count();
        assert!(
            continuation_lines >= FUNCTIONS.len(),
            "expected continuation-local indirect successors: continuations={}, functions={}",
            continuation_lines,
            FUNCTIONS.len()
        );
        let runtime_choice_stores = text.lines().filter(|line| line.contains("store volatile i1 %")).count();
        assert!(
            runtime_choice_stores >= FUNCTIONS.len(),
            "expected state-derived continuation choices, stores={}, functions={}",
            runtime_choice_stores,
            FUNCTIONS.len()
        );
        let continuation_vault_decodes = text.lines().filter(|line| line.contains(" = inttoptr i64 ")).count();
        assert!(
            continuation_vault_decodes >= FUNCTIONS.len() * 2,
            "expected two runtime continuation target decodes per function, decodes={}, functions={}",
            continuation_vault_decodes,
            FUNCTIONS.len()
        );
        let pointer_vault_ops = text
            .lines()
            .filter(|line| line.contains("load volatile ptr") || line.contains("store volatile ptr"))
            .count();
        assert_eq!(
            pointer_vault_ops, 0,
            "continuation targets must use the sparse integer vault, pointer vault ops={pointer_vault_ops}"
        );
        let bucket_abi_a = "(i64 %0, ptr %1, i64 %2, ptr %3)";
        let bucket_abi_b = "(ptr %0, i64 %1, ptr %2, i64 %3)";
        let is_bucket_resolver = |line: &str| {
            line.starts_with("define private i64 @") && (line.contains(bucket_abi_a) || line.contains(bucket_abi_b))
        };
        let bucket_resolver_helpers = text.lines().filter(|line| is_bucket_resolver(line)).count();
        let bucket_abi_a_helpers = text.lines().filter(|line| line.contains(bucket_abi_a)).count();
        let bucket_abi_b_helpers = text.lines().filter(|line| line.contains(bucket_abi_b)).count();
        let direct_materializer_helpers = text
            .lines()
            .filter(|line| line.starts_with("define private i64 @") && line.contains("(i64 %0, ptr %1)"))
            .count();
        assert!(
            bucket_resolver_helpers > 0,
            "expected shared target bucket resolvers: helpers={}",
            bucket_resolver_helpers,
        );
        assert!(
            bucket_abi_a_helpers > 0 && bucket_abi_b_helpers > 0,
            "expected both bucket resolver ABI families: nonce-frame={}, frame-nonce={}",
            bucket_abi_a_helpers,
            bucket_abi_b_helpers,
        );
        assert_eq!(
            direct_materializer_helpers, 0,
            "one-address target materializer helpers leaked: helpers={direct_materializer_helpers}"
        );
        let mut bucket_target_counts = Vec::new();
        let mut in_bucket_resolver = false;
        let mut bucket_target_count = 0usize;
        for line in text.lines() {
            if line.starts_with("define private i64 @") {
                if in_bucket_resolver {
                    bucket_target_counts.push(bucket_target_count);
                }
                in_bucket_resolver = is_bucket_resolver(line);
                bucket_target_count = 0;
            }
            if in_bucket_resolver {
                bucket_target_count += line.matches("blockaddress(").count();
            }
            if in_bucket_resolver && line.trim() == "}" {
                bucket_target_counts.push(bucket_target_count);
                in_bucket_resolver = false;
            }
        }
        assert!(
            bucket_target_counts.iter().all(|count| *count == 0),
            "bucket resolvers must consume runtime bucket values, not direct blockaddresses: counts={bucket_target_counts:?}"
        );
        let mut selector_program_shapes = Vec::new();
        let mut in_selector_resolver = false;
        let mut has_selector_multiply = false;
        let mut has_selector_xorshift = false;
        for line in text.lines() {
            if line.starts_with("define private i64 @") {
                if in_selector_resolver {
                    selector_program_shapes.push((has_selector_multiply, has_selector_xorshift));
                }
                in_selector_resolver = is_bucket_resolver(line);
                has_selector_multiply = false;
                has_selector_xorshift = false;
            }
            if in_selector_resolver {
                has_selector_multiply |= line.contains(" mul i64 ");
                has_selector_xorshift |= line.contains(" lshr i64 ");
            }
            if in_selector_resolver && line.trim() == "}" {
                selector_program_shapes.push((has_selector_multiply, has_selector_xorshift));
                in_selector_resolver = false;
            }
        }
        assert!(
            selector_program_shapes
                .iter()
                .all(|(has_multiply, has_xorshift)| *has_multiply && *has_xorshift),
            "every bucket resolver must contain its per-bucket nonlinear selector program: shapes={selector_program_shapes:?}"
        );
        let lines: Vec<_> = text.lines().collect();
        let direct_selector_xors = lines
            .windows(2)
            .filter(|window| {
                let call = window[1];
                let previous = window[0];
                let bucket_call = call.contains("call i64 @") && call.contains(", ptr %") && call.contains(", i64 %");
                let rhs = previous.split(',').next_back().map(str::trim).unwrap_or_default();
                bucket_call && previous.contains("= xor i64 ") && !rhs.starts_with('%')
            })
            .count();
        assert_eq!(
            direct_selector_xors, 0,
            "bucket resolver calls must receive selector-vault data, direct selector constants={direct_selector_xors}"
        );
        let mut in_continuation_materializer = false;
        let mut direct_select_in_continuation = false;
        for line in text.lines() {
            if line.starts_with("define private i64 @") {
                in_continuation_materializer = line.contains(", ptr %2, ptr %3, ptr %4)");
            }
            if in_continuation_materializer && line.contains("select i1") && line.contains("ptrtoint (ptr blockaddress")
            {
                direct_select_in_continuation = true;
            }
            if in_continuation_materializer && line.trim() == "}" {
                in_continuation_materializer = false;
            }
        }
        assert!(
            !direct_select_in_continuation,
            "continuation materializers must select runtime vault pointers"
        );
        assert!(!text.contains("private constant [256 x i8]"));
        assert!(!text.contains("vmf.sbox."));
        assert!(text.contains("= mul i32 "));
        assert!(text.matches("= mul i32 ").count() >= text.matches("indirectbr").count());
        assert!(
            !text
                .lines()
                .any(|line| line.contains("store volatile i64 sub (i64 ptrtoint") && line.contains("blockaddress")),
            "raw blockaddress displacement leaked into a pool store"
        );
        assert!(
            text.lines()
                .any(|line| line.contains("= call i64 @v") || line.contains("call void @v")),
            "frame-bound target materialization helper calls disappeared"
        );
        assert!(
            text.lines().any(|line| line.contains("= inttoptr i64 ")),
            "decoded target address was not reconstructed"
        );
        assert!(
            !text
                .lines()
                .any(|line| line.contains("store volatile ptr") && line.contains("blockaddress")),
            "target pointer blockaddresses leaked into the VM body"
        );
        assert!(
            text.lines().any(|line| line.contains("= sub i64 %")),
            "frame-bound target envelope was not removed"
        );
        assert!(
            !text
                .lines()
                .any(|line| line.contains("= sub i64 ptrtoint (ptr blockaddress")),
            "blockaddress was reintroduced directly into displacement arithmetic"
        );
        let original_blockaddresses: usize = FUNCTIONS
            .iter()
            .map(|name| body(&text, name).matches("blockaddress").count())
            .sum();
        assert_eq!(
            original_blockaddresses, 0,
            "target addresses must stay in materialization helpers, not VM function bodies"
        );
        // Each transformed function contributes at least one three-block
        // decoy island. Its entry is present in its transfer row's indirectbr
        // destination list, while the valid decoder never emits its address.
        assert!(
            text.matches("unreachable").count() >= FUNCTIONS.len() * 2,
            "expected decoy island terminators"
        );
        assert!(!indirect_lines.is_empty());
        let target_pool_allocas = text
            .lines()
            .filter(|line| line.contains("= alloca [") && line.contains(" x i64], align"))
            .count();
        assert!(
            target_pool_allocas > indirect_lines.len(),
            "expected row-local target pools to be split into shards: pools={}, dispatchers={}",
            target_pool_allocas,
            indirect_lines.len()
        );
        let bytecode_state_bindings = text
            .lines()
            .filter(|line| line.contains(" = trunc i64 ") && line.contains(" to i32"))
            .count();
        assert!(
            bytecode_state_bindings >= indirect_lines.len(),
            "expected invocation/state-bound bytecode masks: state_bindings={}, dispatchers={}",
            bytecode_state_bindings,
            indirect_lines.len()
        );
        let state_selected_bytecode_envelopes = text
            .lines()
            .filter(|line| line.contains(" = and i32 %") && line.contains(", 3"))
            .count();
        assert!(
            state_selected_bytecode_envelopes >= indirect_lines.len(),
            "expected state-selected bytecode envelopes: selectors={}, dispatchers={}",
            state_selected_bytecode_envelopes,
            indirect_lines.len()
        );
        let invocation_bound_opcode_selectors = text
            .lines()
            .filter(|line| line.contains(" = and i32 %") && line.contains(", 3"))
            .count();
        assert!(
            invocation_bound_opcode_selectors >= indirect_lines.len(),
            "expected invocation-bound three-way opcode selectors: selectors={}, dispatchers={}",
            invocation_bound_opcode_selectors,
            indirect_lines.len()
        );
        let mixed_opcode_selectors = text
            .lines()
            .filter_map(|line| {
                let source = line
                    .strip_prefix("  %")?
                    .split_once(" = and i32 %")?
                    .1
                    .split_once(", 3")?
                    .0;
                Some(text.lines().any(|definition| {
                    let Some(rest) = definition.trim_start().strip_prefix('%') else {
                        return false;
                    };
                    let Some((name, rhs)) = rest.split_once(" = ") else {
                        return false;
                    };
                    name == source
                        && ["add i32 ", "xor i32 ", "mul i32 ", "or i32 "]
                            .iter()
                            .any(|opcode| rhs.starts_with(opcode))
                }))
            })
            .filter(|mixed| *mixed)
            .count();
        assert!(
            mixed_opcode_selectors >= indirect_lines.len(),
            "expected three-way opcode selectors to mix the frame envelope with state: mixed={}, dispatchers={}",
            mixed_opcode_selectors,
            indirect_lines.len()
        );
        let third_opcode_family = text
            .lines()
            .filter(|line| line.contains(" = icmp eq i32 %") && line.contains(", 2"))
            .count();
        assert!(
            third_opcode_family >= indirect_lines.len(),
            "expected third opcode decoder family: comparisons={}, dispatchers={}",
            third_opcode_family,
            indirect_lines.len()
        );
        let runtime_keyed_opcode_constants = text
            .lines()
            .filter(|line| {
                let Some(rhs) = line.split_once("= xor i32 ").map(|(_, rhs)| rhs.trim_start()) else {
                    return false;
                };
                // The keyed networks materialize constants as `const XOR
                // runtime_key`; the old static form had the value first.
                rhs.as_bytes()
                    .first()
                    .is_some_and(|byte| *byte == b'-' || byte.is_ascii_digit())
                    && rhs.contains(", %")
            })
            .count();
        assert!(
            runtime_keyed_opcode_constants >= indirect_lines.len(),
            "expected runtime-keyed opcode arithmetic: keyed_constants={}, dispatchers={}",
            runtime_keyed_opcode_constants,
            indirect_lines.len()
        );
        let runtime_ordinal_selects = text
            .lines()
            .filter(|line| {
                line.contains("select i1") && (line.contains("i32 0, i32 1") || line.contains("i32 1, i32 0"))
            })
            .count();
        assert!(
            runtime_ordinal_selects > 0,
            "expected source paths to select plain successor ordinals before inverse encoding"
        );
        let dynamic_record_fields = text
            .lines()
            .filter(|line| {
                line.contains("getelementptr inbounds [") && line.contains(" x [") && line.matches("i64 %").count() >= 2
            })
            .count();
        assert!(
            dynamic_record_fields >= indirect_lines.len(),
            "expected runtime-selected physical record fields: dynamic_geps={}, dispatchers={}",
            dynamic_record_fields,
            indirect_lines.len()
        );
        let permutation_moduli = text
            .lines()
            .filter(|line| {
                line.contains(" = urem ")
                    && line.contains('%')
                    && [", 4", ", 3", ", 2"].iter().any(|modulus| line.contains(modulus))
            })
            .count();
        assert!(
            permutation_moduli >= indirect_lines.len() * 3,
            "expected branch-free record permutation stages: urem_ops={}, dispatchers={}",
            permutation_moduli,
            indirect_lines.len()
        );
        let permutation_selects = text.lines().filter(|line| line.contains(" = select i1 %")).count();
        assert!(
            permutation_selects >= indirect_lines.len() * 6,
            "expected branch-free record swaps: select_ops={}, dispatchers={}",
            permutation_selects,
            indirect_lines.len(),
        );
        let decoder_groups = decoder_variant_groups(&text);
        assert!(
            !decoder_groups.is_empty() && decoder_groups.len() <= indirect_lines.len() * 2,
            "expected bounded decoder-envelope groups in multi-row output: variant-select groups={}, dispatchers={}",
            decoder_groups.len(),
            indirect_lines.len()
        );
        let alias_normalizations = text
            .lines()
            .filter(|line| line.contains(" = udiv i32 %") && (line.contains(", 2") || line.contains(", 3")))
            .count();
        assert!(
            alias_normalizations > 0 && alias_normalizations <= indirect_lines.len(),
            "multi-edge rows must normalize bounded physical alias slots: normalizations={}, dispatchers={}",
            alias_normalizations,
            indirect_lines.len()
        );
        assert_eq!(
            gateway_token_record_dependencies(&text),
            indirect_lines.len(),
            "each dispatcher token must depend on its row record lookup"
        );
        // A module-wide intersection is always empty across unrelated
        // functions and would miss a shared decoy universe in one function.
        // Each row owns its gateways and decoys, so compare rows locally.
        for name in FUNCTIONS {
            let destination_sets = indirect_destination_sets(body(&text, name));
            for (index, left) in destination_sets.iter().enumerate() {
                for right in destination_sets.iter().skip(index + 1) {
                    assert!(
                        left.is_disjoint(right),
                        "{name}: row-local destinations overlap: {:?}",
                        left.intersection(right).collect::<Vec<_>>()
                    );
                }
            }
        }

        // Every transfer row owns a separate one-dimensional sparse target
        // pool. Nested record allocas are deliberately excluded here; this
        // check protects the row-domain split from regressing to one shared
        // function-wide pool while keeping the LLVM destination list intact.
        let row_domain_pools = text
            .lines()
            .filter(|line| line.contains(" = alloca [") && line.contains(" x i64]") && line.matches('[').count() == 1)
            .count();
        assert!(
            row_domain_pools >= indirect_lines.len(),
            "expected one sparse target domain per dispatcher row: pools={row_domain_pools}, indirectbr={}",
            indirect_lines.len()
        );
        let conditional_branches = text
            .lines()
            .filter(|line| line.trim_start().starts_with("br i1 "))
            .count();
        assert!(
            conditional_branches >= indirect_lines.len(),
            "gateway authentication branches disappeared: conditional={conditional_branches}, indirectbr={}",
            indirect_lines.len()
        );
        let semantic_continuations = text
            .lines()
            .filter(|line| line.contains("call void (ptr, ptr, ...) @v"))
            .count();
        assert!(
            semantic_continuations >= FUNCTIONS.len() * 2,
            "expected private semantic continuation calls after authenticated gateways, found {semantic_continuations}"
        );
        let stateful_helper_stores = text
            .lines()
            .filter(|line| line.contains("store volatile") && line.contains("ptr %1"))
            .count();
        assert!(
            stateful_helper_stores >= FUNCTIONS.len(),
            "continuation helpers must feed the next VM state lane, found {stateful_helper_stores} state-pointer stores"
        );
        let live_rejection_sinks = self_loop_blocks(&text);
        assert!(
            live_rejection_sinks >= indirect_lines.len(),
            "expected live authentication rejection sinks, found {live_rejection_sinks} for {} dispatchers",
            indirect_lines.len()
        );
        assert!(
            indirect_lines.iter().all(|line| line.contains("label %v")),
            "every VM destination set should include an opaque decoy island"
        );
        for original_label in ["%left", "%right", "%loop", "%exit", "%join", "%other", "%zero", "%ones"] {
            assert!(
                indirect_lines
                    .iter()
                    .all(|line| !line.contains(&format!("label {original_label}"))),
                "raw original destination leaked into indirectbr: {original_label}"
            );
        }
        // The distributed state transition consumes existing SSA values. The
        // fixture deliberately provides both a branch predicate and an i128
        // switch condition, covering widening and narrowing of witnesses to
        // the target pointer width.
        assert!(text.contains("= zext i1 "));
        assert!(
            text.lines()
                .any(|line| line.contains("= trunc i128 ") && line.contains(" to i64"))
        );
        // At least one loop transfer has several pre-existing integer SSA
        // values. Their frozen forms prove that state coupling is not limited
        // to one easily isolated witness per transfer.
        let swap_loop_freezes = body(&text, "swap_loop")
            .lines()
            .filter(|line| line.contains(" = freeze i64 "))
            .count();
        assert!(
            swap_loop_freezes >= 2,
            "expected multiple integer SSA witnesses in swap_loop, found {swap_loop_freezes}"
        );
        let record_allocas = text
            .lines()
            .filter(|line| line.contains("alloca [") && line.contains(" x ["))
            .collect::<Vec<_>>();
        assert!(!record_allocas.is_empty(), "expected invocation-local VM records");
        assert!(
            record_allocas.iter().any(|line| {
                ["[5 x i", "[6 x i", "[7 x i", "[8 x i"]
                    .iter()
                    .any(|width| line.contains(width))
            }),
            "record arrays must reserve the invocation-dependent displacement spill slot"
        );
        assert!(
            record_allocas.iter().all(|line| !line.contains("[3 x i")),
            "fixed three-word VM records leaked"
        );
        assert!(
            !text
                .lines()
                .any(|line| line.contains("private constant [") && line.contains("[3 x i")),
            "static VM record table leaked"
        );
        let permutation_caches = text.lines().filter(|line| line.contains("alloca [4 x [4 x i")).count();
        assert!(
            permutation_caches >= indirect_lines.len(),
            "expected one invocation-local permutation cache per row: caches={}, dispatchers={}",
            permutation_caches,
            indirect_lines.len()
        );
        assert!(
            !text
                .lines()
                .any(|line| { line.contains("private constant [") && line.contains("blockaddress") })
        );
        assert!(text.contains("blockaddress"));
        assert!(
            text.lines().any(|line| line.starts_with("define private void @")),
            "expected private bucket initializers to own blockaddress materialization"
        );
        assert!(
            !text
                .lines()
                .any(|line| line.contains("inttoptr") && line.contains("blockaddress"))
        );
        assert_eq!(text.matches(" = phi ").count(), SOURCE.matches(" = phi ").count());
        assert!(!text.contains("reg2mem"));
        for name in FUNCTIONS {
            assert!(body(&text, name).contains("indirectbr"), "{name}");
        }
        let repeated = dir.join(format!("same-seed-{seed}.ll"));
        run(&mut command(&original, &repeated, seed, max_ops, "default<O0>"));
        assert_eq!(text, std::fs::read_to_string(&repeated).unwrap());
        let reapplied = dir.join(format!("reapplied-{seed}.ll"));
        run(&mut command(&transformed, &reapplied, seed, max_ops, "default<O0>"));
        assert_eq!(
            without_printed_predecessors(&text),
            without_printed_predecessors(&std::fs::read_to_string(reapplied).unwrap())
        );
        if let Some(previous) = previous.replace(text) {
            assert_ne!(previous, std::fs::read_to_string(&transformed).unwrap());
        }
        for level in ["O0", "O2", "O3"] {
            let optimized = dir.join(format!("seed-{seed}-{level}.ll"));
            run(Command::new(tool("opt"))
                .arg(format!("-passes=default<{level}>"))
                .args(["-verify-each", "-S"])
                .arg(&transformed)
                .arg("-o")
                .arg(&optimized));
            let text = std::fs::read_to_string(&optimized).unwrap();
            assert!(body(&text, "swap_loop").contains("indirectbr"));
            assert!(body(&text, "swap_loop").contains("load volatile"));
            let optimized_blockaddresses: usize = FUNCTIONS
                .iter()
                .map(|name| body(&text, name).matches("blockaddress").count())
                .sum();
            assert_eq!(
                optimized_blockaddresses, 0,
                "optimized VM function bodies must not regain direct blockaddresses at {level}"
            );
            let optimized_indirect_lines = text
                .lines()
                .filter(|line| is_vm_dispatcher_line(line))
                .collect::<Vec<_>>();
            let optimized_continuation_lines = text
                .lines()
                .filter(|line| line.contains("indirectbr") && !is_vm_dispatcher_line(line))
                .count();
            assert!(
                optimized_continuation_lines >= continuation_lines,
                "optimized VM lost continuation-local indirect successors at {level}: before={}, after={}",
                continuation_lines,
                optimized_continuation_lines
            );
            for original_label in ["%left", "%right", "%loop", "%exit", "%join", "%other", "%zero", "%ones"] {
                assert!(
                    optimized_indirect_lines
                        .iter()
                        .all(|line| !line.contains(&format!("label {original_label}"))),
                    "optimized raw original destination leaked into indirectbr: {original_label}"
                );
            }
            assert!(
                !text
                    .lines()
                    .any(|line| line.contains("private constant [") && line.contains("[3 x i")),
                "optimization recreated a static VM record table at {level}"
            );
            check_execution(&optimized, &reference, &driver, level);
        }
    }
}

#[test]
fn program_variant_budgets_preserve_semantics() {
    ensure_plugin_built();
    let source = include_str!("c/fixtures/control_flow/vmf_distributed.ll");
    let source = format!(
        "{}\n}}\n",
        source
            .split_once("\n}\n")
            .map(|(function, _)| function)
            .expect("diamond fixture function")
    );
    let functions = ["diamond"];
    let dir = output_dir().join("vmf-program-variant-budgets");
    let (original, reference, driver) = fixture_from(&dir, &source, &functions);
    for variants in 1..=3 {
        let transformed = dir.join(format!("variants-{variants}.ll"));
        run(&mut command_with_variants(
            &original,
            &transformed,
            7,
            4,
            "default<O0>",
            variants,
        ));
        let text = std::fs::read_to_string(&transformed).unwrap();
        let transformed_body = body(&text, "diamond");
        assert!(
            transformed_body.contains("indirectbr"),
            "variant budget {variants} did not lower the function"
        );
        let optimized = dir.join(format!("variants-{variants}-O2.ll"));
        run(Command::new(tool("opt"))
            .args(["-passes=default<O2>", "-verify-each", "-S"])
            .arg(&transformed)
            .arg("-o")
            .arg(&optimized));
        check_execution(&optimized, &reference, &driver, "O2");
    }
}

#[test]
fn phi_transport_preserves_widths_poison_and_noninteger_values() {
    ensure_plugin_built();
    let source = include_str!("c/fixtures/control_flow/vmf_phi_transport.ll");
    let functions = ["phi_widths", "phi_poison_masked", "phi_unreachable", "phi_noninteger"];
    let dir = output_dir().join("vmf-phi-transport");
    let (original, reference, driver) = fixture_from(&dir, source, &functions);
    for seed in [0, 1, 42, u64::MAX] {
        let transformed = dir.join(format!("seed-{seed}.ll"));
        run(&mut command(&original, &transformed, seed, 8, "default<O0>"));
        let text = std::fs::read_to_string(&transformed).unwrap();
        for name in functions {
            assert!(body(&text, name).contains("indirectbr"), "VMF skipped {name}");
        }
        assert_eq!(text.matches(" = phi ").count(), source.matches(" = phi ").count());
        // The integer edge values must now be produced in the gateway, while
        // pointer/vector/FP PHIs retain their original representation.
        for width in [1, 8, 17, 65, 128] {
            let line = body(&text, "phi_widths")
                .lines()
                .find(|line| line.contains(&format!(" = phi i{width} ")))
                .expect("original integer PHI retained");
            assert!(
                !line.contains("[ %a") && !line.contains("[ %b"),
                "unencoded PHI edge: {line}"
            );
        }
        for kind in ["ptr", "<2 x i64>", "double"] {
            assert!(body(&text, "phi_noninteger").contains(&format!(" = phi {kind} ")));
        }
        for level in ["O0", "O2", "O3"] {
            let optimized = dir.join(format!("seed-{seed}-{level}.ll"));
            run(Command::new(tool("opt"))
                .arg(format!("-passes=default<{level}>"))
                .args(["-verify-each", "-S"])
                .arg(&transformed)
                .arg("-o")
                .arg(&optimized));
            check_execution(&optimized, &reference, &driver, level);
        }
    }
    // Compile the additional odd/wide integer and pointer PHIs at both word
    // widths as well; host execution alone cannot exercise truncation to i32.
    for (triple, layout) in [
        ("i686-pc-linux-gnu", "e-p:32:32-i64:64-n8:16:32-S128"),
        ("aarch64-linux-android", "e-m:e-i64:64-i128:128-n32:64-S128"),
    ] {
        let input = dir.join(format!("{triple}-input.ll"));
        std::fs::write(&input, format!("target datalayout = \"{layout}\"\n{source}")).unwrap();
        let transformed = dir.join(format!("{triple}.ll"));
        run(&mut command(&input, &transformed, 42, 8, "default<O0>"));
        run(Command::new(tool("llc"))
            .args(["-O3", "-filetype=obj"])
            .arg(format!("-mtriple={triple}"))
            .arg(&transformed)
            .arg("-o")
            .arg(transformed.with_extension("o")));
    }
}

#[test]
fn encoded_targets_survive_codegen_on_32_and_64_bit_targets() {
    ensure_plugin_built();
    let dir = output_dir().join("vmf-codegen");
    std::fs::create_dir_all(&dir).unwrap();
    for (triple, layout, bits) in [
        ("x86_64-pc-windows-gnu", "e-p:64:64", 64),
        ("i686-pc-linux-gnu", "e-p:32:32", 32),
        ("aarch64-linux-android", "e-p:64:64", 64),
        ("armv7-linux-androideabi", "e-p:32:32", 32),
    ] {
        let original = dir.join(format!("{triple}-input.ll"));
        std::fs::write(
            &original,
            format!("target datalayout = \"{layout}\"\ntarget triple = \"{triple}\"\n{SOURCE}"),
        )
        .unwrap();
        let transformed = dir.join(format!("{triple}.ll"));
        run(&mut command(&original, &transformed, 42, 8, "default<O0>"));
        let ir = std::fs::read_to_string(&transformed).unwrap();
        assert!(ir.contains(&format!("alloca i{bits}")));
        assert!(ir.contains(&format!("inttoptr i{bits}")));
        let optimized = dir.join(format!("{triple}-O3.ll"));
        run(Command::new(tool("opt"))
            .args(["-passes=default<O3>", "-verify-each", "-S"])
            .arg(&transformed)
            .arg("-o")
            .arg(&optimized));
        let asm = dir.join(format!("{triple}.s"));
        let mut llc = Command::new(tool("llc"));
        llc.arg("-O3").arg(&optimized).arg("-o").arg(&asm);
        if triple.starts_with("x86") || triple.starts_with("i686") {
            llc.arg("--x86-asm-syntax=intel");
        }
        run(&mut llc);
        // Object emission also catches illegal encoded-symbol relocations,
        // which assembly-only inspection would miss.
        run(Command::new(tool("llc"))
            .args(["-O3", "-filetype=obj"])
            .arg(&optimized)
            .arg("-o")
            .arg(dir.join(format!("{triple}.o"))));
        let asm = std::fs::read_to_string(asm).unwrap();
        if triple.starts_with("x86") || triple.starts_with("i686") {
            let jumps: Vec<_> = asm
                .lines()
                .filter(|line| line.trim_start().starts_with("jmp\t"))
                .collect();
            assert!(
                jumps.iter().any(|line| {
                    matches!(
                        line.split_whitespace().nth(1),
                        Some(
                            "rax"
                                | "rcx"
                                | "rdx"
                                | "rsi"
                                | "rdi"
                                | "r8"
                                | "r9"
                                | "r10"
                                | "r11"
                                | "rbx"
                                | "rbp"
                                | "r12"
                                | "r13"
                                | "r14"
                                | "r15"
                                | "eax"
                                | "ecx"
                                | "edx"
                                | "esi"
                                | "edi"
                                | "ebx"
                                | "ebp"
                        )
                    )
                }),
                "missing register indirect branch: {triple}"
            );
            assert!(
                jumps.iter().all(|line| !line.contains('[')),
                "raw memory jump reappeared: {triple}"
            );
        }
    }
}

#[test]
fn bcf_precedes_vm_even_with_reversed_order_or_priority() {
    ensure_plugin_built();
    let dir = output_dir().join("vmf-bcf-order");
    let (original, reference, driver) = fixture(&dir);
    for mode in ["default", "explicit-reverse", "priority-reverse", "interpreter"] {
        let transformed = dir.join(format!("{mode}.ll"));
        let mut cmd = command(&original, &transformed, 0, 8, "default<O0>");
        cmd.env("AMICE_BOGUS_CONTROL_FLOW", "true");
        match mode {
            "default" => {
                cmd.env_remove("AMICE_PASS_ORDER");
            },
            "explicit-reverse" => {
                cmd.env("AMICE_PASS_ORDER", "VmFlatten,BogusControlFlow");
            },
            "priority-reverse" => {
                cmd.env_remove("AMICE_PASS_ORDER")
                    .env("AMICE_PASS_PRIORITY_OVERRIDE", "VmFlatten=9999,BogusControlFlow=1");
            },
            "interpreter" => {
                cmd.env("AMICE_PASS_ORDER", "VmFlatten,BogusControlFlow")
                    .env("AMICE_VM_FLATTEN_DISTRIBUTED", "false");
            },
            _ => unreachable!(),
        }
        run(&mut cmd);
        let text = std::fs::read_to_string(&transformed).unwrap();
        assert!(text.contains("\"amice.bcf.clone\"=\"1\""));
        if mode == "interpreter" {
            assert!(text.contains("vm_flatten_opcodes"));
        } else {
            assert!(body(&text, "diamond").contains("indirectbr"));
        }
        let repeated = dir.join(format!("{mode}-repeated.ll"));
        let mut repeat = command(&transformed, &repeated, 0, 8, "default<O0>");
        repeat
            .env("AMICE_PASS_ORDER", "VmFlatten,BogusControlFlow")
            .env("AMICE_BOGUS_CONTROL_FLOW", "true")
            .env(
                "AMICE_VM_FLATTEN_DISTRIBUTED",
                if mode == "interpreter" { "false" } else { "true" },
            );
        run(&mut repeat);
        assert_eq!(
            without_printed_predecessors(&text),
            without_printed_predecessors(&std::fs::read_to_string(repeated).unwrap())
        );
        check_execution(&transformed, &reference, &driver, "O2");
    }
    let mut pipelines = vec!["default<O2>", "default<O3>"];
    if detect_llvm_config().is_some_and(|cfg| common::llvm_major_from_feature(&cfg.feature) >= 15) {
        pipelines.push("lto<O2>");
    }
    for (index, pipeline) in pipelines.into_iter().enumerate() {
        let transformed = dir.join(format!("late-{index}.ll"));
        let mut cmd = command(&original, &transformed, 42, 8, pipeline);
        cmd.env("AMICE_PASS_ORDER", "VmFlatten,BogusControlFlow")
            .env("AMICE_BOGUS_CONTROL_FLOW", "true");
        run(&mut cmd);
        let text = std::fs::read_to_string(&transformed).unwrap();
        assert!(text.contains("\"amice.bcf.clone\"=\"1\"") && text.contains("indirectbr"));
        check_execution(&transformed, &reference, &driver, "O2");
    }
}

#[test]
fn annotations_and_config_file_enable_the_same_distributed_program() {
    ensure_plugin_built();
    let dir = output_dir().join("vmf-config");
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("annotated.c");
    std::fs::write(
        &source,
        r#"
__attribute__((noinline, annotate("+vmf,+vmf_distributed,vmf_max_ops=1,vmf_seed=0")))
unsigned annotated(unsigned x) { if (x & 1) return x * 7; return x + 13; }
__attribute__((noinline, annotate("-vmf")))
unsigned disabled(unsigned x) { if (x & 1) return x * 7; return x + 13; }
__attribute__((noinline))
unsigned configured(unsigned x) { if (x & 1) return x * 7; return x + 13; }
"#,
    )
    .unwrap();
    let input = dir.join("input.ll");
    run(Command::new(tool("clang"))
        .args(["-O0", "-S", "-emit-llvm", "-Xclang", "-disable-O0-optnone"])
        .arg(&source)
        .arg("-o")
        .arg(&input));
    let annotated = dir.join("annotated.ll");
    let mut cmd = command(&input, &annotated, 999, 8, "default<O0>");
    cmd.env("AMICE_VM_FLATTEN", "false")
        .env("AMICE_VM_FLATTEN_DISTRIBUTED", "false");
    run(&mut cmd);
    let text = std::fs::read_to_string(&annotated).unwrap();
    assert!(body(&text, "annotated").contains("indirectbr"));
    assert!(!body(&text, "disabled").contains("indirectbr"));
    assert!(!body(&text, "configured").contains("indirectbr"));
    let config = dir.join("amice.toml");
    std::fs::write(
        &config,
        "[vm_flatten]\nenable=true\ndistributed=true\nmax_ops=1\nseed=0\n",
    )
    .unwrap();
    let configured = dir.join("configured.ll");
    let mut cmd = command(&input, &configured, 999, 8, "default<O0>");
    cmd.env("AMICE_CONFIG_PATH", &config);
    for key in [
        "AMICE_VM_FLATTEN",
        "AMICE_VM_FLATTEN_DISTRIBUTED",
        "AMICE_VM_FLATTEN_MAX_OPS",
        "AMICE_VM_FLATTEN_SEED",
    ] {
        cmd.env_remove(key);
    }
    run(&mut cmd);
    let configured = std::fs::read_to_string(configured).unwrap();
    assert_eq!(body(&text, "annotated"), body(&configured, "annotated"));
    assert!(!body(&configured, "disabled").contains("indirectbr"));
    assert!(body(&configured, "configured").contains("indirectbr"));
}
