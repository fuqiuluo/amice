use crate::aotu::vmp::compiler::VMPCompilerContext;
use crate::aotu::vmp::isa::{VMPOpcode, VMPValue};
use amice_llvm::inkwell2::{AddInst, AllocaInst, CallInst, GepInst, LoadInst, ReturnInst, StoreInst};
use anyhow::anyhow;
use llvm_plugin::inkwell::llvm_sys::core::{LLVMGetElementType, LLVMGetGEPSourceElementType};
use llvm_plugin::inkwell::llvm_sys::prelude::LLVMValueRef;
use llvm_plugin::inkwell::types::{AnyTypeEnum, BasicType, BasicTypeEnum};
use llvm_plugin::inkwell::values::{AsValueRef, BasicValueEnum, InstructionValue};
use log::{Level, debug, log_enabled, warn};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

pub trait IRConverter<'a> {
    fn to_avm_ir(&self, ctx: &mut VMPCompilerContext) -> anyhow::Result<()>;
}

impl<'a> IRConverter<'a> for AddInst<'a> {
    fn to_avm_ir(&self, ctx: &mut VMPCompilerContext) -> anyhow::Result<()> {
        let lhs = self.get_lhs_value();
        let rhs = self.get_rhs_value();

        if lhs.get_type() != rhs.get_type() {
            return Err(anyhow::anyhow!(
                "Mismatched types in AddInst: lhs is {}, rhs is {}",
                lhs.get_type(),
                rhs.get_type()
            ));
        }

        match (lhs, rhs) {
            (BasicValueEnum::IntValue(lhs), BasicValueEnum::IntValue(rhs)) => {
                match (lhs.is_constant_int(), rhs.is_constant_int()) {
                    (true, false) => {
                        let lhs_value = lhs
                            .get_sign_extended_constant()
                            .ok_or_else(|| anyhow!("Failed to get LHS constant value"))?;
                        let rhs_reg = ctx.get_register(rhs.as_value_ref() as LLVMValueRef)?;
                        ctx.emit(VMPOpcode::Push {
                            value: VMPValue::I64(lhs_value),
                        });
                        ctx.emit(VMPOpcode::PushFromReg { reg: rhs_reg });
                        ctx.emit(VMPOpcode::Add {
                            nsw: self.has_nsw(),
                            nuw: self.has_nuw(),
                        })
                    },
                    (false, true) => {
                        let lhs_reg = ctx.get_register(lhs.as_value_ref() as LLVMValueRef)?;
                        let rhs_value = rhs
                            .get_sign_extended_constant()
                            .ok_or_else(|| anyhow!("Failed to get RHS constant value"))?;
                        ctx.emit(VMPOpcode::PushFromReg { reg: lhs_reg });
                        ctx.emit(VMPOpcode::Push {
                            value: VMPValue::I64(rhs_value),
                        });
                        ctx.emit(VMPOpcode::Add {
                            nsw: self.has_nsw(),
                            nuw: self.has_nuw(),
                        })
                    },
                    (true, true) => {
                        // 两个都是常量，直接计算结果
                        let lhs_value = lhs
                            .get_sign_extended_constant()
                            .ok_or_else(|| anyhow!("Failed to get LHS constant value"))?;
                        let rhs_value = rhs
                            .get_sign_extended_constant()
                            .ok_or_else(|| anyhow!("Failed to get RHS constant value"))?;
                        let result = lhs_value.wrapping_add(rhs_value);
                        ctx.emit(VMPOpcode::Push {
                            value: VMPValue::I64(result),
                        });
                    },
                    (false, false) => {
                        let lhs_reg = ctx.get_register(lhs.as_value_ref() as LLVMValueRef)?;
                        let rhs_reg = ctx.get_register(rhs.as_value_ref() as LLVMValueRef)?;
                        ctx.emit(VMPOpcode::PushFromReg { reg: lhs_reg });
                        ctx.emit(VMPOpcode::PushFromReg { reg: rhs_reg });
                        ctx.emit(VMPOpcode::Add {
                            nsw: self.has_nsw(),
                            nuw: self.has_nuw(),
                        })
                    },
                }
            },
            (BasicValueEnum::FloatValue(lhs), BasicValueEnum::FloatValue(rhs)) => {
                unimplemented!()
            },
            _ => panic!("failed to handle AddInst with lhs: {}, rhs: {}", lhs, rhs),
        }
        let result_reg = ctx.get_or_allocate_register(self.as_value_ref() as LLVMValueRef, false);
        ctx.emit(VMPOpcode::PopToReg { reg: result_reg.value });

        Ok(())
    }
}

