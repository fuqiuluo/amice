//! 从 `abi.vm` 解析出的 ABI 模型。
//!
//! # 契约
//! ABI profile 负责把宿主函数参数、宿主返回槽、VM 内部调用状态以及
//! native-call clobber 映射到 AMICE 固定 VM 寄存器模型上。它不负责定义寄存器组本身；
//! 寄存器组会再和 `runtime.vm` 做一致性校验。
//!
//! # 不变量
//! - 整数寄存器是 `x0..x31` 的索引。
//! - 向量寄存器是 `q0..q64` 的索引。
//! - 解析后的 profile 可以包含 `q` 寄存器，但当 `q.lowering = disabled` 时，
//!   verifier 会拒绝任何实际依赖。
//! - `lr` 通过别名表达，lowering 规则不能写死具体 link register。

use serde::{Deserialize, Serialize};

/// ABI 列表声明中可引用的 VM 寄存器。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VmRegister {
    /// 固定 `x0..x31` 组里的整数或指针寄存器。
    X(u8),
    /// 固定 `q0..q64` 组里的宽寄存器。
    Q(u8),
}

/// `abi.vm` 声明的 native-call lowering 策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeCallPolicy {
    /// 在生成的 runtime 中把 callee 还原为直接 LLVM 调用。
    Direct,
}

/// wrapper 使用的宿主 ABI 到 VM ABI 的映射。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbiProfile {
    /// 宿主整数或指针参数序号到 VM `x` 寄存器序号的映射。
    pub integer_args: Vec<u8>,
    /// 宿主标量返回槽到 VM `x` 寄存器序号的映射。
    pub integer_returns: Vec<u8>,
    /// 第一个标量返回寄存器的兼容别名。
    pub integer_return: u8,
    /// 宿主向量参数序号到 VM `q` 寄存器序号的映射。
    pub vector_args: Vec<u8>,
    /// 宿主向量返回槽到 VM `q` 寄存器序号的映射。
    pub vector_returns: Vec<u8>,
    /// VM 内部调用使用的 runtime link-register 别名。
    pub lr_alias: String,
    /// 恢复 VM return PC 时使用的 runtime 别名。
    pub ret_pc_alias: String,
    /// `abi.vm` 是否显式声明了 `call_link`。
    pub call_link_declared: bool,
    /// `abi.vm` 是否显式声明了 `ret_pc`。
    pub ret_pc_declared: bool,
    /// 宿主 ABI 和 VM 调用 ABI 允许的最大 VM 返回值数量。
    pub max_returns: u8,
    /// VM bytecode 内部调用参数使用的寄存器。
    pub vm_call_args: Vec<VmRegister>,
    /// VM bytecode 内部返回值使用的寄存器。
    pub vm_call_returns: Vec<VmRegister>,
    /// VM 指令调用 native LLVM 代码时读取为参数的寄存器。
    pub native_args: Vec<VmRegister>,
    /// 接收 native-call 返回值的寄存器。
    pub native_returns: Vec<VmRegister>,
    /// native call 允许覆盖的寄存器。
    pub native_clobbers: Vec<VmRegister>,
    /// 把直接 LLVM 调用 lowering 成 VM bytecode 时使用的策略。
    pub native_policy: NativeCallPolicy,
}

impl Default for AbiProfile {
    fn default() -> Self {
        Self {
            integer_args: (0..8).collect(),
            integer_returns: vec![0],
            integer_return: 0,
            vector_args: Vec::new(),
            vector_returns: Vec::new(),
            lr_alias: "lr".to_owned(),
            ret_pc_alias: "lr".to_owned(),
            call_link_declared: false,
            ret_pc_declared: false,
            max_returns: 1,
            vm_call_args: (0..8).map(VmRegister::X).collect(),
            vm_call_returns: vec![VmRegister::X(0)],
            native_args: (0..8).map(VmRegister::X).collect(),
            native_returns: vec![VmRegister::X(0)],
            native_clobbers: (0..16).map(VmRegister::X).collect(),
            native_policy: NativeCallPolicy::Direct,
        }
    }
}
