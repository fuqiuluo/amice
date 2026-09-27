use super::{algebra, integer};
use crate::config::MbaConfig;
use amice_plugin::inkwell::{
    attributes::{Attribute, AttributeLoc},
    builder::Builder,
    context::ContextRef,
    module::{Linkage, Module},
    values::{BasicValue, FunctionValue, InstructionOpcode as Op, InstructionValue, IntValue, PhiValue},
};
use std::collections::{HashMap, HashSet};

// Unbiased exponents -128..127 keep both carriers and their smallest nonzero
// difference (one mantissa unit) normal and finite, including with FTZ/DAZ.
const MIN_EXPONENT: u64 = 1023 - 128;

fn binary(op: Op) -> bool {
    matches!(op, Op::Add | Op::Sub | Op::And | Op::Or | Op::Xor)
}

fn operand(inst: InstructionValue<'_>, index: u32) -> IntValue<'_> {
    inst.get_operand(index).unwrap().value().unwrap().into_int_value()
}

fn float_allowed(module: &Module<'_>, function: FunctionValue<'_>) -> bool {
    let target = module.get_triple();
    let triple = target.as_str().to_string_lossy();
    if !(triple.starts_with("x86_64-") || triple.starts_with("aarch64-")) {
        return false;
    }
    for name in ["strictfp", "noimplicitfloat", "naked"] {
        if function
            .get_enum_attribute(AttributeLoc::Function, Attribute::get_named_enum_kind_id(name))
            .is_some()
        {
            return false;
        }
    }
    if function
        .get_string_attribute(AttributeLoc::Function, "use-soft-float")
        .is_some_and(|a| a.get_string_value().to_bytes() == b"true")
    {
        return false;
    }
    if let Some(a) = function.get_string_attribute(AttributeLoc::Function, "target-features") {
        let features = a.get_string_value().to_string_lossy();
        if features
            .split(',')
            .any(|v| matches!(v, "-sse" | "-sse2" | "-fp-armv8" | "-neon" | "+soft-float"))
        {
            return false;
        }
    }
    true
}

fn safe_phi(inst: InstructionValue<'_>) -> bool {
    // An empty entry phi has no incoming carrier whose range we can establish.
    // Keep it narrow and freeze it at the region boundary instead.
    if PhiValue::try_from(inst).unwrap().count_incoming() == 0 {
        return false;
    }
    // A pad must remain the first non-phi instruction in its block; our decode
    // would otherwise be inserted before it.
    if inst
        .get_parent()
        .unwrap()
        .get_instructions()
        .find(|i| i.get_opcode() != Op::Phi)
        .is_some_and(|i| {
            matches!(
                i.get_opcode(),
                Op::LandingPad | Op::CatchPad | Op::CleanupPad | Op::CatchSwitch
            )
        })
    {
        return false;
    }
    PhiValue::try_from(inst).unwrap().get_incomings().all(|(v, block)| {
        // Invoke/callbr results exist only after the edge, and catchswitch blocks
        // cannot contain our edge conversions. Leave these phis as boundaries.
        !v.as_instruction_value().is_some_and(|i| i.is_terminator())
            && block
                .get_terminator()
                .is_some_and(|i| i.get_opcode() != Op::CatchSwitch)
    })
}

pub(super) fn rewrite<'ctx>(
    module: &Module<'ctx>,
    function: FunctionValue<'ctx>,
    cfg: &MbaConfig,
) -> anyhow::Result<(bool, bool)> {
    let fp = cfg.float_regions && float_allowed(module, function);
    // Reserve entry setup before choosing originals. No global is created
    // unless a real FP region survives planning.
    let mut budget = (cfg.max_added_instructions as u64).saturating_sub(if fp && cfg.opaque_guard { 12 } else { 0 });
    let mut planned = Vec::new();
    for block in function.get_basic_blocks() {
        for inst in block.get_instructions() {
            if planned.len() >= cfg.max_instructions.min(512) as usize {
                break;
            }
            // Inkwell's general instruction-to-value conversion cannot represent
            // EH token types. Only inspect values of candidate opcodes.
            let op = inst.get_opcode();
            if !binary(op) && !matches!(op, Op::Select | Op::Phi) {
                continue;
            }
            let Ok(value) = IntValue::try_from(inst) else {
                continue;
            };
            let width = value.get_type().get_bit_width();
            if width > 128 {
                continue;
            }
            if !binary(op) && !(fp && width <= 32 && (op == Op::Select || (op == Op::Phi && safe_phi(inst)))) {
                continue;
            }
            // Includes both input encodings, canonicalization and output decode.
            let cost = if op == Op::Phi {
                2 + 3 * PhiValue::try_from(inst).unwrap().count_incoming() as u64
            } else if fp && width <= 32 && cfg.pre_expand && binary(op) {
                // At most five carrier operations plus input/output conversion.
                96
            } else if fp && width <= 32 && binary(op) {
                24
            } else {
                20
            };
            if cost > budget {
                continue;
            }
            budget -= cost;
            planned.push(inst);
        }
    }
    // Propagate membership along same-width SSA edges. Without pre-expansion,
    // pure boolean components stay on the small integer fallback.
    let narrow: HashSet<_> = planned
        .iter()
        .copied()
        .filter(|i| fp && IntValue::try_from(*i).unwrap().get_type().get_bit_width() <= 32)
        .collect();
    let mut selected: HashSet<_> = narrow
        .iter()
        .copied()
        .filter(|i| matches!(i.get_opcode(), Op::Add | Op::Sub) || (cfg.pre_expand && binary(i.get_opcode())))
        .collect();
    loop {
        let before = selected.len();
        for &inst in &planned {
            if !narrow.contains(&inst) {
                continue;
            }
            let ty = IntValue::try_from(inst).unwrap().get_type();
            for n in 0..inst.get_num_operands() {
                let Some(v) = inst.get_operand(n).and_then(|o| o.value()) else {
                    continue;
                };
                if !v.is_int_value() || v.into_int_value().get_type() != ty {
                    continue;
                }
                if let Some(source) = v.as_instruction_value() {
                    if narrow.contains(&source) && (selected.contains(&inst) || selected.contains(&source)) {
                        selected.insert(inst);
                        selected.insert(source);
                    }
                }
            }
        }
        if before == selected.len() {
            break;
        }
    }
    let ctx = module.get_context();
    let builder = ctx.create_builder();
    let ty = ctx.i64_type();
    let volatile = cfg.opaque_guard && !selected.is_empty();
    let seed = if volatile {
        let entry = function.get_first_basic_block().unwrap();
        let anchor = entry.get_instructions().find(|i| i.get_opcode() != Op::Phi).unwrap();
        builder.position_before(&anchor);
        let global = module.add_global(ctx.i32_type(), None, ".amice.mba.seed");
        global.set_linkage(Linkage::Private);
        global.set_initializer(&ctx.i32_type().const_int(rand::random::<u32>() as u64, false));
        global.set_alignment(4);
        let loaded = builder.build_load(ctx.i32_type(), global.as_pointer_value(), "mba.seed")?;
        loaded.as_instruction_value().unwrap().set_volatile(true)?;
        builder.build_int_z_extend(loaded.into_int_value(), ty, "mba.seed.wide")?
    } else {
        ty.const_int(rand::random::<u32>() as u64, false)
    };
    let (exponent, guard) = if volatile {
        let high = builder.build_right_shift(seed, ty.const_int(17, false), false, "mba.scale.source")?;
        let range = builder.build_and(high, ty.const_int(255, false), "mba.scale.range")?;
        let biased = builder.build_int_add(range, ty.const_int(MIN_EXPONENT, false), "mba.scale.biased")?;
        let exponent = builder.build_left_shift(biased, ty.const_int(52, false), "mba.scale")?;
        let low = builder.build_and(seed, ty.const_int(0x1ffff, false), "mba.seed.range")?;
        let nonzero = builder.build_int_add(low, ty.const_int(1, false), "mba.seed.nonzero")?;
        let shifted = builder.build_left_shift(nonzero, ty.const_int(33, false), "mba.seed.shift")?;
        let guard = builder.build_or(shifted, exponent, "mba.guard")?;
        (exponent, guard)
    } else {
        // Constant construction needs no insertion point in integer-only code.
        let seed = seed.get_zero_extended_constant().unwrap();
        let exponent = (MIN_EXPONENT + ((seed >> 17) & 255)) << 52;
        (
            ty.const_int(exponent, false),
            ty.const_int(exponent | (((seed & 0x1ffff) + 1) << 33), false),
        )
    };
    let mut emitter = Region {
        ctx,
        builder,
        selected,
        encoded: HashMap::new(),
        // The scale is shared, but every primitive refreshes its mantissa
        // header using live operands. Phi/select carry each value's own header.
        exponent,
        guard,
        pre_expand: cfg.pre_expand,
    };
    // Phi placeholders break loop cycles before any recursive emission.
    for &inst in &planned {
        if emitter.selected.contains(&inst) && inst.get_opcode() == Op::Phi {
            emitter.builder.position_before(&inst);
            let phi = emitter.builder.build_phi(ctx.i64_type(), "mba.state")?;
            emitter.encoded.insert(inst, phi.as_basic_value().into_int_value());
        }
    }
    for &inst in &planned {
        if emitter.selected.contains(&inst) {
            emitter.emit(inst)?;
        }
    }
    for &inst in &planned {
        if !emitter.selected.contains(&inst) || inst.get_opcode() != Op::Phi {
            continue;
        }
        let phi = PhiValue::try_from(emitter.encoded[&inst].as_instruction_value().unwrap()).unwrap();
        let mut edge_values = HashMap::new();
        for (value, block) in PhiValue::try_from(inst).unwrap().get_incomings() {
            let anchor = block.get_terminator().unwrap();
            // Repeated switch edges must use the SAME value for one predecessor.
            let encoded = if let Some(&encoded) = edge_values.get(&(value, block)) {
                encoded
            } else {
                let encoded = emitter.input(value.into_int_value(), anchor)?;
                edge_values.insert((value, block), encoded);
                encoded
            };
            phi.add_incoming(&[(&encoded, block)]);
        }
    }
    let mut replacements = Vec::new();
    for &inst in &planned {
        let value = IntValue::try_from(inst).unwrap();
        let replacement = if let Some(&encoded) = emitter.encoded.get(&inst) {
            let anchor = if inst.get_opcode() == Op::Phi {
                inst.get_parent()
                    .unwrap()
                    .get_instructions()
                    .find(|i| i.get_opcode() != Op::Phi)
                    .unwrap()
            } else {
                inst
            };
            emitter.builder.position_before(&anchor);
            emitter
                .builder
                .build_int_truncate(encoded, value.get_type(), "mba.decode")?
        } else if binary(inst.get_opcode()) {
            emitter.builder.position_before(&inst);
            integer::emit(
                ctx,
                &emitter.builder,
                inst.get_opcode(),
                operand(inst, 0),
                operand(inst, 1),
            )?
        } else {
            continue;
        };
        replacements.push((inst, value, replacement));
    }
    // Replace all uses before erasing any source, including cyclic phi uses.
    // Decodes used solely by sources become dead and normal DCE removes them.
    for &(_, old, new) in &replacements {
        old.replace_all_uses_with(new);
    }
    let changed = !replacements.is_empty();
    for (inst, _, _) in replacements {
        inst.erase_from_basic_block();
    }
    Ok((changed, volatile))
}

