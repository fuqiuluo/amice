use crate::aotu::mba::config::{BitWidth, ConstantMbaConfig, NumberType, bits_to_signed};
use crate::aotu::mba::expr::Expr;
use rand::prelude::*;
use std::fmt;

pub fn rand_u128_mod2n<R: Rng + ?Sized>(rng: &mut R, bits: u32) -> u128 {
    // 生成 [0, 2^n) 的随机数（通过掩码到 n 位）
    let v: u128 = rng.random();
    if bits == 128 { v } else { v & ((1u128 << bits) - 1) }
}

// 基础恒等项：恒为 0
pub fn gen_base_zero_term<R: Rng + ?Sized>(rng: &mut R, aux_count: usize) -> Expr {
    let safe = aux_count.max(1);
    let a = rng.random_range(0..safe);
    match rng.random_range(0..3) {
        0 => Expr::And(Box::new(Expr::Var(a)), Box::new(Expr::Not(Box::new(Expr::Var(a))))), // a & ~a = 0
        1 => Expr::Xor(Box::new(Expr::Var(a)), Box::new(Expr::Var(a))),                      // a ^ a = 0
        _ => Expr::Sub(Box::new(Expr::Var(a)), Box::new(Expr::Var(a))),                      // a - a = 0
    }
}

// 基础恒等项：恒为 mask（全 1）
pub fn gen_base_mask_term<R: Rng + ?Sized>(rng: &mut R, aux_count: usize) -> Expr {
    let safe = aux_count.max(1);
    let a = rng.random_range(0..safe);
    match rng.random_range(0..3) {
        0 => Expr::Or(Box::new(Expr::Var(a)), Box::new(Expr::Not(Box::new(Expr::Var(a))))), // a | ~a = mask
        1 => Expr::Not(Box::new(Expr::Xor(Box::new(Expr::Var(a)), Box::new(Expr::Var(a))))), // ~(a ^ a) = mask
        _ => Expr::Not(Box::new(Expr::And(
            Box::new(Expr::Var(a)),
            Box::new(Expr::Not(Box::new(Expr::Var(a)))),
        ))), // ~(a & ~a) = mask
    }
}

// 等价重写（不改变表达式语义）
pub fn rewrite_once<R: Rng + ?Sized>(rng: &mut R, e: Expr, nmask: u128, aux_count: usize) -> Expr {
    use Expr::*;
    match rng.random_range(0..9) {
        // 9个安全的重写规则
        0 => Not(Box::new(Not(Box::new(e)))),          // ~~e = e
        1 => Xor(Box::new(e), Box::new(Const(0))),     // e ^ 0 = e
        2 => Or(Box::new(e), Box::new(Const(0))),      // e | 0 = e
        3 => And(Box::new(e), Box::new(Const(nmask))), // e & mask = e
        4 => {
            // 德摩根定律（保持等价）
            match e {
                And(a, b) => Not(Box::new(Or(Box::new(Not(a)), Box::new(Not(b))))),
                Or(a, b) => Not(Box::new(And(Box::new(Not(a)), Box::new(Not(b))))),
                other => other,
            }
        },
        5 => {
            // 交换律
            match e {
                And(a, b) => And(b, a),
                Or(a, b) => Or(b, a),
                Xor(a, b) => Xor(b, a),
                Add(a, b) => Add(b, a),
                other => other,
            }
        },
        6 => {
            // 加/减 0
            if rng.random_bool(0.5) {
                Add(Box::new(e), Box::new(Const(0)))
            } else {
                Sub(Box::new(e), Box::new(Const(0)))
            }
        },
        7 => {
            // e = e & mask（冗余但安全）
            And(Box::new(e), Box::new(Const(nmask)))
        },
        _ => {
            // e = e | 0（冗余但安全）
            Or(Box::new(e), Box::new(Const(0)))
        },
    }
}

pub fn rewrite_n<R: Rng + ?Sized>(rng: &mut R, e: Expr, depth: usize, nmask: u128, aux_count: usize) -> Expr {
    let mut cur = e;
    for _ in 0..depth {
        cur = rewrite_once(rng, cur, nmask, aux_count);
    }
    cur
}

