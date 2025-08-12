use rand::prelude::*;
use std::fmt;

#[derive(Clone, Copy, Debug)]
enum BitWidth {
    W8,
    W16,
    W32,
    W64,
    W128,
}

impl BitWidth {
    fn from_bits(bits: u32) -> Option<Self> {
        match bits {
            8 => Some(BitWidth::W8),
            16 => Some(BitWidth::W16),
            32 => Some(BitWidth::W32),
            64 => Some(BitWidth::W64),
            128 => Some(BitWidth::W128),
            _ => None,
        }
    }

    fn bits(self) -> u32 {
        match self {
            BitWidth::W8 => 8,
            BitWidth::W16 => 16,
            BitWidth::W32 => 32,
            BitWidth::W64 => 64,
            BitWidth::W128 => 128,
        }
    }

    fn mask_u128(self) -> u128 {
        match self {
            BitWidth::W128 => u128::MAX,
            _ => (1u128 << self.bits()) - 1,
        }
    }

    fn c_type(&self) -> &'static str {
        match self {
            BitWidth::W8 => "uint8_t",
            BitWidth::W16 => "uint16_t",
            BitWidth::W32 => "uint32_t",
            BitWidth::W64 => "uint64_t",
            BitWidth::W128 => "__uint128_t",
        }
    }

    fn rust_type(&self) -> &'static str {
        match self {
            BitWidth::W8 => "u8",
            BitWidth::W16 => "u16",
            BitWidth::W32 => "u32",
            BitWidth::W64 => "u64",
            BitWidth::W128 => "u128",
        }
    }
}

#[derive(Clone, Debug)]
enum Expr {
    Const(u128),
    Var(usize), // aux index: aux0, aux1, ...
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Xor(Box<Expr>, Box<Expr>),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    MulConst(u128, Box<Expr>), // c * expr（按位宽溢出）
}

impl Expr {
    fn const0() -> Self { Expr::Const(0) }
}

#[derive(Clone, Debug)]
struct MbaConfig {
    width: BitWidth,
    aux_count: usize,
    rewrite_ops: usize,
    rewrite_depth: usize,
    constant: u128, // desired constant (mod 2^n)
    seed: Option<u64>,
    func_name: String,
}

impl MbaConfig {
    fn normalized_constant(&self) -> u128 {
        self.constant & self.width.mask_u128()
    }
}

fn rand_u128_mod2n<R: Rng + ?Sized>(rng: &mut R, bits: u32) -> u128 {
    // 生成 [0, 2^n) 的随机数（通过掩码到 n 位）
    let v: u128 = rng.random();
    if bits == 128 {
        v
    } else {
        v & ((1u128 << bits) - 1)
    }
}

// 基础恒等项：恒为 0
fn gen_base_zero_term<R: Rng + ?Sized>(rng: &mut R, aux_count: usize) -> Expr {
    let safe = aux_count.max(1);
    let a = rng.random_range(0..safe);
    match rng.random_range(0..3) {
        0 => Expr::And(Box::new(Expr::Var(a)), Box::new(Expr::Not(Box::new(Expr::Var(a))))), // a & ~a = 0
        1 => Expr::Xor(Box::new(Expr::Var(a)), Box::new(Expr::Var(a))), // a ^ a = 0
        _ => Expr::Sub(Box::new(Expr::Var(a)), Box::new(Expr::Var(a))), // a - a = 0
    }
}

// 基础恒等项：恒为 mask（全 1）
fn gen_base_mask_term<R: Rng + ?Sized>(rng: &mut R, aux_count: usize) -> Expr {
    let safe = aux_count.max(1);
    let a = rng.random_range(0..safe);
    match rng.random_range(0..3) {
        0 => Expr::Or(Box::new(Expr::Var(a)), Box::new(Expr::Not(Box::new(Expr::Var(a))))), // a | ~a = mask
        1 => Expr::Not(Box::new(Expr::Xor(Box::new(Expr::Var(a)), Box::new(Expr::Var(a))))), // ~(a ^ a) = mask
        _ => Expr::Not(Box::new(Expr::And(Box::new(Expr::Var(a)), Box::new(Expr::Not(Box::new(Expr::Var(a))))))), // ~(a & ~a) = mask
    }
}

