use crate::config::{bool_var, parse_kv_map, parse_list};
use crate::pass_registry::EnvOverlay;
use log::warn;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PassOrderConfig {
    /// 显式安装顺序；若为 None，则按优先级排序
    pub order: Option<Vec<String>>,
    /// 覆盖各 Pass 的优先级（越大越靠前）
    pub priority_override: Option<HashMap<String, i32>>,
}

impl EnvOverlay for PassOrderConfig {
    fn overlay_env(&mut self) {
        // 显式顺序：AMICE_PASS_ORDER="StringEncryption,SplitBasicBlock,ShuffleBlocks,IndirectBranch"
        if let Ok(v) = std::env::var("AMICE_PASS_ORDER") {
            let list = parse_list(&v);
            if !list.is_empty() {
                self.order = Some(list);
            } else {
                warn!("AMICE_PASS_ORDER is set but empty after parsing, ignoring");
            }
        }

        // 覆盖优先级：AMICE_PASS_PRIORITY_OVERRIDE="StringEncryption=1200,IndirectBranch=500"
        if let Ok(v) = std::env::var("AMICE_PASS_PRIORITY_OVERRIDE") {
            let map = parse_kv_map(&v);
            if map.is_empty() {
                warn!("AMICE_PASS_PRIORITY_OVERRIDE is set but empty after parsing, ignoring");
            } else {
                if self.priority_override.is_none() {
                    self.priority_override = Some(HashMap::new());
                }

                // 合并到已有覆盖表（环境变量优先）
                let priority_override = self.priority_override.as_mut().unwrap();
                for (k, val) in map {
                    priority_override.insert(k, val);
                }
            }
        }
    }
}
