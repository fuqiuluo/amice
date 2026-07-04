//! 从 `runtime.vm` 解析出的 runtime profile 模型。
//!
//! # 契约
//! runtime 设置定义生成的 dispatcher 放在哪里、bytecode 多态如何派生 key，
//! 以及哪些固定寄存器组存在。解析完成后只允许 `func` 和 `module` 两种 scope。
//!
//! # 不变量
//! - AMICE VMP 始终是包含 `x0..x31` 与 `q0..q64` 的寄存器 VM。
//! - `q.lowering = disabled` 会被显式建模，并在 lowering 接受宽 ABI、
//!   语义或规则依赖前完成校验。
//! - 不支持的 dispatch 策略必须由 parser 或 verifier 拒绝，不能回退到写死的 runtime。

use crate::profile::RuntimeScope;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// `runtime.vm` 声明的寄存器组。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterBank {
    /// 寄存器组名称，当前只能是 `x` 或 `q`。
    pub name: String,
    /// 此寄存器组包含的第一个寄存器序号。
    pub first: u8,
    /// 此寄存器组包含的最后一个寄存器序号。
    pub last: u8,
    /// 此寄存器组存储的 DSL 值类型，例如 `u64` 或 `v128`。
    pub value_type: String,
}

/// `runtime.vm` 声明的控制状态槽。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlStateSlot {
    /// 控制状态字段名，例如 `pc` 或 `flags`。
    pub name: String,
    /// 此字段存储的 DSL 值类型。
    pub value_type: String,
}

/// profile 声明的 runtime 状态与 dispatch 模型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeProfile {
    /// runtime 生成 scope，仅限 `func` 或 `module`。
    pub scope: RuntimeScope,
    /// 用来派生 opcode、key 和 layout 多态的 scope。
    pub polymorph_scope: RuntimeScope,
    /// 生成的 LLVM runtime 使用的 dispatch 策略。
    pub dispatch: DispatchStrategy,
    /// 是否生成用于测试和调试识别的稳定 marker 符号。
    pub emit_markers: bool,
    /// profile 声明的寄存器组；verifier 会强制固定模型。
    pub banks: Vec<RegisterBank>,
    /// 符号别名，例如 `lr -> x30`。
    pub aliases: HashMap<String, String>,
    /// 暴露给 handler 语义的非寄存器解释器状态。
    pub control_state: Vec<ControlStateSlot>,
    /// 此 profile 是否接受宽寄存器 lowering。
    pub q_lowering: WideRegisterPolicy,
    /// runtime 多态与混淆增强开关。
    pub enhancements: RuntimeEnhancements,
}

/// verifier 接受的 runtime dispatch 策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DispatchStrategy {
    /// 基于解码后的 opcode 生成 LLVM `switch`。
    Switch,
}

/// profile 声明的宽 VM 寄存器的当前策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WideRegisterPolicy {
    /// 在状态模型中保留 `q` 组，但拒绝任何 lowering 依赖。
    Disabled,
}

/// verifier 接受的 runtime 增强开关。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeEnhancements {
    /// 请求 threaded dispatch；当前启用时会被拒绝。
    pub threaded_dispatch: bool,
    /// 请求 indirect-branch dispatch；当前启用时会被拒绝。
    pub indirect_branch_dispatch: bool,
    /// 把每个生成的 handler 拆成 profile 派生的入口块与执行块。
    pub handler_splitting: bool,
    /// 使用当前 profile 或函数 key 打乱 handler switch case 顺序。
    pub handler_order_shuffle: bool,
    /// 允许 encoder 和 runtime 使用 ISA 声明的全部 opcode alias。
    pub opcode_alias: bool,
    /// 按配置粒度克隆 handler body。
    pub handler_clone: HandlerClonePolicy,
}

/// `runtime.vm` 声明的 handler 克隆粒度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandlerClonePolicy {
    /// 在选定 runtime scope 内共享一个 handler body。
    Disabled,
    /// 为 runtime 级多态生成 per-function handler clone。
    PerFunction,
}

/// LLVM 侧 emitter 消费的最小 runtime 生成计划。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEmitterPlan {
    /// dispatcher 函数使用的 LLVM 符号名。
    pub dispatch_name: &'static str,
    /// verifier 归一化后的可用 `x` 寄存器数量。
    pub x_register_count: u8,
    /// verifier 归一化后的可用 `q` 寄存器数量。
    pub q_register_count: u8,
}

impl RuntimeEmitterPlan {
    /// 构建 VMP 设计要求的固定寄存器 runtime 计划。
    pub fn from_profile(profile: &RuntimeProfile) -> Self {
        Self {
            dispatch_name: ".amice.vm.dispatch",
            x_register_count: bank_len(profile, "x").unwrap_or(32),
            q_register_count: bank_len(profile, "q").unwrap_or(65),
        }
    }
}

fn bank_len(profile: &RuntimeProfile, name: &str) -> Option<u8> {
    profile
        .banks
        .iter()
        .find(|bank| bank.name == name)
        .map(|bank| bank.last - bank.first + 1)
}

impl Default for RuntimeProfile {
    fn default() -> Self {
        Self {
            scope: RuntimeScope::Module,
            polymorph_scope: RuntimeScope::Func,
            dispatch: DispatchStrategy::Switch,
            emit_markers: false,
            banks: vec![
                RegisterBank {
                    name: "x".to_owned(),
                    first: 0,
                    last: 31,
                    value_type: "u64".to_owned(),
                },
                RegisterBank {
                    name: "q".to_owned(),
                    first: 0,
                    last: 64,
                    value_type: "v128".to_owned(),
                },
            ],
            aliases: HashMap::from([("lr".to_owned(), "x30".to_owned()), ("sp".to_owned(), "x31".to_owned())]),
            q_lowering: WideRegisterPolicy::Disabled,
            control_state: vec![
                ControlStateSlot {
                    name: "pc".to_owned(),
                    value_type: "label".to_owned(),
                },
                ControlStateSlot {
                    name: "flags".to_owned(),
                    value_type: "u64".to_owned(),
                },
            ],
            enhancements: RuntimeEnhancements {
                threaded_dispatch: false,
                indirect_branch_dispatch: false,
                handler_splitting: false,
                handler_order_shuffle: false,
                opcode_alias: false,
                handler_clone: HandlerClonePolicy::Disabled,
            },
        }
    }
}
