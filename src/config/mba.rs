use crate::config::bool_var;
use crate::pass_registry::EnvOverlay;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MbaConfig {
    pub enable: bool,
    pub aux_count: u32,
    pub rewrite_ops: u32,
    pub rewrite_depth: u32,
    // mba pass注册的位置不正确会被llvm优化掉
    // 这里设置为true，会在表达式里面插入一个全局变量
    // 以保证至少不被那么完全地优化
    pub alloc_aux_params_in_global: bool,
    pub fix_stack: bool,
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
    }
}