impl<'a> IRConverter<'a> for AllocaInst<'a> {
    fn to_avm_ir(&self, ctx: &mut VMPCompilerContext) -> anyhow::Result<()> {
        let allocated_type = self.allocated_type();
        if log_enabled!(Level::Debug) {
            debug!("Allocated type: {}, inst: {:?}", allocated_type, self.get_name());
        }

        let size = allocated_type.size_in_bytes()?;
        let result_reg = ctx.get_or_allocate_register(self.as_value_ref() as LLVMValueRef, true);

        if !ctx.is_polymorphic_inst() {
            ctx.emit(VMPOpcode::Alloca { size });
            ctx.emit(VMPOpcode::PopToReg { reg: result_reg.value });
            return Ok(());
        }

        if rand::random::<bool>() {
            ctx.emit(VMPOpcode::Alloca { size });
        } else {
            ctx.emit(VMPOpcode::Push {
                value: VMPValue::I64(size as i64),
            });
            ctx.emit(VMPOpcode::Alloca2);
        }

        ctx.emit(VMPOpcode::PopToReg { reg: result_reg.value });
        Ok(())
    }
}

impl<'a> IRConverter<'a> for StoreInst<'a> {
    fn to_avm_ir(&self, ctx: &mut VMPCompilerContext) -> anyhow::Result<()> {
        let value = self.get_value();
        let pointer = self.get_pointer();

        let pointer_reg = ctx.get_register(pointer.as_value_ref() as LLVMValueRef)?;

        if log_enabled!(Level::Debug) {
            debug!(
                "Store value: {}, pointer: {}, pointer_reg: {:?}",
                value, pointer, pointer_reg
            );
        }

        match value {
            BasicValueEnum::IntValue(int) => {
                if int.is_constant_int() {
                    let int_val = int
                        .get_sign_extended_constant()
                        .ok_or_else(|| anyhow!("Failed to get constant int value: {}", int))?;

                    // 根据整数宽度选择适当的AVMValue类型
                    let avm_value = match int.get_type().get_bit_width() {
                        1 => VMPValue::I1(int_val != 0),
                        8 => VMPValue::I8(int_val as i8),
                        16 => VMPValue::I16(int_val as i16),
                        32 => VMPValue::I32(int_val as i32),
                        64 => VMPValue::I64(int_val),
                        w => return Err(anyhow!("Unsupported integer width: {}", w)),
                    };

                    ctx.emit(VMPOpcode::PushFromReg { reg: pointer_reg });
                    ctx.emit(VMPOpcode::Push { value: avm_value });
                    ctx.emit(VMPOpcode::StoreValue);
                } else {
                    let value_reg = ctx.get_register(int.as_value_ref() as LLVMValueRef)?;
                    ctx.emit(VMPOpcode::PushFromReg { reg: pointer_reg });
                    ctx.emit(VMPOpcode::PushFromReg { reg: value_reg });
                    ctx.emit(VMPOpcode::StoreValue);
                }
            },
            BasicValueEnum::FloatValue(float) => {
                unimplemented!()
            },
            BasicValueEnum::PointerValue(ptr) => {
                unimplemented!()
            },
            _ => {
                return Err(anyhow!("Unsupported store value type: {:?}", value));
            },
        }

        Ok(())
    }
}

impl<'a> IRConverter<'a> for LoadInst<'a> {
    fn to_avm_ir(&self, ctx: &mut VMPCompilerContext) -> anyhow::Result<()> {
        let pointer = self.get_pointer();
        let result_type = self.get_loaded_type();

        let pointer_reg = ctx.get_register(pointer.as_value_ref() as LLVMValueRef)?;

        ctx.emit(VMPOpcode::PushFromReg { reg: pointer_reg });
        ctx.emit(VMPOpcode::LoadValue);

        let result_reg = ctx.get_or_allocate_register(self.as_value_ref() as LLVMValueRef, false);
        ctx.emit(VMPOpcode::PopToReg { reg: result_reg.value });

        Ok(())
    }
}