pub fn gen_term<R: Rng + ?Sized>(rng: &mut R, aux_count: usize, want_mask: bool, depth: usize, nmask: u128) -> Expr {
    let base = if want_mask {
        gen_base_mask_term(rng, aux_count)
    } else {
        gen_base_zero_term(rng, aux_count)
    };
    rewrite_n(rng, base, depth, nmask, aux_count)
}

fn build_constant_mba<R: Rng + ?Sized>(rng: &mut R, cfg: &ConstantMbaConfig) -> Expr {
    let bits = cfg.width.bits();
    let nmask = cfg.width.mask_u128();
    let k = cfg.normalized_constant();

    let ops = cfg.rewrite_ops.max(2);

    // 随机初值 R，作为顶层常量（不暴露 K）
    let base = rand_u128_mod2n(rng, bits) & nmask;

    // (coeff, term)
    let mut terms: Vec<(u128, Expr)> = Vec::new();
    let mut mask_term_indices: Vec<usize> = Vec::new();

    // 生成项，尽量让 mask 项数量 >= 1
    for i in 0..ops {
        let want_mask = if i == ops - 1 {
            mask_term_indices.is_empty() || rng.random_bool(0.5)
        } else if mask_term_indices.is_empty() {
            rng.random_bool(0.75)
        } else {
            rng.random_bool(0.5)
        };

        let term = gen_term(rng, cfg.aux_count, want_mask, cfg.rewrite_depth, nmask);
        let coeff = rand_u128_mod2n(rng, bits) & nmask;

        if want_mask {
            mask_term_indices.push(terms.len());
        }
        terms.push((coeff, term));
    }

    // 若没有 mask 项，则补一个
    if mask_term_indices.is_empty() {
        let term = gen_term(rng, cfg.aux_count, true, cfg.rewrite_depth, nmask);
        let coeff = rand_u128_mod2n(rng, bits) & nmask;
        mask_term_indices.push(terms.len());
        terms.push((coeff, term));
    }

    // 目标：Σ mask 系数 ≡ (base - k) mod 2^n
    let target_sum = base.wrapping_sub(k) & nmask;
    let adjust_idx = *mask_term_indices.last().unwrap();

    let mut sum_others: u128 = 0;
    for &idx in &mask_term_indices {
        if idx == adjust_idx {
            continue;
        }
        sum_others = sum_others.wrapping_add(terms[idx].0);
    }
    sum_others &= nmask;

    let mut adjust_coeff = target_sum.wrapping_sub(sum_others) & nmask;
    terms[adjust_idx].0 = adjust_coeff;

    // 可选防泄漏：避免某个系数恰好等于 base 或 k（直观敏感值）
    if adjust_coeff == k || adjust_coeff == base {
        // 挑一个其它 mask 项做对冲；如仅一个，则补一个系数为 0 的 mask 项
        let other_idx = if mask_term_indices.len() >= 2 {
            mask_term_indices[mask_term_indices.len() - 2]
        } else {
            let term = gen_term(rng, cfg.aux_count, true, cfg.rewrite_depth, nmask);
            terms.push((0, term));
            let idx = terms.len() - 1;
            mask_term_indices.push(idx);
            idx
        };

        let mut delta: u128 = 1;
        if nmask > 1 {
            let r = rand_u128_mod2n(rng, bits) & nmask;
            delta = if r == 0 { 1 } else { r };
        }

        adjust_coeff = adjust_coeff.wrapping_add(delta) & nmask;
        let mut other_coeff = terms[other_idx].0.wrapping_sub(delta) & nmask;

        if adjust_coeff == k || adjust_coeff == base || other_coeff == k || other_coeff == base {
            adjust_coeff = adjust_coeff.wrapping_add(1) & nmask;
            other_coeff = other_coeff.wrapping_sub(1) & nmask;
        }

        terms[adjust_idx].0 = adjust_coeff;
        terms[other_idx].0 = other_coeff;
    }

    // 用随机初值 base 作为表达式初值
    let mut acc = Expr::Const(base);
    for (coeff, term) in terms {
        if coeff != 0 {
            let mt = Expr::MulConst(coeff, Box::new(term));
            acc = Expr::Add(Box::new(acc), Box::new(mt));
        }
    }
    acc
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use Expr::*;
        match self {
            Const(v) => write!(f, "{}", v),
            Var(i) => write!(f, "aux{}", i),
            Not(x) => write!(f, "(not {})", x),
            And(a, b) => write!(f, "(and {} {})", a, b),
            Or(a, b) => write!(f, "(or {} {})", a, b),
            Xor(a, b) => write!(f, "(xor {} {})", a, b),
            Add(a, b) => write!(f, "(add {} {})", a, b),
            Sub(a, b) => write!(f, "(sub {} {})", a, b),
            MulConst(c, x) => write!(f, "(mul {} {})", c, x),
        }
    }
}

