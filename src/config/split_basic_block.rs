use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::FunctionValue;
use super::{EnvOverlay, bool_var};
use serde::{Deserialize, Serialize};
use amice_llvm::inkwell2::ModuleExt;
use crate::pass_registry::FunctionAnnotationsOverlay;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SplitBasicBlockConfig {
    /// Whether to enable basic block splitting obfuscation
    pub enable: bool,
    /// Number of splits to perform on each basic block
    pub num: u32,
}

impl Default for SplitBasicBlockConfig {
    fn default() -> Self {
        Self { enable: false, num: 3 }
    }
}

impl EnvOverlay for SplitBasicBlockConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_SPLIT_BASIC_BLOCK").is_ok() {
            self.enable = bool_var("AMICE_SPLIT_BASIC_BLOCK", self.enable);
        }
        if let Ok(v) = std::env::var("AMICE_SPLIT_BASIC_BLOCK_NUM") {
            self.num = v.parse::<u32>().unwrap_or(self.num);
        }
    }
}

impl FunctionAnnotationsOverlay for SplitBasicBlockConfig {
    type Config = Self;

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

        let mut parser = crate::config::eloquent_config::EloquentConfigParser::new();
        parser
            .parse(&annotations_expr)
            .map_err(|e| anyhow::anyhow!("parse function annotations failed: {}", e))?;

        parser.get_bool("split_basic_block").map(|v| cfg.enable = v);
        parser.get_number::<u32>("split_basic_block_num").map(|v| cfg.num = v);

        Ok(cfg)
    }
}