use crate::config::Config;
use lazy_static::lazy_static;
use llvm_plugin::ModulePassManager;
use log::info;
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

    // priority 越大越先安装
    entries.sort_by_key(|e| -e.priority);

    for e in entries {
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
