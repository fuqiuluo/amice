use crate::aotu::vmp::avm::{AVMOpcode, AVMValue};
use crate::aotu::vmp::compiler::AVMCompilerContext;
use amice_llvm::inkwell2::{AddInst, AllocaInst, GepInst, LoadInst, StoreInst};
use llvm_plugin::inkwell::llvm_sys::prelude::LLVMValueRef;
use llvm_plugin::inkwell::types::{AnyTypeEnum, BasicType, BasicTypeEnum};
use llvm_plugin::inkwell::values::{AsValueRef, BasicValueEnum, InstructionValue};
use log::{Level, debug, log_enabled};
use std::ops::Deref;

pub trait IRConverter<'a> {
    fn to_avm_ir(&self, ctx: &mut AVMCompilerContext) -> anyhow::Result<()>;
}

impl<'a> IRConverter<'a> for AddInst<'a> {
    fn to_avm_ir(&self, ctx: &mut AVMCompilerContext) -> anyhow::Result<()> {
        let lhs = self.lhs_value();
        let rhs = self.rhs_value();

        if lhs.get_type() != rhs.get_type() {
            return Err(anyhow::anyhow!(
                "Mismatched types in AddInst: lhs is {}, rhs is {}",
                lhs.get_type(),
                rhs.get_type()
            ));
        }

        match lhs {
            BasicValueEnum::IntValue(int) => {
                if int.is_constant_int() {
                    unimplemented!()
                } else {
                    let lhs_reg = ctx
                        .get_register(lhs.as_value_ref() as LLVMValueRef)
                        .ok_or(anyhow::anyhow!("Failed to get register for lhs in AddInst: {}", lhs))?;
                    let rhs_reg = ctx
                        .get_register(rhs.as_value_ref() as LLVMValueRef)
                        .ok_or(anyhow::anyhow!("Failed to get register for rhs in AddInst: {}", rhs))?;
                    ctx.emit(AVMOpcode::PushFromReg { reg: lhs_reg.value });
                    ctx.emit(AVMOpcode::PushFromReg { reg: rhs_reg.value });
                    ctx.emit(AVMOpcode::Add {
                        nsw: self.has_nsw(),
                        nuw: self.has_nuw(),
                    });
                    let result_reg = ctx.get_or_allocate_register(self.as_value_ref() as LLVMValueRef, true);
                    ctx.emit(AVMOpcode::PopToReg { reg: result_reg.value });
                }
            },
            _ => {
                return Err(anyhow::anyhow!("Unsupported type in AddInst: add({}, {})", lhs, rhs));
            },
        }

        Ok(())
    }
}

impl<'a> IRConverter<'a> for AllocaInst<'a> {
    fn to_avm_ir(&self, ctx: &mut AVMCompilerContext) -> anyhow::Result<()> {
        let allocated_type = self.allocated_type();
        if log_enabled!(Level::Debug) {
            debug!("Allocated type: {}, inst: {:?}", allocated_type, self.get_name());
        }

        let size = allocated_type.size_in_bytes()?;
        let result_reg = ctx.get_or_allocate_register(self.as_value_ref() as LLVMValueRef, true);

        if !ctx.is_polymorphic_inst() {
            ctx.emit(AVMOpcode::Alloca { size });
            ctx.emit(AVMOpcode::PopToReg { reg: result_reg.value });
            return Ok(());
        }

        if rand::random::<bool>() {
            ctx.emit(AVMOpcode::Alloca { size });
        } else {
            ctx.emit(AVMOpcode::Push {
                value: AVMValue::I64(size as i64),
            });
            ctx.emit(AVMOpcode::Alloca2);
        }
        ctx.emit(AVMOpcode::PopToReg { reg: result_reg.value });
        Ok(())
    }
}

impl<'a> IRConverter<'a> for StoreInst<'a> {
    fn to_avm_ir(&self, ctx: &mut AVMCompilerContext) -> anyhow::Result<()> {
        let value = self.get_value();
        let pointer = self.get_pointer();

        let pointer_reg = ctx.get_or_allocate_register(pointer.as_value_ref() as LLVMValueRef, false);
        ctx.emit(AVMOpcode::PushFromReg { reg: pointer_reg.value });
        if log_enabled!(Level::Debug) {
            debug!(
                "Allocated value: {}, pointer: {}, pointer_reg: {:?}",
                value, pointer, pointer_reg
            );
        }
        match value {
            BasicValueEnum::IntValue(int) => {
                if int.is_constant_int() {
                    let int_val = int
                        .get_sign_extended_constant()
                        .ok_or_else(|| anyhow::anyhow!("Failed to get constant int value: {}", int))?;
                    ctx.emit(AVMOpcode::Push {
                        value: AVMValue::I64(int_val),
                    });
                    ctx.emit(AVMOpcode::StoreValue);
                } else {
                    //let value_reg = ctx.get_or_allocate_address(int.as_value_ref() as LLVMValueRef);
                    unimplemented!()
                }
            },
            _ => {
                return Err(anyhow::anyhow!("Unsupported store value type: {:?}", value));
            },
        }

        Ok(())
    }
}

impl<'a> IRConverter<'a> for LoadInst<'a> {
    fn to_avm_ir(&self, ctx: &mut AVMCompilerContext) -> anyhow::Result<()> {
        let pointer = self.get_pointer();
        let result = self.loaded_type();

        let pointer_reg = ctx.get_or_allocate_register(pointer.as_value_ref() as LLVMValueRef, false);
        ctx.emit(AVMOpcode::PushFromReg { reg: pointer_reg.value });

        match result {
            AnyTypeEnum::IntType(int) => {
                ctx.emit(AVMOpcode::LoadValue);
                if ctx.is_type_check_enabled() {
                    let width = int.get_bit_width();
                    ctx.emit(AVMOpcode::TypeCheckInt { width });
                }
                let result_reg = ctx.get_or_allocate_register(self.as_value_ref() as LLVMValueRef, true);
                ctx.emit(AVMOpcode::PopToReg { reg: result_reg.value });
            },
            AnyTypeEnum::VoidType(_) => {
                return Err(anyhow::anyhow!("Cannot load void type"));
            },
            _ => {
                return Err(anyhow::anyhow!("Unsupported load result type: {:?}", result));
            },
        }

        Ok(())
    }
}

impl<'a> IRConverter<'a> for GepInst<'a> {
    fn to_avm_ir(&self, ctx: &mut AVMCompilerContext) -> anyhow::Result<()> {
        let indices = self.get_indices();
        let pointer = self.get_pointer();

        let pointer_reg = ctx.get_or_allocate_register(pointer.as_value_ref() as LLVMValueRef, false);
        ctx.emit(AVMOpcode::PushFromReg { reg: pointer_reg.value });
        if indices.is_empty() {
            ctx.emit(AVMOpcode::Push {
                value: AVMValue::I64(0),
            });
        } else {
            for index in indices {}
        }
        ctx.emit(AVMOpcode::Add { nsw: false, nuw: false });

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
                bw => Err(anyhow::anyhow!("Unsupported float bit width: {}", bw)),
            },
            BasicTypeEnum::IntType(int) => match int.get_bit_width() {
                1 => Ok(1),
                8 => Ok(1),
                16 => Ok(2),
                32 => Ok(4),
                64 => Ok(8),
                128 => Ok(16),
                bw => Err(anyhow::anyhow!("Unsupported int bit width: {}", bw)),
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
                        .ok_or_else(|| anyhow::anyhow!("Failed to get field type at index {} of struct", i))?;
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
