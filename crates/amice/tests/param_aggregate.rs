//! Integration tests for parameter aggregation.

mod common;

use crate::common::Language;
use common::{CppCompileBuilder, ObfuscationConfig, ensure_plugin_built, fixture_path};

#[test]
fn test_param_aggregate_basic() {
    ensure_plugin_built();

    let result = CppCompileBuilder::new(
        fixture_path("param_aggregate", "basic.c", Language::C),
        "param_aggregate_basic",
    )
    .config(ObfuscationConfig {
        param_aggregate: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .optimization("O2")
    .compile();

    result.assert_success();
    let run = result.run();
    run.assert_success();
    assert_eq!(run.stdout_lines(), ["247 9"]);

    let ir = CppCompileBuilder::new(
        fixture_path("param_aggregate", "basic.c", Language::C),
        "param_aggregate_basic.ll",
    )
    .config(ObfuscationConfig {
        param_aggregate: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .optimization("O2")
    .arg("-S")
    .arg("-emit-llvm")
    .compile();

    ir.assert_success();
    let ir_text = std::fs::read_to_string(&ir.binary_path).expect("failed to read parameter aggregation IR");
    assert!(
        ir_text.contains("param.aggregate"),
        "ParamAggregate was enabled but did not create an aggregate ABI wrapper"
    );
    let abi_ir = CppCompileBuilder::new(
        fixture_path("param_aggregate", "abi_attrs.ll", Language::C),
        "param_aggregate_abi_attrs.ll",
    )
    .config(ObfuscationConfig {
        param_aggregate: Some(true),
        ..ObfuscationConfig::disabled()
    })
    // Check the pass output before target-specific optimization is allowed to
    // discard ABI attributes that are redundant for the host architecture.
    .optimization("O0")
    .arg("-S")
    .arg("-emit-llvm")
    .compile();

    abi_ir.assert_success();
    let abi_ir_text = std::fs::read_to_string(&abi_ir.binary_path).expect("failed to read parameter ABI attribute IR");
    assert!(
        abi_ir_text.contains("abi_narrow.param.aggregate")
            && abi_ir_text.contains("signext")
            && abi_ir_text.contains("zeroext"),
        "ParamAggregate did not preserve narrow integer ABI attributes: {abi_ir_text}"
    );

    let poison_arg = CppCompileBuilder::new(
        fixture_path("param_aggregate", "poison_arg.ll", Language::C),
        "param_aggregate_poison_arg",
    )
    .config(ObfuscationConfig {
        param_aggregate: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .optimization("O2")
    .compile();

    poison_arg.assert_success();
    poison_arg.run().assert_success();

    let poison_arg_ir = CppCompileBuilder::new(
        fixture_path("param_aggregate", "poison_arg.ll", Language::C),
        "param_aggregate_poison_arg.ll",
    )
    .config(ObfuscationConfig {
        param_aggregate: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .optimization("O2")
    .arg("-S")
    .arg("-emit-llvm")
    .compile();

    poison_arg_ir.assert_success();
    let poison_arg_ir_text =
        std::fs::read_to_string(&poison_arg_ir.binary_path).expect("failed to read memory(none) regression IR");
    assert!(
        !poison_arg_ir_text.contains("return_first.param.aggregate"),
        "ParamAggregate must not wrap a memory(none) callee"
    );

    let empty_object = CppCompileBuilder::new(
        fixture_path("param_aggregate", "empty_object.cpp", Language::Cpp),
        "param_aggregate_empty_object",
    )
    .config(ObfuscationConfig {
        param_aggregate: Some(true),
        ..ObfuscationConfig::disabled()
    })
    .optimization("O2")
    .compile();

    empty_object.assert_success();
    empty_object.run().assert_success();
}
