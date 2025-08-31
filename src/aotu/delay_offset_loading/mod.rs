// translate from AmaObfuscatePass

use crate::config::Config;
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::inkwell2::{BuilderExt, FunctionExt, InstructionExt, LLVMValueRefExt, VerifyResult};
use amice_llvm::ptr_type;
use amice_macro::amice;
use llvm_plugin::inkwell::llvm_sys::prelude::LLVMValueRef;
use llvm_plugin::inkwell::module::{Linkage, Module};
use llvm_plugin::inkwell::values::{AsValueRef, BasicValue, InstructionOpcode};
use llvm_plugin::inkwell::{AddressSpace, GlobalVisibility};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{debug, error, warn};
use std::collections::HashMap;
use std::ops::BitXor;

#[amice(priority = 1150, name = "DelayOffsetLoading", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct DelayOffsetLoading {
    enable: bool,
    xor_offset: bool,
}

impl AmicePassLoadable for DelayOffsetLoading {
    fn init(&mut self, cfg: &Config, _position: PassPosition) -> bool {
        self.enable = cfg.delay_offset_loading.enable;
        self.xor_offset = cfg.delay_offset_loading.xor_offset;
        self.enable
    }
}

impl LlvmModulePass for DelayOffsetLoading {
    fn run_pass(&self, module: &mut Module<'_>, _manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        let mut shared_global_offset_map = HashMap::new();
        for function in module.get_functions() {
            if function.is_undef_function() {
                continue;
            }

            let mut gep_vec = Vec::new();
            for bb in function.get_basic_blocks() {
                for inst in bb.get_instructions() {
                    if inst.get_opcode() == InstructionOpcode::GetElementPtr {
                        let gep = inst.into_gep_inst();
                        if !gep.get_type().is_pointer_type() {
                            continue;
                        }
                        if gep.get_indices().iter().any(|operand| {
                            if let Some(operand) = operand
                                && operand.is_int_value()
                            {
                                let int_value = operand.into_int_value();
                                int_value.is_constant_int() && int_value.is_const() && !int_value.is_null()
                            } else {
                                false
                            }
                        }) {
                            continue;
                        }
                        gep_vec.push(gep);
                    }
                }
            }

            if gep_vec.is_empty() {
                continue;
            }

            let ctx = module.get_context();
            let i8_type = ctx.i8_type();
            let i32_type = ctx.i32_type();

            let i8_ptr = ptr_type!(ctx, i8_type);

            let builder = ctx.create_builder();
            for gep_inst in &gep_vec {
                let Some(struct_ptr) = gep_inst.get_pointer_operand() else {
                    continue;
                };
                let Some(offset) = gep_inst.accumulate_constant_offset(module) else {
                    continue;
                };

                let (global_offset_value, xor_key) = if !shared_global_offset_map.contains_key(&offset) {
                    let xor_key = if self.xor_offset { rand::random::<u64>() } else { 0 };
                    let initializer = if self.xor_offset {
                        i32_type.const_int(offset.bitxor(xor_key), false)
                    } else {
                        i32_type.const_int(offset, false)
                    };
                    let global_value = module.add_global(i32_type, None, &format!(".ama.offset.{}", offset));
                    global_value.set_constant(false);
                    global_value.set_linkage(Linkage::Private);
                    global_value.set_initializer(&initializer);
                    shared_global_offset_map.insert(offset, (global_value, xor_key));
                    (global_value, xor_key)
                } else {
                    shared_global_offset_map[&offset]
                };

                builder.position_before(&gep_inst);
                let Ok(offset_value) =
                    builder.build_load2(i32_type, global_offset_value.as_pointer_value(), "offset_val")
                else {
                    error!("(delay-offset-loading) load global_offset_value failed");
                    continue;
                };
                let mut offset_value = offset_value.into_int_value();

                if self.xor_offset {
                    offset_value =
                        match builder.build_xor(offset_value, i32_type.const_int(xor_key, false), "offset_val_no_xor") {
                            Ok(value) => value,
                            Err(e) => {
                                error!("(delay-offset-loading) xor offset value failed: {}", e);
                                continue;
                            },
                        }
                }

                let Ok(ptr) = builder.build_bit_cast(struct_ptr.into_pointer_value(), i8_ptr, "st_ptr_as_i8_ptr")
                else {
                    error!("(delay-offset-loading) bit cast struct_ptr to i8_ptr failed");
                    continue;
                };
                let Ok(gep) =
                    builder.build_gep2(i8_type, ptr.into_pointer_value(), &[offset_value], "st_ptr_to_gep_ptr")
                else {
                    error!("(delay-offset-loading) gep failed");
                    continue;
                };
                let Ok(ptr) =
                    builder.build_bit_cast(gep, gep_inst.get_type().into_pointer_type(), "gep_ptr_to_gep_ty_ptr")
                else {
                    error!("(delay-offset-loading) bit cast gep to gep_inst.get_type() failed");
                    continue;
                };
                let ptr = (ptr.as_value_ref() as LLVMValueRef).into_instruction_value();

                gep_inst.replace_all_uses_with(&ptr);
                gep_inst.erase_from_basic_block();
            }

            if let VerifyResult::Broken(err) = function.verify_function() {
                warn!(
                    "(delay-offset-loading) function {:?} is broken: {}",
                    function.get_name(),
                    err
                );
            }
        }

        PreservedAnalyses::None
    }
}
