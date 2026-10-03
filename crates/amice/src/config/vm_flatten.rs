use super::{EnvOverlay, bool_var};
use crate::config::eloquent_config::EloquentConfigParser;
use crate::pass_registry::FunctionAnnotationsOverlay;
use amice_llvm::inkwell2::ModuleExt;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VmFlattenConfig {
    /// Whether to enable virtual machine based control flow flattening
    pub enable: bool,
    pub random_none_node_opcode: bool,
    /// Emit per-block index programs and indirect jumps instead of the interpreter.
    pub distributed: bool,
    /// Maximum index operations per transfer, including one S-box, clamped to 1..=32.
    pub max_ops: u32,
    /// Number of interleaved opcode program variants in distributed mode, clamped to 1..=3.
    pub program_variants: u32,
    /// Diversification seed for distributed mode, including reproducible seed zero.
    pub seed: u64,
}

impl Default for VmFlattenConfig {
    fn default() -> Self {
        Self {
            enable: false,
            random_none_node_opcode: false,
            distributed: false,
            max_ops: 8,
            program_variants: 3,
            seed: rand::random(),
        }
    }
}

impl EnvOverlay for VmFlattenConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_VM_FLATTEN").is_ok() {
            self.enable = bool_var("AMICE_VM_FLATTEN", self.enable);
        }
        self.distributed = bool_var("AMICE_VM_FLATTEN_DISTRIBUTED", self.distributed);
        if let Ok(value) = std::env::var("AMICE_VM_FLATTEN_MAX_OPS") {
            match value.parse() {
                Ok(value) => self.max_ops = value,
                Err(_) => log::warn!("Invalid AMICE_VM_FLATTEN_MAX_OPS: {value}"),
            }
        }
        if let Ok(value) = std::env::var("AMICE_VM_FLATTEN_PROGRAM_VARIANTS") {
            match value.parse() {
                Ok(value) => self.program_variants = value,
                Err(_) => log::warn!("Invalid AMICE_VM_FLATTEN_PROGRAM_VARIANTS: {value}"),
            }
        }
        if let Ok(value) = std::env::var("AMICE_VM_FLATTEN_SEED") {
            match value.parse() {
                Ok(value) => self.seed = value,
                Err(_) => log::warn!("Invalid AMICE_VM_FLATTEN_SEED: {value}"),
            }
        }
    }
}

impl FunctionAnnotationsOverlay for VmFlattenConfig {
    type Config = VmFlattenConfig;

    fn overlay_annotations<'a>(
        &self,
        module: &mut amice_plugin::inkwell::module::Module<'a>,
        function: amice_plugin::inkwell::values::FunctionValue<'a>,
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
            .get_bool("vm_flatten")
            .or_else(|| parser.get_bool("vmf"))
            .map(|v| cfg.enable = v);

        if let Some(value) = parser
            .get_bool("vm_flatten_distributed")
            .or_else(|| parser.get_bool("vmf_distributed"))
        {
            cfg.distributed = value;
        }
        if let Some(value) = parser
            .get_number("vm_flatten_max_ops")
            .or_else(|| parser.get_number("vmf_max_ops"))
        {
            cfg.max_ops = value;
        }
        if let Some(value) = parser
            .get_number("vm_flatten_program_variants")
            .or_else(|| parser.get_number("vmf_program_variants"))
        {
            cfg.program_variants = value;
        }
        if let Some(value) = parser
            .get_number("vm_flatten_seed")
            .or_else(|| parser.get_number("vmf_seed"))
        {
            cfg.seed = value;
        }

        Ok(cfg)
    }
}
