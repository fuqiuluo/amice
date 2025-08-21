use log::error;
use crate::config::{bool_var, IndirectBranchFlags};
use crate::config::indirect_branch::parse_indirect_branch_flags;
use crate::pass_registry::EnvOverlay;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FlattenConfig {
    /// 是否开启
    pub enable: bool,
    /// 关掉，打概率会崩溃的哇！
    pub fix_stack: bool,
    /// 自动降级switch，毕竟switch太难处理了喵
    pub lower_switch: bool,
    /// 混淆模式
    pub mode: FlattenMode,
    /// 循环次数
    pub loop_count: usize,
    /// 跳过块太多的方法
    pub skip_big_function: bool,
    /// 是否把`dominator`模式的更新`key_array`的函数给inline了
    pub always_inline: bool,
}

#[derive(Default, Debug, Copy, Clone, Serialize, Deserialize)]
pub enum FlattenMode {
    #[serde(rename = "basic")]
    #[default]
    Basic,
    #[serde(rename = "dominator")]
    DominatorEnhanced
}

impl Default for FlattenConfig {
    fn default() -> Self {
        Self {
            enable: false,
            fix_stack: true,
            lower_switch: true,
            mode: FlattenMode::Basic,
            loop_count: 1,
            skip_big_function: false,
            always_inline: false,
        }
    }
}

fn parse_flatten_mode(s: &str) -> Result<FlattenMode, String> {
    let s = s.to_lowercase();
    match s.as_str() {
        "basic" | "v1" => Ok(FlattenMode::Basic),
        "dominator" | "dominator_enhanced" | "v2" => Ok(FlattenMode::DominatorEnhanced),
        _ => Err(format!("unknown flatten mode: {}", s))
    }
}

impl EnvOverlay for FlattenConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_FLATTEN").is_ok() {
            self.enable = bool_var("AMICE_FLATTEN", self.enable);
        }

        if std::env::var("AMICE_FLATTEN_FIX_STACK").is_ok() {
            self.fix_stack = bool_var("AMICE_FLATTEN_FIX_STACK", self.fix_stack);
        }

        if std::env::var("AMICE_FLATTEN_LOWER_SWITCH").is_ok() {
            self.lower_switch = bool_var("AMICE_FLATTEN_LOWER_SWITCH", self.lower_switch);
        }

        if let Ok(s) = std::env::var("AMICE_FLATTEN_MODE") {
            self.mode = parse_flatten_mode(&s).unwrap_or_else(|e| {
                error!("invalid flatten mode: {}", e);
                FlattenMode::Basic
            })
        }

        if let Ok(s) = std::env::var("AMICE_FLATTEN_LOOP_COUNT") {
            self.loop_count = s.parse::<usize>().unwrap_or(1);
        }

        if std::env::var("AMICE_FLATTEN_SKIP_BIG_FUNCTION").is_ok() {
            self.skip_big_function = bool_var("AMICE_FLATTEN_SKIP_BIG_FUNCTION", self.skip_big_function);
        }

        if std::env::var("AMICE_FLATTEN_ALWAYS_INLINE").is_ok() {
            self.always_inline = bool_var("AMICE_FLATTEN_ALWAYS_INLINE", self.always_inline);
        }
    }
}
