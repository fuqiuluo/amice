use llvm_plugin::inkwell::context::ContextRef;
use llvm_plugin::inkwell::types::IntType;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(super) enum BitWidth {
    W8,
    W16,
    W32,
    W64,
    W128,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum NumberType {
    Unsigned,
    Signed,
}

impl BitWidth {
    pub fn from_bits(bits: u32) -> Option<Self> {
        match bits {
            8 => Some(BitWidth::W8),
            16 => Some(BitWidth::W16),
            32 => Some(BitWidth::W32),
            64 => Some(BitWidth::W64),
            128 => Some(BitWidth::W128),
            _ => None,
        }
    }

    pub fn bits(self) -> u32 {
        match self {
            BitWidth::W8 => 8,
            BitWidth::W16 => 16,
            BitWidth::W32 => 32,
            BitWidth::W64 => 64,
            BitWidth::W128 => 128,
        }
    }

    pub fn mask_u128(self) -> u128 {
        match self {
            BitWidth::W128 => u128::MAX,
            _ => (1u128 << self.bits()) - 1,
        }
    }

    pub fn c_type(&self, number_type: NumberType) -> &'static str {
        match (self, number_type) {
            (BitWidth::W8, NumberType::Unsigned) => "uint8_t",
            (BitWidth::W8, NumberType::Signed) => "int8_t",
            (BitWidth::W16, NumberType::Unsigned) => "uint16_t",
            (BitWidth::W16, NumberType::Signed) => "int16_t",
            (BitWidth::W32, NumberType::Unsigned) => "uint32_t",
            (BitWidth::W32, NumberType::Signed) => "int32_t",
            (BitWidth::W64, NumberType::Unsigned) => "uint64_t",
            (BitWidth::W64, NumberType::Signed) => "int64_t",
            (BitWidth::W128, NumberType::Unsigned) => "__uint128_t",
            (BitWidth::W128, NumberType::Signed) => "__int128_t",
        }
    }

    pub fn rust_type(&self, number_type: NumberType) -> &'static str {
        match (self, number_type) {
            (BitWidth::W8, NumberType::Unsigned) => "u8",
            (BitWidth::W8, NumberType::Signed) => "i8",
            (BitWidth::W16, NumberType::Unsigned) => "u16",
            (BitWidth::W16, NumberType::Signed) => "i16",
            (BitWidth::W32, NumberType::Unsigned) => "u32",
            (BitWidth::W32, NumberType::Signed) => "i32",
            (BitWidth::W64, NumberType::Unsigned) => "u64",
            (BitWidth::W64, NumberType::Signed) => "i64",
            (BitWidth::W128, NumberType::Unsigned) => "u128",
            (BitWidth::W128, NumberType::Signed) => "i128",
        }
    }

    // 获取有符号数的最大值
    pub fn signed_max(self) -> u128 {
        match self {
            BitWidth::W8 => i8::MAX as u128,
            BitWidth::W16 => i16::MAX as u128,
            BitWidth::W32 => i32::MAX as u128,
            BitWidth::W64 => i64::MAX as u128,
            BitWidth::W128 => i128::MAX as u128,
        }
    }

    // 获取有符号数的最小值的位模式
    pub fn signed_min_bits(self) -> u128 {
        match self {
            BitWidth::W8 => i8::MIN as u8 as u128,
            BitWidth::W16 => i16::MIN as u16 as u128,
            BitWidth::W32 => i32::MIN as u32 as u128,
            BitWidth::W64 => i64::MIN as u64 as u128,
            BitWidth::W128 => i128::MIN as u128,
        }
    }

    pub(crate) fn to_llvm_int_type<'ctx>(&self, context: ContextRef<'ctx>) -> IntType<'ctx> {
        match self {
            BitWidth::W8 => context.i8_type(),
            BitWidth::W16 => context.i16_type(),
            BitWidth::W32 => context.i32_type(),
            BitWidth::W64 => context.i64_type(),
            BitWidth::W128 => context.i128_type(),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct ConstantMbaConfig {
    pub(crate) width: BitWidth,
    pub(crate) number_type: NumberType,
    pub(crate) aux_count: usize,
    pub(crate) rewrite_ops: usize,
    pub(crate) rewrite_depth: usize,
    pub(crate) constant: u128, // desired constant (mod 2^n)，存储为位模式
    pub(crate) seed: Option<u64>,
    pub(crate) func_name: String,
}

impl ConstantMbaConfig {
    pub fn new(
        width: BitWidth,
        number_type: NumberType,
        aux_count: usize,
        rewrite_ops: usize,
        rewrite_depth: usize,
        func_name: String,
    ) -> Self {
        ConstantMbaConfig {
            width,
            number_type,
            aux_count,
            rewrite_ops,
            rewrite_depth,
            constant: 0,
            seed: None,
            func_name,
        }
    }

    pub(crate) fn normalized_constant(&self) -> u128 {
        self.constant & self.width.mask_u128()
    }

    // 从有符号整数创建配置
    pub fn with_signed_constant(mut self, signed_value: i128) -> Self {
        self.number_type = NumberType::Signed;
        self.constant = signed_to_bits(signed_value, self.width);
        self
    }

    // 从无符号整数创建配置
    pub fn with_unsigned_constant(mut self, unsigned_value: u128) -> Self {
        self.number_type = NumberType::Unsigned;
        self.constant = unsigned_value & self.width.mask_u128();
        self
    }

    // 获取常数的有符号解释
    pub(crate) fn get_signed_constant(&self) -> i128 {
        bits_to_signed(self.constant, self.width)
    }

    // 获取常数的无符号解释
    pub(crate) fn get_unsigned_constant(&self) -> u128 {
        self.constant & self.width.mask_u128()
    }
}

// 将有符号数转换为位模式
pub fn signed_to_bits(value: i128, width: BitWidth) -> u128 {
    let mask = width.mask_u128();
    (value as u128) & mask
}

// 将位模式转换为有符号数
pub fn bits_to_signed(bits: u128, width: BitWidth) -> i128 {
    let sign_bit = 1u128 << (width.bits() - 1);
    let mask = width.mask_u128();
    let value = bits & mask;

    if value & sign_bit != 0 {
        // 负数，需要符号扩展
        let extended = value | (!mask);
        extended as i128
    } else {
        // 正数
        value as i128
    }
}
