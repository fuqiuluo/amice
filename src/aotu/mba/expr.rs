#[derive(Clone, Debug)]
pub(super) enum Expr {
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
    pub(super) fn const0() -> Self {
        Expr::Const(0)
    }
}