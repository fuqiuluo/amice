//! `vm_virtualize` pass 使用的 profile 驱动 VM 编译模型。
//!
//! # 职责
//! - 加载 AMICE VMP profile package。
//! - 在改写 LLVM IR 前校验固定寄存器 VM、ABI、ISA、bytecode、decoder 与 runtime 契约。
//! - 向 LLVM pass 提供 VM IR 与 bytecode 编码基础设施。
//!
//! # 边界
//! 本 crate 不直接触碰 LLVM IR。LLVM 翻译和 runtime IR 构造位于
//! 路径：`crates/amice/src/aotu/vm_virtualize`。

/// 从 `abi.vm` 解析出的 ABI 结构。
pub mod abi;
/// profile 驱动的 bytecode encoder 与 package image。
pub mod bytecode;
/// ISA 描述符与解析后的 semantic AST。
pub mod isa;
/// bytecode 编码前生成的 VM IR。
pub mod lowering;
/// profile package 加载器与 DSL parser。
pub mod profile;
/// 从 `runtime.vm` 解析出的 runtime profile 结构。
pub mod runtime;
/// 检查跨文件 VMP 不变量的 profile verifier。
pub mod verify;

pub use bytecode::{BytecodeEncoder, BytecodeImage};
pub use lowering::{
    LabelId, NATIVE_CALL_MAX_ARGS, NATIVE_CALL_MAX_RETURNS, NativeReturn, VmFunction, VmFunctionBuilder, VmInstruction,
    fuse_superinstructions,
};
pub use profile::{ProfileError, ProfilePackage, RuntimeScope};

/// wrapper 传入 VM dispatcher 的固定宿主入口参数槽数量。
///
/// native_call record 仍由 `NATIVE_CALL_MAX_ARGS` 单独限制；这里仅描述原函数入口
/// `host_to_vm` 能覆盖多少个扁平标量参数槽。
pub const HOST_VM_MAX_ARGS: usize = 16;
