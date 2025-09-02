use crate::config::bool_var;
use crate::config::eloquent_config::EloquentConfigParser;
use crate::pass_registry::{EnvOverlay, FunctionAnnotationsOverlay};
use amice_llvm::inkwell2::ModuleExt;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::FunctionValue;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CloneFunctionConfig {
    pub enable: bool,
}

impl Default for CloneFunctionConfig {
    fn default() -> Self {
        Self { enable: false }
    }
}

impl EnvOverlay for CloneFunctionConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_CLONE_FUNCTION").is_ok() {
            self.enable = bool_var("AMICE_CLONE_FUNCTION", self.enable);
        }
    }
}

impl FunctionAnnotationsOverlay for CloneFunctionConfig {
    type Config = CloneFunctionConfig;

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

        parser.get_bool("clone_function").map(|v| cfg.enable = v);

        Ok(cfg)
    }
}
