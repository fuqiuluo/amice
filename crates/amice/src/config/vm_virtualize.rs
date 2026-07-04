use super::{EnvOverlay, bool_var};
use crate::config::eloquent_config::EloquentConfigParser;
use crate::pass_registry::FunctionAnnotationsOverlay;
use amice_llvm::inkwell2::ModuleExt;
use amice_plugin::inkwell::module::Module;
use amice_plugin::inkwell::values::FunctionValue;
use amice_vm::RuntimeScope;
use log::warn;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VmVirtualizeConfig {
    /// 是否启用 LLVM IR 指令级 VM 虚拟化。
    pub enable: bool,
    /// 可选 profile package 路径；未提供时使用内置测试 profile。
    pub profile_path: Option<PathBuf>,
    /// 可选 runtime scope 覆盖值；只接受 `func` 和 `module`。
    pub runtime_scope: Option<RuntimeScope>,
    /// 可选 marker emission 覆盖值；仅用于测试和调试。
    pub emit_markers: Option<bool>,
    /// 通过 debug 日志输出编码后的 bytecode。
    pub dump_bytecode: bool,
    /// 通过 debug 日志输出 VM lowering 结果。
    pub dump_lowering: bool,
}

impl Default for VmVirtualizeConfig {
    fn default() -> Self {
        Self {
            enable: false,
            profile_path: None,
            runtime_scope: None,
            emit_markers: None,
            dump_bytecode: false,
            dump_lowering: false,
        }
    }
}

impl EnvOverlay for VmVirtualizeConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_VM_VIRTUALIZE").is_ok() {
            self.enable = bool_var("AMICE_VM_VIRTUALIZE", self.enable);
        }

        if let Ok(path) = std::env::var("AMICE_VM_PROFILE_PATH") {
            if !path.trim().is_empty() {
                self.profile_path = Some(PathBuf::from(path));
            }
        }

        if let Ok(scope) = std::env::var("AMICE_VM_RUNTIME_SCOPE") {
            self.runtime_scope = parse_scope(&scope).or(self.runtime_scope);
        }

        if std::env::var("AMICE_VM_EMIT_MARKERS").is_ok() {
            self.emit_markers = Some(bool_var("AMICE_VM_EMIT_MARKERS", self.emit_markers.unwrap_or(false)));
        }

        if std::env::var("AMICE_VM_DUMP_BYTECODE").is_ok() {
            self.dump_bytecode = bool_var("AMICE_VM_DUMP_BYTECODE", self.dump_bytecode);
        }

        if std::env::var("AMICE_VM_DUMP_LOWERING").is_ok() {
            self.dump_lowering = bool_var("AMICE_VM_DUMP_LOWERING", self.dump_lowering);
        }
    }
}

impl FunctionAnnotationsOverlay for VmVirtualizeConfig {
    type Config = VmVirtualizeConfig;

    fn overlay_annotations<'a>(
        &self,
        module: &mut Module<'a>,
        function: FunctionValue<'a>,
    ) -> anyhow::Result<Self::Config> {
        let mut cfg = self.clone();
        let annotations_expr = module
            .read_function_annotate(function)
            .map_err(|e| anyhow::anyhow!("read function annotations failed: {}", e))?
            .join(" ");

        let mut parser = EloquentConfigParser::new();
        parser
            .parse(&annotations_expr)
            .map_err(|e| anyhow::anyhow!("parse function annotations failed: {}", e))?;

        parser
            .get_bool("vm_virtualize")
            .or_else(|| parser.get_bool("vmp"))
            .map(|v| cfg.enable = v);

        parser
            .get_string("vm_profile")
            .or_else(|| parser.get_string("vm_profile_path"))
            .filter(|v| !v.trim().is_empty())
            .map(|v| cfg.profile_path = Some(PathBuf::from(v)));

        parser
            .get_string("vm_runtime_scope")
            .and_then(|v| parse_scope(&v))
            .map(|scope| cfg.runtime_scope = Some(scope));

        Ok(cfg)
    }
}

fn parse_scope(scope: &str) -> Option<RuntimeScope> {
    match scope.trim().parse() {
        Ok(scope) => Some(scope),
        Err(err) => {
            warn!("Ignoring invalid AMICE VM runtime scope: {err}");
            None
        },
    }
}