impl<'a> IRConverter<'a> for GepInst<'a> {
    fn to_avm_ir(&self, ctx: &mut VMPCompilerContext) -> anyhow::Result<()> {
        let indices = self.get_indices();
        let base_pointer = self.get_pointer();

        let base_pointer_reg = ctx.get_register(base_pointer.as_value_ref() as LLVMValueRef)?;

        // stack: [] -> [base_ptr]
        ctx.emit(VMPOpcode::PushFromReg { reg: base_pointer_reg });

        if indices.is_empty() {
            // 没有索引，直接返回基指针
            let result_reg = ctx.get_or_allocate_register(self.as_value_ref() as LLVMValueRef, false);
            ctx.emit(VMPOpcode::PopToReg { reg: result_reg.value });
            return Ok(());
        }

        // 获取源元素类型
        let source_element_type = self
            .get_element_type()
            .ok_or_else(|| anyhow!("Failed to get element type for GEP"))?;

        // 计算每个索引的偏移量
        let mut current_type = source_element_type;
        for (i, index_opt) in indices.iter().enumerate() {
            let Some(index) = index_opt else {
                return Err(anyhow!("GEP index {} is not an integer value", i));
            };
            let index = index.into_int_value();

            // 计算当前类型的大小
            let element_size = current_type.size_in_bytes()?;

            if index.is_constant_int() {
                let offset = index
                    .get_sign_extended_constant()
                    .ok_or_else(|| anyhow!("Failed to get constant int value for GEP index {}", i))?;
                let byte_offset = offset * element_size as i64;

                ctx.emit(VMPOpcode::Push {
                    value: VMPValue::I64(byte_offset),
                });
                ctx.emit(VMPOpcode::Add { nsw: false, nuw: false });
            } else {
                let index_reg = ctx.get_register(index.as_value_ref() as LLVMValueRef)?;

                ctx.emit(VMPOpcode::PushFromReg { reg: index_reg });
                ctx.emit(VMPOpcode::Push {
                    value: VMPValue::I64(element_size as i64),
                });
                ctx.emit(VMPOpcode::Mul);
                ctx.emit(VMPOpcode::Add { nsw: false, nuw: false });
            }

            // 更新当前类型用于下一个索引
            match current_type {
                BasicTypeEnum::ArrayType(arr) => {
                    current_type = arr.get_element_type();
                },
                BasicTypeEnum::StructType(st) => {
                    unimplemented!()
                },
                BasicTypeEnum::PointerType(ptr) => {
                    unimplemented!()
                },
                _ => {
                    if i != indices.len() - 1 {
                        return Err(anyhow!("GEP index leads to non-indexable type before last index"));
                    }
                },
            }
        }

        let result_reg = ctx.get_or_allocate_register(self.as_value_ref() as LLVMValueRef, false);
        ctx.emit(VMPOpcode::PopToReg { reg: result_reg.value });

        Ok(())
    }
}

impl<'a> IRConverter<'a> for CallInst<'a> {
    fn to_avm_ir(&self, ctx: &mut VMPCompilerContext) -> anyhow::Result<()> {
        let call_function = self
            .get_call_function()
            .ok_or_else(|| anyhow!("Failed to get called function in CallInst"))?;
        let call_params = self.get_call_params();

        // 推送参数到栈（逆序，因为栈是LIFO）
        for param in call_params.iter().rev() {
            match param {
                BasicValueEnum::IntValue(int) if int.is_constant_int() => {
                    let val = int
                        .get_sign_extended_constant()
                        .ok_or_else(|| anyhow!("Failed to get constant parameter value"))?;
                    let avm_val = match int.get_type().get_bit_width() {
                        1 => VMPValue::I1(val != 0),
                        8 => VMPValue::I8(val as i8),
                        16 => VMPValue::I16(val as i16),
                        32 => VMPValue::I32(val as i32),
                        64 => VMPValue::I64(val),
                        w => return Err(anyhow!("Unsupported parameter integer width: {}", w)),
                    };
                    ctx.emit(VMPOpcode::Push { value: avm_val });
                },
                BasicValueEnum::IntValue(int) if !int.is_constant_int() => {
                    let int_reg = ctx.get_register(int.as_value_ref() as LLVMValueRef)?;
                    ctx.emit(VMPOpcode::PushFromReg { reg: int_reg });
                },
                BasicValueEnum::FloatValue(float) => {
                    unimplemented!()
                },
                BasicValueEnum::PointerValue(ptr) => {
                    let ptr_reg = ctx.get_register(ptr.as_value_ref() as LLVMValueRef)?;
                    ctx.emit(VMPOpcode::PushFromReg { reg: ptr_reg });
                },
                _ => {
                    unimplemented!("param: {:?}", param)
                },
            }
        }

        let call = VMPOpcode::Call {
            function_name: call_function.get_name().to_str()?.to_string(),
            function: Some(call_function.as_value_ref() as LLVMValueRef),
            is_void: call_function.get_type().get_return_type().is_none(),
            arg_num: call_function.count_params(),
            args: call_params.iter().map(|v| v.as_value_ref() as LLVMValueRef).collect(),
        };
        ctx.emit(call);

        if call_function.get_type().get_return_type().is_some() {
            let result_reg = ctx.get_or_allocate_register(self.as_value_ref() as LLVMValueRef, false);
            ctx.emit(VMPOpcode::PopToReg { reg: result_reg.value });
        }

        Ok(())
    }
}

