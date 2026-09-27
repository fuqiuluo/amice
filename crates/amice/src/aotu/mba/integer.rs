use amice_plugin::inkwell::{
    IntPredicate,
    builder::Builder,
    context::ContextRef,
    values::{AsValueRef, InstructionOpcode, IntValue},
};

pub(super) fn stable<'ctx>(b: &Builder<'ctx>, value: IntValue<'ctx>) -> IntValue<'ctx> {
    if value.is_constant_int() {
        return value;
    }
    // A decomposition uses each input more than once. Freeze once so that undef
    // cannot choose different low/high limbs or different boolean terms.
    // The caller has positioned this builder, and the value is an integer.
    unsafe {
        IntValue::new(amice_plugin::inkwell::llvm_sys::core::LLVMBuildFreeze(
            b.as_mut_ptr(),
            value.as_value_ref(),
            c"mba.stable".as_ptr(),
        ))
    }
}

/// All operations use wrapping arithmetic. Never inherit nsw/nuw from the source.
pub(super) fn emit<'ctx>(
    ctx: ContextRef<'ctx>,
    b: &Builder<'ctx>,
    op: InstructionOpcode,
    x: IntValue<'ctx>,
    y: IntValue<'ctx>,
) -> anyhow::Result<IntValue<'ctx>> {
    let result_ty = x.get_type();
    let ty = if result_ty.get_bit_width() == 1 {
        ctx.i8_type()
    } else {
        result_ty
    };
    let x = b.build_int_z_extend_or_bit_cast(stable(b, x), ty, "mba.x")?;
    let y = b.build_int_z_extend_or_bit_cast(stable(b, y), ty, "mba.y")?;
    let value = match op {
        InstructionOpcode::Add | InstructionOpcode::Sub => {
            let k = ty.get_bit_width() / 2;
            let shift = ty.const_int(k as u64, false);
            let mask = ty.const_int(if k == 64 { u64::MAX } else { (1u64 << k) - 1 }, false);
            let xl = b.build_and(x, mask, "mba.low.x")?;
            let yl = b.build_and(y, mask, "mba.low.y")?;
            let xh = b.build_right_shift(x, shift, false, "mba.high.x")?;
            let yh = b.build_right_shift(y, shift, false, "mba.high.y")?;
            let (lo, hi) = if op == InstructionOpcode::Add {
                let lo = b.build_int_add(xl, yl, "mba.low.sum")?;
                let carry = b.build_right_shift(lo, shift, false, "mba.carry")?;
                let hi = b.build_int_add(xh, yh, "mba.high.sum")?;
                (lo, b.build_int_add(hi, carry, "mba.high.carried")?)
            } else {
                let lo = b.build_int_sub(xl, yl, "mba.low.diff")?;
                let borrow = b.build_int_compare(IntPredicate::ULT, xl, yl, "mba.borrow")?;
                let borrow = b.build_int_z_extend(borrow, ty, "mba.borrow.wide")?;
                let hi = b.build_int_sub(xh, yh, "mba.high.diff")?;
                (lo, b.build_int_sub(hi, borrow, "mba.high.borrowed")?)
            };
            let lo = b.build_and(lo, mask, "mba.low")?;
            let hi = b.build_left_shift(hi, shift, "mba.high")?;
            b.build_or(lo, hi, "mba.join")?
        },
        InstructionOpcode::And => {
            let union = b.build_or(x, y, "mba.union")?;
            let differing = b.build_xor(x, y, "mba.differing")?;
            b.build_int_sub(union, differing, "mba.intersection")?
        },
        InstructionOpcode::Or | InstructionOpcode::Xor => {
            let sum = b.build_int_add(x, y, "mba.sum")?;
            let shared = b.build_and(x, y, "mba.shared")?;
            let shared = if op == InstructionOpcode::Xor {
                b.build_left_shift(shared, ty.const_int(1, false), "mba.twice")?
            } else {
                shared
            };
            b.build_int_sub(sum, shared, "mba.boolean")?
        },
        _ => unreachable!("planner admitted unsupported integer opcode"),
    };
    Ok(b.build_int_truncate_or_bit_cast(value, result_ty, "mba.result")?)
}