pub(super) struct CPrinter {
    pub(crate) width: BitWidth,
    pub(crate) number_type: NumberType,
}

#[allow(dead_code)]
impl CPrinter {
    fn c_ty(&self) -> &'static str {
        self.width.c_type(self.number_type)
    }

    fn mask(&self) -> u128 {
        self.width.mask_u128()
    }

    fn const_c_u128_hex(v: u128) -> String {
        let hi = (v >> 64) as u64;
        let lo = (v & 0xFFFF_FFFF_FFFF_FFFFu128) as u64;
        if hi == 0 {
            format!("((__uint128_t)0x{lo:016x}ull)")
        } else {
            format!("((((__uint128_t)0x{hi:016x}ull) << 64) | ((__uint128_t)0x{lo:016x}ull))")
        }
    }

    fn const_c_i128_hex(v: u128) -> String {
        let hi = (v >> 64) as u64;
        let lo = (v & 0xFFFF_FFFF_FFFF_FFFFu128) as u64;
        if hi == 0 {
            format!("((__int128_t)0x{lo:016x}ull)")
        } else {
            format!("((((__int128_t)0x{hi:016x}ull) << 64) | ((__int128_t)0x{lo:016x}ull))")
        }
    }

    fn const_c(&self, v: u128) -> String {
        let vv = v & self.mask();
        match (self.width, self.number_type) {
            (BitWidth::W128, NumberType::Unsigned) => Self::const_c_u128_hex(vv),
            (BitWidth::W128, NumberType::Signed) => Self::const_c_i128_hex(vv),
            (_, NumberType::Unsigned) => format!("(({}) {})", self.c_ty(), vv),
            (_, NumberType::Signed) => {
                let signed_val = bits_to_signed(vv, self.width);
                format!("(({}) {})", self.c_ty(), signed_val)
            },
        }
    }

    fn print_expr(&self, e: &Expr) -> String {
        use Expr::*;
        match e {
            Const(v) => self.const_c(*v),
            Var(i) => format!("aux{}", i),
            Not(x) => format!("(~({}))", self.print_expr(x)),
            And(a, b) => format!("(({}) & ({}))", self.print_expr(a), self.print_expr(b)),
            Or(a, b) => format!("(({}) | ({}))", self.print_expr(a), self.print_expr(b)),
            Xor(a, b) => format!("(({}) ^ ({}))", self.print_expr(a), self.print_expr(b)),
            Add(a, b) => format!("(({}) + ({}))", self.print_expr(a), self.print_expr(b)),
            Sub(a, b) => format!("(({}) - ({}))", self.print_expr(a), self.print_expr(b)),
            MulConst(c, x) => {
                let cc = self.const_c(*c);
                format!("({} * ({}))", cc, self.print_expr(x))
            },
        }
    }

    pub fn emit_function(&self, func_name: &str, aux_count: usize, body: &Expr) -> String {
        let ret_ty = self.c_ty();
        let mut args = Vec::new();
        for i in 0..aux_count {
            args.push(format!("{} aux{}", ret_ty, i));
        }
        let arglist = if args.is_empty() {
            "void".to_string()
        } else {
            args.join(", ")
        };
        let expr = self.print_expr(body);
        format!("{} {}({}) {{\n\treturn {};\n}}", ret_ty, func_name, arglist, expr)
    }
}