struct Region<'ctx> {
    ctx: ContextRef<'ctx>,
    builder: Builder<'ctx>,
    selected: HashSet<InstructionValue<'ctx>>,
    encoded: HashMap<InstructionValue<'ctx>, IntValue<'ctx>>,
    exponent: IntValue<'ctx>,
    guard: IntValue<'ctx>,
    pre_expand: bool,
}

impl<'ctx> Region<'ctx> {
    fn input(&mut self, value: IntValue<'ctx>, anchor: InstructionValue<'ctx>) -> anyhow::Result<IntValue<'ctx>> {
        if let Some(inst) = value.as_instruction_value() {
            if self.selected.contains(&inst) {
                return self.emit(inst);
            }
        }
        self.builder.position_before(&anchor);
        // Keep the carrier in its finite range even when a poison result will
        // later be masked by select. Speculated FP must not raise exceptions.
        let value = integer::stable(&self.builder, value);
        let wide = self
            .builder
            .build_int_z_extend(value, self.ctx.i64_type(), "mba.payload")?;
        Ok(self.builder.build_or(wide, self.guard, "mba.encode")?)
    }

    fn emit(&mut self, inst: InstructionValue<'ctx>) -> anyhow::Result<IntValue<'ctx>> {
        if let Some(&value) = self.encoded.get(&inst) {
            return Ok(value);
        }
        let op = inst.get_opcode();
        let first = if op == Op::Select { 1 } else { 0 };
        let x = self.input(operand(inst, first), inst)?;
        let y = self.input(operand(inst, first + 1), inst)?;
        self.builder.position_before(&inst);
        let width = IntValue::try_from(inst).unwrap().get_type().get_bit_width();
        let result = if op == Op::Select {
            self.builder
                .build_select(integer::stable(&self.builder, operand(inst, 0)), x, y, "mba.select")?
                .into_int_value()
        } else if self.pre_expand {
            // Expand the *integer* identity graph and immediately lower each
            // node. No InstCombine can collapse the temporary integer DAG.
            // Inputs are encoded/frozen once and shared by every reference.
            let mut values = vec![x, y];
            for &algebra::Step(op, lhs, rhs) in algebra::recipe(op, rand::random()) {
                values.push(self.primitive(op, values[lhs], values[rhs], width)?);
            }
            *values.last().unwrap()
        } else {
            self.primitive(op, x, y, width)?
        };
        self.encoded.insert(inst, result);
        Ok(result)
    }

    fn primitive(&self, op: Op, x: IntValue<'ctx>, y: IntValue<'ctx>, width: u32) -> anyhow::Result<IntValue<'ctx>> {
        let b = &self.builder;
        let ty = self.ctx.i64_type();
        let mask = ty.const_int((1u64 << width) - 1, false);
        // Fold the predecessor header and current data together. The salt is
        // local to this generated node; no mutable global state is involved.
        let previous = b.build_right_shift(x, ty.const_int(33, false), false, "mba.header.previous")?;
        let left = b.build_xor(previous, x, "mba.header.left")?;
        let mixed = b.build_int_add(left, y, "mba.header.data")?;
        let salted = b.build_xor(
            mixed,
            ty.const_int(rand::random::<u64>() & 0x1ffff, false),
            "mba.header.salt",
        )?;
        let bounded = b.build_and(salted, ty.const_int(0x1ffff, false), "mba.header.range")?;
        let nonzero = b.build_int_add(bounded, ty.const_int(1, false), "mba.header.nonzero")?;
        let shifted = b.build_left_shift(nonzero, ty.const_int(33, false), "mba.header.shift")?;
        let guard = b.build_or(shifted, self.exponent, "mba.header")?;
        let bits = match op {
            Op::Add | Op::Sub => {
                let xf = b
                    .build_bit_cast(x, self.ctx.f64_type(), "mba.carrier")?
                    .into_float_value();
                let yf = b
                    .build_bit_cast(y, self.ctx.f64_type(), "mba.rhs.carrier")?
                    .into_float_value();
                // Different SSA values need not have the same header. Recover
                // this operand's origin instead of subtracting a shared constant.
                let origin_bits = b.build_and(y, ty.const_int(!((1u64 << width) - 1), false), "mba.rhs.header")?;
                let origin = b
                    .build_bit_cast(origin_bits, self.ctx.f64_type(), "mba.rhs.origin")?
                    .into_float_value();
                let payload = b.build_float_sub(yf, origin, "mba.rhs.payload")?;
                let result = if op == Op::Add {
                    b.build_float_add(xf, payload, "mba.add")?
                } else {
                    b.build_float_sub(xf, payload, "mba.sub")?
                };
                b.build_bit_cast(result, ty, "mba.bits")?.into_int_value()
            },
            Op::And => b.build_and(x, y, "mba.and")?,
            Op::Or => b.build_or(x, y, "mba.or")?,
            Op::Xor => b.build_xor(x, y, "mba.xor.payload")?,
            _ => unreachable!("planner admitted unsupported region opcode"),
        };
        // Boolean operands may also have different headers. Canonicalize all
        // operations before their values reach another FP node or loop edge.
        let low = b.build_and(bits, mask, "mba.canonical.payload")?;
        Ok(b.build_or(low, guard, "mba.canonical")?)
    }
}
