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
}

impl Default for VmFlattenConfig {
    fn default() -> Self {
        Self {
            enable: false,
            random_none_node_opcode: false,
        }
    }
}

impl EnvOverlay for VmFlattenConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_VM_FLATTEN").is_ok() {
            self.enable = bool_var("AMICE_VM_FLATTEN", self.enable);
        }
    }
}

impl FunctionAnnotationsOverlay for VmFlattenConfig {
    type Config = VmFlattenConfig;

    fn overlay_annotations<'a>(
        &self,
        module: &mut llvm_plugin::inkwell::module::Module<'a>,
        function: llvm_plugin::inkwell::values::FunctionValue<'a>,
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

        Ok(cfg)
    }
}