// 等价重写（不改变表达式语义）
fn rewrite_once<R: Rng + ?Sized>(rng: &mut R, e: Expr, nmask: u128, aux_count: usize) -> Expr {
    use Expr::*;
    match rng.random_range(0..9) {  // 9个安全的重写规则
        0 => Not(Box::new(Not(Box::new(e)))),         // ~~e = e
        1 => Xor(Box::new(e), Box::new(Const(0))),    // e ^ 0 = e
        2 => Or(Box::new(e), Box::new(Const(0))),     // e | 0 = e
        3 => And(Box::new(e), Box::new(Const(nmask))),// e & mask = e
        4 => { // 德摩根定律（保持等价）
            match e {
                And(a, b) => Not(Box::new(Or(Box::new(Not(a)), Box::new(Not(b))))),
                Or(a, b)  => Not(Box::new(And(Box::new(Not(a)), Box::new(Not(b))))),
                other => other,
            }
        }
        5 => { // 交换律
            match e {
                And(a, b) => And(b, a),
                Or(a, b)  => Or(b, a),
                Xor(a, b) => Xor(b, a),
                Add(a, b) => Add(b, a),
                other => other,
            }
        }
        6 => { // 加/减 0
            if rng.random_bool(0.5) {
                Add(Box::new(e), Box::new(Const(0)))
            } else {
                Sub(Box::new(e), Box::new(Const(0)))
            }
        }
        7 => { // e = e & mask（冗余但安全）
            And(Box::new(e), Box::new(Const(nmask)))
        }
        _ => { // e = e | 0（冗余但安全）
            Or(Box::new(e), Box::new(Const(0)))
        }
    }
}

fn rewrite_n<R: Rng + ?Sized>(rng: &mut R, e: Expr, depth: usize, nmask: u128, aux_count: usize) -> Expr {
    let mut cur = e;
    for _ in 0..depth {
        cur = rewrite_once(rng, cur, nmask, aux_count);
    }
    cur
}

fn gen_term<R: Rng + ?Sized>(rng: &mut R, aux_count: usize, want_mask: bool, depth: usize, nmask: u128) -> Expr {
    let base = if want_mask {
        gen_base_mask_term(rng, aux_count)
    } else {
        gen_base_zero_term(rng, aux_count)
    };
    rewrite_n(rng, base, depth, nmask, aux_count)
}

fn build_constant_mba<R: Rng + ?Sized>(rng: &mut R, cfg: &MbaConfig) -> Expr {
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
        if idx == adjust_idx { continue; }
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

struct CPrinter {
    width: BitWidth,
}

impl CPrinter {
    fn c_ty(&self) -> &'static str { self.width.c_type() }
    fn mask(&self) -> u128 { self.width.mask_u128() }

    fn const_c_u128_hex(v: u128) -> String {
        let hi = (v >> 64) as u64;
        let lo = (v & 0xFFFF_FFFF_FFFF_FFFFu128) as u64;
        if hi == 0 {
            format!("((__uint128_t)0x{lo:016x}ull)")
        } else {
            format!("((((__uint128_t)0x{hi:016x}ull) << 64) | ((__uint128_t)0x{lo:016x}ull))")
        }
    }

    fn const_c(&self, v: u128) -> String {
        let vv = v & self.mask();
        match self.width {
            BitWidth::W128 => Self::const_c_u128_hex(vv),
            _ => format!("(({}) {})", self.c_ty(), vv),
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
            }
        }
    }

    fn emit_function(&self, func_name: &str, aux_count: usize, body: &Expr) -> String {
        let ret_ty = self.c_ty();
        let mut args = Vec::new();
        for i in 0..aux_count {
            args.push(format!("{} aux{}", ret_ty, i));
        }
        let arglist = if args.is_empty() { "void".to_string() } else { args.join(", ") };
        let expr = self.print_expr(body);
        format!(
            "{} {}({}) {{\n\treturn {};\n}}",
            ret_ty, func_name, arglist, expr
        )
    }
}

fn generate_mba(cfg: &MbaConfig) -> Expr {
    let mut rng: StdRng = match cfg.seed {
        Some(s) => StdRng::seed_from_u64(s),
        None => StdRng::from_os_rng(),
    };
    build_constant_mba(&mut rng, cfg)
}

