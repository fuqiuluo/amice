use crate::config::bool_var;
use crate::config::eloquent_config::EloquentConfigParser;
use crate::pass_registry::{EnvOverlay, FunctionAnnotationsOverlay};
use amice_llvm::inkwell2::ModuleExt;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::FunctionValue;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct BasicBlockOutliningConfig {
    pub enable: bool,
    pub max_extractor_size: usize,
}

impl EnvOverlay for BasicBlockOutliningConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_BASIC_BLOCK_OUTLINING").is_ok() {
            self.enable = bool_var("AMICE_BASIC_BLOCK_OUTLINING", self.enable);
        }

        if std::env::var("AMICE_BASIC_BLOCK_OUTLINING_MAX_EXTRACTOR_SIZE").is_ok() {
            self.max_extractor_size = usize::from_str_radix(
                &std::env::var("AMICE_BASIC_BLOCK_OUTLINING_MAX_EXTRACTOR_SIZE").unwrap(),
                10,
            )
            .unwrap();
        } else {
            self.max_extractor_size = 16;
        }
    }
}

impl FunctionAnnotationsOverlay for BasicBlockOutliningConfig {
    type Config = BasicBlockOutliningConfig;

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
            .get_bool("basic_block_outlining")
            .or_else(|| parser.get_bool("bb2func"))
            .map(|v| cfg.enable = v);

        parser
            .get_number::<usize>("basic_block_outlining_max_extractor_size")
            .or_else(|| parser.get_number::<usize>("bb2func_max_extractor_size"))
            .map(|v| cfg.max_extractor_size = v);

        Ok(cfg)
    }
}