pub(super) fn generate_const_mba(cfg: &ConstantMbaConfig) -> Expr {
    let mut rng: StdRng = match cfg.seed {
        Some(s) => StdRng::seed_from_u64(s),
        None => StdRng::from_os_rng(),
    };
    build_constant_mba(&mut rng, cfg)
}

// 评估表达式
pub(super) fn eval_const_mba_expr(expr: &Expr, aux_values: &[u128], width: BitWidth) -> u128 {
    use Expr::*;
    let mask = width.mask_u128();

    match expr {
        Const(v) => *v & mask,
        Var(i) => aux_values.get(*i).copied().unwrap_or(0) & mask,
        Not(x) => (!eval_const_mba_expr(x, aux_values, width)) & mask,
        And(a, b) => (eval_const_mba_expr(a, aux_values, width) & eval_const_mba_expr(b, aux_values, width)) & mask,
        Or(a, b) => (eval_const_mba_expr(a, aux_values, width) | eval_const_mba_expr(b, aux_values, width)) & mask,
        Xor(a, b) => (eval_const_mba_expr(a, aux_values, width) ^ eval_const_mba_expr(b, aux_values, width)) & mask,
        Add(a, b) => {
            (eval_const_mba_expr(a, aux_values, width).wrapping_add(eval_const_mba_expr(b, aux_values, width))) & mask
        },
        Sub(a, b) => {
            (eval_const_mba_expr(a, aux_values, width).wrapping_sub(eval_const_mba_expr(b, aux_values, width))) & mask
        },
        MulConst(c, x) => ((*c & mask).wrapping_mul(eval_const_mba_expr(x, aux_values, width))) & mask,
    }
}