impl<'a> IRConverter<'a> for ReturnInst<'a> {
    fn to_avm_ir(&self, ctx: &mut VMPCompilerContext) -> anyhow::Result<()> {
        if self.has_return_value() {
            if let Some(value) = self.get_return_value() {
                match value {
                    BasicValueEnum::IntValue(int) => {
                        if int.is_constant_int() {
                            let int_val = int
                                .get_sign_extended_constant()
                                .ok_or_else(|| anyhow!("Failed to get constant int value: {}", int))?;
                            let avm_value = match int.get_type().get_bit_width() {
                                1 => VMPValue::I1(int_val != 0),
                                8 => VMPValue::I8(int_val as i8),
                                16 => VMPValue::I16(int_val as i16),
                                32 => VMPValue::I32(int_val as i32),
                                64 => VMPValue::I64(int_val),
                                w => return Err(anyhow!("Unsupported integer width: {}", w)),
                            };
                            ctx.emit(VMPOpcode::Push { value: avm_value });
                        } else {
                            let reg = ctx.get_register(int.as_value_ref() as LLVMValueRef)?;
                            ctx.emit(VMPOpcode::PushFromReg { reg });
                        }
                    },
                    BasicValueEnum::PointerValue(ptr) => {
                        let reg = ctx.get_register(ptr.as_value_ref() as LLVMValueRef)?;
                        ctx.emit(VMPOpcode::PushFromReg { reg });
                    },
                    BasicValueEnum::FloatValue(_float) => {
                        unimplemented!()
                    },
                    _ => {
                        return Err(anyhow!("Unsupported return value type: {:?}", value));
                    },
                }
            }
        }

        ctx.emit(VMPOpcode::Ret);
        Ok(())
    }
}

pub trait BasicTypeEnumSized {
    fn size_in_bytes(&self) -> anyhow::Result<usize>;
}

impl BasicTypeEnumSized for BasicTypeEnum<'_> {
    fn size_in_bytes(&self) -> anyhow::Result<usize> {
        match self {
            BasicTypeEnum::ArrayType(array) => {
                let elem_type = array.get_element_type();
                let elem_size = elem_type.size_in_bytes()?;
                let num_elems = array.len() as usize;
                Ok(elem_size * num_elems)
            },
            BasicTypeEnum::FloatType(float) => match float.get_bit_width() {
                16 => Ok(2),
                32 => Ok(4),
                64 => Ok(8),
                80 => Ok(10),
                128 => Ok(16),
                bw => Err(anyhow!("Unsupported float bit width: {}", bw)),
            },
            BasicTypeEnum::IntType(int) => match int.get_bit_width() {
                1 => Ok(1),
                8 => Ok(1),
                16 => Ok(2),
                32 => Ok(4),
                64 => Ok(8),
                128 => Ok(16),
                bw => Err(anyhow!("Unsupported int bit width: {}", bw)),
            },
            BasicTypeEnum::PointerType(_pointer) => {
                // Assuming 64-bit architecture
                Ok(8)
            },
            BasicTypeEnum::StructType(struct_type) => {
                let mut total_size = 0;
                for i in 0..struct_type.count_fields() {
                    let field_type = struct_type
                        .get_field_type_at_index(i)
                        .ok_or_else(|| anyhow!("Failed to get field type at index {} of struct", i))?;
                    total_size += field_type.size_in_bytes()?;
                }
                Ok(total_size)
            },
            BasicTypeEnum::VectorType(vector) => {
                let elem_type = vector.get_element_type();
                let elem_size = elem_type.size_in_bytes()?;
                let num_elems = vector.get_size() as usize;
                Ok(elem_size * num_elems)
            },
            BasicTypeEnum::ScalableVectorType(vector) => {
                let elem_type = vector.get_element_type();
                let elem_size = elem_type.size_in_bytes()?;
                let num_elems = vector.get_size() as usize;
                Ok(elem_size * num_elems)
            },
        }
    }
}
