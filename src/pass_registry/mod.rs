use crate::config::Config;
use lazy_static::lazy_static;
use llvm_plugin::ModulePassManager;
use log::info;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Mutex;

lazy_static! {
    static ref REGISTRY: Mutex<Vec<PassEntry>> = Mutex::new(Vec::new());
}

pub trait AmicePass {
    fn name() -> &'static str;
}

pub trait AmicePassLoadable {
    fn init(&mut self, cfg: &Config) -> bool;
}

pub trait EnvOverlay {
    fn overlay_env(&mut self);
}

#[derive(Clone, Copy)]
pub struct PassEntry {
    pub name: &'static str,
    pub priority: i32, // 优先级越大越先执行
    pub add: fn(&Config, &mut ModulePassManager),
}

/// 供宏生成的注册函数调用
pub fn register(entry: PassEntry) {
    let mut reg = REGISTRY.lock().expect("pass_registry: lock poisoned");
    reg.push(entry);
}

/// 安装全部已注册的 pass：按优先级从高到低排序后依次调用 add
pub fn install_all(cfg: &Config, manager: &mut ModulePassManager) {
    // 拷贝一份快照，避免持锁执行用户代码
    let mut entries = {
        let reg = REGISTRY.lock().expect("pass_registry: lock poisoned");
        reg.clone()
    };

    // 如果提供了显式顺序 pass_order，则按该顺序优先
    if let Some(order) = &cfg.pass_order.order {
        // name -> index
        let mut idx = HashMap::with_capacity(order.len());
        for (i, name) in order.iter().enumerate() {
            idx.insert(name.as_str(), i as i32);
        }

        // 不运行不在显示顺序内的模块
        entries.retain(|e| idx.contains_key(e.name));
        entries.sort_by(|a, b| {
            let a_idx = idx.get(a.name).unwrap_or(&i32::MAX);
            let b_idx = idx.get(b.name).unwrap_or(&i32::MAX);
            a_idx.cmp(b_idx)
        });
    } else if let Some(priority_override) = &cfg.pass_order.priority_override {
        entries.sort_by_key(|e| {
            -if let Some(priority) = priority_override.get(e.name) {
                *priority
            } else {
                e.priority
            }
        });
    } else {
        // priority 越大越先安装
        entries.sort_by_key(|e| -e.priority);
    }

    for e in entries {
        info!("pass_registry: install pass: {}", e.name);
        (e.add)(cfg, manager);
    }
}

/// 打印注册表所有的Pass名称
#[allow(dead_code)]
pub fn print_all_registry() {
    let mut passes = REGISTRY
        .lock()
        .expect("pass_registry: lock poisoned")
        .iter()
        .map(|e| (e.name, e.priority))
        .collect::<Vec<_>>();

    passes.sort_by_key(|e| -e.1);

    passes
        .iter()
        .for_each(|(name, priority)| info!("pass_registry: {} (priority: {})", name, priority))
}

/// 清空注册表
#[allow(dead_code)]
pub fn clear() {
    let mut reg = REGISTRY.lock().expect("pass_registry: lock poisoned");
    reg.clear();
}
