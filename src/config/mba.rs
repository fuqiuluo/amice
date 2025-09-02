use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::FunctionValue;
use crate::config::bool_var;
use crate::pass_registry::{EnvOverlay, FunctionAnnotationsOverlay};
use serde::{Deserialize, Serialize};
use amice_llvm::inkwell2::ModuleExt;
use crate::config::eloquent_config::EloquentConfigParser;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MbaConfig {
    /// Whether to enable Mixed Boolean Arithmetic (MBA) obfuscation
    pub enable: bool,
    /// Number of auxiliary parameters to use in MBA expressions
    pub aux_count: u32,
    /// Number of operations to rewrite with MBA expressions
    pub rewrite_ops: u32,
    /// Maximum depth of MBA expression rewriting
    pub rewrite_depth: u32,
    /// Allocate auxiliary parameters in global variables to prevent optimization
    /// When true, inserts global variables in expressions to resist LLVM optimizations
    pub alloc_aux_params_in_global: bool,
    /// Enable stack fixing to prevent crashes during MBA transformation
    pub fix_stack: bool,
    ///  MBAFunction must not be optimized
    pub opt_none: bool,
}

impl Default for MbaConfig {
    fn default() -> Self {
        Self {
            enable: false,
            aux_count: 2,
            rewrite_ops: 24,
            rewrite_depth: 3,
            alloc_aux_params_in_global: false,
            fix_stack: false,
            opt_none: false,
        }
    }
}

impl EnvOverlay for MbaConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_MBA").is_ok() {
            self.enable = bool_var("AMICE_MBA", self.enable);
        }

        if let Ok(v) = std::env::var("AMICE_MBA_AUX_COUNT") {
            self.aux_count = v.parse::<u32>().unwrap_or(self.aux_count);
        }

        if let Ok(v) = std::env::var("AMICE_MBA_REWRITE_OPS") {
            self.rewrite_ops = v.parse::<u32>().unwrap_or(self.rewrite_ops);
        }

        if let Ok(v) = std::env::var("AMICE_MBA_REWRITE_DEPTH") {
            self.rewrite_depth = v.parse::<u32>().unwrap_or(self.rewrite_depth);
        }

        if std::env::var("AMICE_MBA_ALLOC_AUX_PARAMS_IN_GLOBAL").is_ok() {
            self.alloc_aux_params_in_global =
                bool_var("AMICE_MBA_ALLOC_AUX_PARAMS_IN_GLOBAL", self.alloc_aux_params_in_global);
        }

        if std::env::var("AMICE_MBA_FIX_STACK").is_ok() {
            self.fix_stack = bool_var("AMICE_MBA_FIX_STACK", self.fix_stack);
        }

        if std::env::var("AMICE_MBA_OPT_NONE").is_ok() {
            self.opt_none = bool_var("AMICE_MBA_OPT_NONE", self.opt_none);
        }
    }
}

impl FunctionAnnotationsOverlay for MbaConfig {
    type Config = MbaConfig;

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
            .get_bool("mba")
            .or_else(|| parser.get_bool("linearmba"))
            .map(|v| cfg.enable = v);

        Ok(cfg)
    }
}