// 评估表达式
fn eval_expr(expr: &Expr, aux_values: &[u128], width: BitWidth) -> u128 {
    use Expr::*;
    let mask = width.mask_u128();

    match expr {
        Const(v) => *v & mask,
        Var(i) => aux_values.get(*i).copied().unwrap_or(0) & mask,
        Not(x) => (!eval_expr(x, aux_values, width)) & mask,
        And(a, b) => (eval_expr(a, aux_values, width) & eval_expr(b, aux_values, width)) & mask,
        Or(a, b) => (eval_expr(a, aux_values, width) | eval_expr(b, aux_values, width)) & mask,
        Xor(a, b) => (eval_expr(a, aux_values, width) ^ eval_expr(b, aux_values, width)) & mask,
        Add(a, b) => (eval_expr(a, aux_values, width).wrapping_add(eval_expr(b, aux_values, width))) & mask,
        Sub(a, b) => (eval_expr(a, aux_values, width).wrapping_sub(eval_expr(b, aux_values, width))) & mask,
        MulConst(c, x) => ((*c & mask).wrapping_mul(eval_expr(x, aux_values, width))) & mask,
    }
}

// 验证MBA表达式是否返回预期常数
fn verify_mba(expr: &Expr, expected: u128, width: BitWidth, aux_count: usize) -> bool {
    let expected = expected & width.mask_u128();

    // 测试一些随机的aux值组合
    let mut rng = StdRng::seed_from_u64(12345);
    for _ in 0..100 {
        let mut aux_values = vec![0u128; aux_count];
        for j in 0..aux_count {
            aux_values[j] = rand_u128_mod2n(&mut rng, width.bits());
        }

        let result = eval_expr(expr, &aux_values, width);
        if result != expected {
            println!("Verification failed: aux={:?}, expected={}, got={}", aux_values, expected, result);
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mba_const() {
        let cfg = MbaConfig {
            width: BitWidth::W64,
            aux_count: 2,
            rewrite_ops: 24,
            rewrite_depth: 3,
            constant: 56645,
            seed: None,
            func_name: "f".to_string(),
        };

        let ir = generate_mba(&cfg);
        println!("IR:");
        println!("{}", ir);

        // 验证MBA是否正确返回期望的常数
        let is_valid = verify_mba(&ir, cfg.constant, cfg.width, cfg.aux_count);
        println!("\nVerification: {}", if is_valid { "PASSED" } else { "FAILED" });

        let c = CPrinter { width: cfg.width }.emit_function(&cfg.func_name, cfg.aux_count, &ir);
        println!("\nC Code:");
        println!("{}", c);

        assert!(is_valid, "Generated MBA does not return the expected constant");
    }

    #[test]
    fn test_mba_const_all_bits() {
        let mut cfg = MbaConfig {
            width: BitWidth::W8,
            aux_count: 2,
            rewrite_ops: 24,
            rewrite_depth: 3,
            constant: 0,
            seed: None,
            func_name: "f".to_string(),
        };

        let all_bits = [BitWidth::W8, BitWidth::W16, BitWidth::W32, BitWidth::W64, BitWidth::W128];
        for bits in all_bits {
            cfg.width = bits;
            let mask = bits.mask_u128();
            match bits {
                BitWidth::W8 | BitWidth::W16 => {
                    // 小位宽做全量验证
                    for c in 0..=mask {
                        cfg.constant = c;
                        let ir = generate_mba(&cfg);
                        let is_valid = verify_mba(&ir, cfg.constant, cfg.width, cfg.aux_count);
                        assert!(is_valid, "width={:?}, constant={} failed", bits, c);
                    }
                }
                BitWidth::W32 => {
                    let mut u32_samples = [0u32; 10000];
                    for i in 0..u32_samples.len() {
                        u32_samples[i] = rand::random::<u32>() & (mask as u32);
                    }
                    for i in 0..u32_samples.len() {
                        cfg.constant = u32_samples[i] as u128;
                        let ir = generate_mba(&cfg);
                        let is_valid = verify_mba(&ir, cfg.constant, cfg.width, cfg.aux_count);
                        assert!(is_valid, "width={:?}, constant={} failed", bits, u32_samples[i]);
                    }
                }
                BitWidth::W64 => {
                    let mut u64_samples = [0u64; 10000];
                    for i in 0..u64_samples.len() {
                        u64_samples[i] = rand::random::<u64>() & (mask as u64);
                    }
                    for i in 0..u64_samples.len() {
                        cfg.constant = u64_samples[i] as u128;
                        let ir = generate_mba(&cfg);
                        let is_valid = verify_mba(&ir, cfg.constant, cfg.width, cfg.aux_count);
                        assert!(is_valid, "width={:?}, constant={} failed", bits, u64_samples[i]);
                    }

                    let u64_samples = [
                        114514u64,
                        1919810u64,
                        263283829,
                        0xdeadbeaf,
                        0xfaceb00c,
                        0x13371337,
                        0x123456789abcdef,
                        0x123456789abcdef0,
                        0x123456789abcdef5,
                        0x911566,
                        0x20250812,
                    ];
                    for i in 0..u64_samples.len() {
                        cfg.constant = u64_samples[i] as u128;
                        let ir = generate_mba(&cfg);
                        let is_valid = verify_mba(&ir, cfg.constant, cfg.width, cfg.aux_count);
                        assert!(is_valid, "width={:?}, constant={} failed", bits, u64_samples[i]);
                    }
                }
                BitWidth::W128 => {
                    let mut u128_samples = [0u128; 10000];
                    for i in 0..u128_samples.len() {
                        u128_samples[i] = rand::random::<u128>() & mask;
                    }
                    for i in 0..u128_samples.len() {
                        cfg.constant = u128_samples[i];
                        let ir = generate_mba(&cfg);
                    }
                }
            }
        }
    }

    #[test]
    fn test_signed_number() {
        fn bit_cast_to_unsigned(value: i128, width: BitWidth) -> u128 {
            (value as u128) & width.mask_u128()
        }

        let mut cfg = MbaConfig {
            width: BitWidth::W64,
            aux_count: 2,
            rewrite_ops: 24,
            rewrite_depth: 3,
            constant: 0,
            seed: None,
            func_name: "f".to_string(),
        };

        let value = -114514;
        let bit_cast = bit_cast_to_unsigned(value, cfg.width);
        cfg.constant = bit_cast;

        let ir = generate_mba(&cfg);
        println!("IR:");
        println!("{}", ir);

        let is_valid = verify_mba(&ir, cfg.constant, cfg.width, cfg.aux_count);
        println!("\nVerification: {}", if is_valid { "PASSED" } else { "FAILED" });

        let c = CPrinter { width: cfg.width }.emit_function(&cfg.func_name, cfg.aux_count, &ir);
        println!("\nC Code:");
        println!("{}", c);
    }

    #[test]
    fn test_mba_minmax() {
        let mut cfg = MbaConfig {
            width: BitWidth::W8,
            aux_count: 2,
            rewrite_ops: 24,
            rewrite_depth: 3,
            constant: (i8::MIN as u128),
            seed: None,
            func_name: "f".to_string(),
        };

        let pairs = vec![
            ("i8_min".to_string(), i8::MIN as u128, BitWidth::W8),
            ("i8_max".to_string(), i8::MAX as u128, BitWidth::W8),
            ("i16_min".to_string(), i16::MIN as u128, BitWidth::W16),
            ("i16_max".to_string(), i16::MAX as u128, BitWidth::W16),
            ("i32_min".to_string(), i32::MIN as u128, BitWidth::W32),
            ("i32_max".to_string(), i32::MAX as u128, BitWidth::W32),
            ("i64_min".to_string(), i64::MIN as u128, BitWidth::W64),
            ("i64_max".to_string(), i64::MAX as u128, BitWidth::W64),
            ("i128_min".to_string(), i128::MIN as u128, BitWidth::W128),
            ("i128_max".to_string(), i128::MAX as u128, BitWidth::W128),
            ("u8_min".to_string(), u8::MIN as u128, BitWidth::W8),
            ("u8_max".to_string(), u8::MAX as u128, BitWidth::W8),
            ("u16_min".to_string(), u16::MIN as u128, BitWidth::W16),
            ("u16_max".to_string(), u16::MAX as u128, BitWidth::W16),
            ("u32_min".to_string(), u32::MIN as u128, BitWidth::W32),
            ("u32_max".to_string(), u32::MAX as u128, BitWidth::W32),
            ("u64_min".to_string(), u64::MIN as u128, BitWidth::W64),
            ("u64_max".to_string(), u64::MAX as u128, BitWidth::W64),
            ("u128_min".to_string(), u128::MIN as u128, BitWidth::W128),
            ("u128_max".to_string(), u128::MAX as u128, BitWidth::W128),
        ];

        for (name, constant, width) in pairs {
            cfg.constant = constant;
            cfg.width = width;
            cfg.func_name = name;

            let ir = generate_mba(&cfg);
            let is_valid = verify_mba(&ir, cfg.constant, cfg.width, cfg.aux_count);
            println!("// Verification: {}", if is_valid { "PASSED" } else { "FAILED" });
            let c = CPrinter { width: cfg.width }.emit_function(&cfg.func_name, cfg.aux_count, &ir);
            println!("// C Code:");
            println!("{}", c);

            assert!(is_valid, "Generated MBA does not return the expected constant");
        }
    }
}