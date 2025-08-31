use crate::config::flatten::FlattenConfig;
use crate::pass_registry::EnvOverlay;
use amice_macro::amice_config_manager;
use lazy_static::lazy_static;
use log::warn;

pub use alias_access::{AliasAccessConfig, AliasAccessMode};
use basic_block_outlining::BasicBlockOutliningConfig;
pub use bogus_control_flow::{BogusControlFlowConfig, BogusControlFlowMode};
pub use clone_function::CloneFunctionConfig;
use custom_calling_conv::CustomCallingConvConfig;
use delay_offset_loading::DelayOffsetLoadingConfig;
pub use flatten::FlattenMode;
pub use function_wrapper::FunctionWrapperConfig;
pub use indirect_branch::{IndirectBranchConfig, IndirectBranchFlags};
pub use indirect_call::IndirectCallConfig;
pub use lower_switch::LowerSwitchConfig;
pub use mba::MbaConfig;
use param_aggregate::ParamAggregateConfig;
pub use pass_order::PassOrderConfig;
use serde::{Deserialize, Serialize};
pub use shuffle_blocks::{ShuffleBlocksConfig, ShuffleBlocksFlags};
pub use split_basic_block::SplitBasicBlockConfig;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
pub use string_encryption::{StringAlgorithm, StringDecryptTiming, StringEncryptionConfig};
pub use vm_flatten::VmFlattenConfig;

mod alias_access;
mod basic_block_outlining;
mod bogus_control_flow;
mod clone_function;
mod custom_calling_conv;
mod delay_offset_loading;
mod flatten;
mod function_wrapper;
mod indirect_branch;
mod indirect_call;
mod lower_switch;
mod mba;
mod param_aggregate;
mod pass_order;
mod shuffle_blocks;
mod split_basic_block;
mod string_encryption;
mod vm_flatten;

lazy_static! {
    pub static ref CONFIG: Config = {
        let mut cfg = load_from_file_env().unwrap_or_default();
        cfg.overlay_env();
        cfg
    };
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
#[amice_config_manager]
pub struct Config {
    pub pass_order: PassOrderConfig,
    pub string_encryption: StringEncryptionConfig,
    pub indirect_call: IndirectCallConfig,
    pub indirect_branch: IndirectBranchConfig,
    pub function_wrapper: FunctionWrapperConfig,
    pub split_basic_block: SplitBasicBlockConfig,
    pub vm_flatten: VmFlattenConfig,
    pub shuffle_blocks: ShuffleBlocksConfig,
    pub lower_switch: LowerSwitchConfig,
    pub flatten: FlattenConfig,
    pub mba: MbaConfig,
    pub bogus_control_flow: BogusControlFlowConfig,
    pub clone_function: CloneFunctionConfig,
    pub alias_access: AliasAccessConfig,
    pub custom_calling_conv: CustomCallingConvConfig,
    pub delay_offset_loading: DelayOffsetLoadingConfig,
    pub param_aggregate: ParamAggregateConfig,
    pub basic_block_outlining: BasicBlockOutliningConfig,
}

fn is_truthy(value: &str) -> bool {
    match value.trim().to_lowercase().as_str() {
        "1" | "true" | "on" => true,
        "0" | "false" | "off" => false,
        _ => {
            warn!("Unknown boolean value: \"{value}\", defaulting to false");
            false
        },
    }
}

fn bool_var(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) => is_truthy(&v),
        Err(_) => default,
    }
}

/// 解析逗号/分号分隔的列表，去除空项与首尾空白
fn parse_list(input: &str) -> Vec<String> {
    input
        .split(|c| c == ',' || c == ';')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// 解析形如 "Name=123,Other=456" 的映射；忽略无效项并给出告警
fn parse_kv_map(input: &str) -> HashMap<String, i32> {
    let mut out = HashMap::new();
    for part in input.split(|c| c == ',' || c == ';') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        let mut it = p.splitn(2, '=');
        let k = it.next().map(str::trim).unwrap_or_default();
        let v = it.next().map(str::trim).unwrap_or_default();
        if k.is_empty() || v.is_empty() {
            warn!("Ignoring malformed priority override entry: \"{p}\"");
            continue;
        }
        match v.parse::<i32>() {
            Ok(num) => {
                out.insert(k.to_string(), num);
            },
            Err(_) => {
                warn!("Ignoring priority override with non-integer value: \"{p}\"");
            },
        }
    }
    out
}

// the parsers and serde helpers for sub-configs are defined in their own modules
fn load_from_file_env() -> Option<Config> {
    let path = std::env::var("AMICE_CONFIG_PATH").ok()?;
    load_from_file(Path::new(&path)).ok()
}

fn load_from_file(path: &Path) -> anyhow::Result<Config> {
    let content = fs::read_to_string(path)?;
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
    let mut cfg: Config = match ext.as_str() {
        "toml" => toml::from_str(&content)?,
        "yml" | "yaml" => serde_yaml::from_str(&content)?,
        "json" => serde_json::from_str(&content)?,
        _ => {
            if let Ok(v) = toml::from_str(&content) {
                v
            } else if let Ok(v) = serde_yaml::from_str(&content) {
                v
            } else {
                serde_json::from_str(&content)?
            }
        },
    };
    cfg.overlay_env();
    Ok(cfg)
}