// 验证MBA表达式是否返回预期常数
pub(super) fn verify_const_mba(expr: &Expr, expected: u128, width: BitWidth, aux_count: usize) -> bool {
    let expected = expected & width.mask_u128();

    // 测试一些随机的aux值组合
    let mut rng = StdRng::seed_from_u64(12345);
    for _ in 0..100 {
        let mut aux_values = vec![0u128; aux_count];
        for j in 0..aux_count {
            aux_values[j] = rand_u128_mod2n(&mut rng, width.bits());
        }

        let result = eval_const_mba_expr(expr, &aux_values, width);
        if result != expected {
            println!(
                "Verification failed: aux={:?}, expected={}, got={}",
                aux_values, expected, result
            );
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aotu::mba::config::signed_to_bits;

    #[test]
    fn test_signed_number_conversion() {
        // 测试有符号数转换
        assert_eq!(signed_to_bits(-1, BitWidth::W8), 0xFF);
        assert_eq!(signed_to_bits(-128, BitWidth::W8), 0x80);
        assert_eq!(signed_to_bits(127, BitWidth::W8), 0x7F);

        assert_eq!(bits_to_signed(0xFF, BitWidth::W8), -1);
        assert_eq!(bits_to_signed(0x80, BitWidth::W8), -128);
        assert_eq!(bits_to_signed(0x7F, BitWidth::W8), 127);
    }

    #[test]
    fn test_mba_signed_const() {
        let cfg = ConstantMbaConfig::new(
            BitWidth::W64,
            NumberType::Signed,
            2,
            24,
            3,
            "signed_function".to_string(),
        )
        .with_signed_constant(-114514);

        let ir = generate_const_mba(&cfg);
        println!("Signed IR:");
        println!("{}", ir);

        let is_valid = verify_const_mba(&ir, cfg.constant, cfg.width, cfg.aux_count);
        println!("\nSigned Verification: {}", if is_valid { "PASSED" } else { "FAILED" });

        let c = CPrinter {
            width: cfg.width,
            number_type: cfg.number_type,
        }
        .emit_function(&cfg.func_name, cfg.aux_count, &ir);
        println!("\nSigned C Code:");
        println!("{}", c);

        println!("Expected signed value: {}", cfg.get_signed_constant());
        println!("Expected unsigned bits: 0x{:x}", cfg.get_unsigned_constant());

        assert!(is_valid, "Generated signed MBA does not return the expected constant");
    }

    #[test]
    fn test_mba_unsigned_const() {
        let cfg = ConstantMbaConfig::new(
            BitWidth::W64,
            NumberType::Unsigned,
            2,
            24,
            3,
            "unsigned_function".to_string(),
        )
        .with_unsigned_constant(0xDEADBEEF);

        let ir = generate_const_mba(&cfg);
        println!("Unsigned IR:");
        println!("{}", ir);

        let is_valid = verify_const_mba(&ir, cfg.constant, cfg.width, cfg.aux_count);
        println!(
            "\nUnsigned Verification: {}",
            if is_valid { "PASSED" } else { "FAILED" }
        );

        let c = CPrinter {
            width: cfg.width,
            number_type: cfg.number_type,
        }
        .emit_function(&cfg.func_name, cfg.aux_count, &ir);
        println!("\nUnsigned C Code:");
        println!("{}", c);

        assert!(is_valid, "Generated unsigned MBA does not return the expected constant");
    }

    #[test]
    fn test_extreme_signed_values() {
        let test_cases = vec![
            (BitWidth::W8, i8::MIN as i128, "i8_min"),
            (BitWidth::W8, i8::MAX as i128, "i8_max"),
            (BitWidth::W16, i16::MIN as i128, "i16_min"),
            (BitWidth::W16, i16::MAX as i128, "i16_max"),
            (BitWidth::W32, i32::MIN as i128, "i32_min"),
            (BitWidth::W32, i32::MAX as i128, "i32_max"),
            (BitWidth::W64, i64::MIN as i128, "i64_min"),
            (BitWidth::W64, i64::MAX as i128, "i64_max"),
        ];

        for (width, signed_val, name) in test_cases {
            let cfg = ConstantMbaConfig::new(width, NumberType::Signed, 2, 24, 3, name.to_string())
                .with_signed_constant(signed_val);

            let ir = generate_const_mba(&cfg);
            let is_valid = verify_const_mba(&ir, cfg.constant, cfg.width, cfg.aux_count);

            println!(
                "// Testing {}: signed_val={}, bits=0x{:x}, verification={}",
                name,
                signed_val,
                cfg.constant,
                if is_valid { "PASSED" } else { "FAILED" }
            );

            let c = CPrinter {
                width: cfg.width,
                number_type: cfg.number_type,
            }
            .emit_function(&cfg.func_name, cfg.aux_count, &ir);
            println!("{}\n", c);

            assert!(
                is_valid,
                "Generated signed MBA for {} does not return the expected constant",
                name
            );
        }
    }

    #[test]
    fn test_extreme_unsigned_values() {
        let test_cases = vec![
            (BitWidth::W8, u8::MIN as u128, "u8_min"),
            (BitWidth::W8, u8::MAX as u128, "u8_max"),
            (BitWidth::W16, u16::MIN as u128, "u16_min"),
            (BitWidth::W16, u16::MAX as u128, "u16_max"),
            (BitWidth::W32, u32::MIN as u128, "u32_min"),
            (BitWidth::W32, u32::MAX as u128, "u32_max"),
            (BitWidth::W64, u64::MIN as u128, "u64_min"),
            (BitWidth::W64, u64::MAX as u128, "u64_max"),
            (BitWidth::W128, u128::MIN as u128, "u128_min"),
            (BitWidth::W128, u128::MAX as u128, "u128_max"),
        ];

        for (width, signed_val, name) in test_cases {
            let cfg = ConstantMbaConfig::new(width, NumberType::Unsigned, 2, 24, 3, name.to_string())
                .with_unsigned_constant(signed_val);

            let ir = generate_const_mba(&cfg);
            let is_valid = verify_const_mba(&ir, cfg.constant, cfg.width, cfg.aux_count);

            println!(
                "// Testing {}: unsigned_val={}, bits=0x{:x}, verification={}",
                name,
                signed_val,
                cfg.constant,
                if is_valid { "PASSED" } else { "FAILED" }
            );

            let c = CPrinter {
                width: cfg.width,
                number_type: cfg.number_type,
            }
            .emit_function(&cfg.func_name, cfg.aux_count, &ir);
            println!("{}\n", c);

            assert!(
                is_valid,
                "Generated unsigned MBA for {} does not return the expected constant",
                name
            );
        }
    }
}
