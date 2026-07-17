//! Vire-AST. Rein syntaktisch (kein Typwissen — das setzt später `infer`). Knoten
//! tragen Spans für Diagnosen und Debug-Info.

use crate::diag::Span;

#[derive(Debug, Clone)]
pub struct Module {
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub enum Item {
    Fn(FnDef),
    Type(TypeDef),
    Trait(TraitDef),
    Impl(ImplDef),
    Const { name: String, value: Expr, span: Span },
    Use { path: Vec<String>, span: Span },
    Extern { abi: String, items: Vec<FnSig>, span: Span },
}

#[derive(Debug, Clone)]
pub struct FnSig {
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FnDef {
    pub sig: FnSig,
    /// `= expr` (Ausdrucksfunktion) oder `{ … }` (Block). None = nur Signatur.
    pub body: Option<Block>,
    pub is_pub: bool,
}

#[derive(Debug, Clone)]
pub struct GenericParam {
    pub name: String,
    pub is_comptime: bool,
    /// Trait-Schranke(n), z.B. `T: Ord + Hash`.
    pub bounds: Vec<String>,
    /// Bei `comptime N: Int` der Typ.
    pub ty: Option<Type>,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Option<Type>,
    pub default: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct TypeDef {
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<Field>,
    pub variants: Vec<Variant>,
    pub methods: Vec<FnDef>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub struct Variant {
    pub name: String,
    /// Felder der Variante (leer bei datenloser Variante).
    pub fields: Vec<Field>,
    /// True, wenn positional (`Circle(radius: Float)` vs. nur Typen).
    pub positional: bool,
}

#[derive(Debug, Clone)]
pub struct TraitDef {
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub methods: Vec<FnDef>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ImplDef {
    pub trait_name: Option<String>,
    pub for_type: Type,
    pub methods: Vec<FnDef>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Type {
    pub name: String,
    pub args: Vec<Type>,
    /// `&T` geborgt.
    pub borrowed: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    /// Letzter Ausdruck = Blockwert (falls kein terminierendes `;`).
    pub tail: Option<Box<Expr>>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    /// `[mut] name [= expr]`
    Let { mutable: bool, name: String, value: Option<Expr>, span: Span },
    /// `lhs op= rhs` bzw. `lhs = rhs`
    Assign { target: Expr, op: Option<BinOp>, value: Expr, span: Span },
    Expr(Expr),
    Return(Option<Expr>, Span),
    Break(Span),
    Continue(Span),
    While { cond: Expr, body: Block, span: Span },
    For { pat: Pattern, iter: Expr, body: Block, span: Span },
}

#[derive(Debug, Clone)]
pub enum Expr {
    Int(i128, Span),
    Float(f64, Span),
    Str(String, Span),
    Char(char, Span),
    Bool(bool, Span),
    Ident(String, Span),
    SelfExpr(Span),
    Unary { op: UnOp, rhs: Box<Expr>, span: Span },
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr>, span: Span },
    Call { callee: Box<Expr>, args: Vec<Expr>, span: Span },
    Field { base: Box<Expr>, name: String, span: Span },
    Index { base: Box<Expr>, index: Box<Expr>, span: Span },
    /// Konstruktor/Call mit Typargumenten oder Generics-Anwendung: hier als Call.
    If { cond: Box<Expr>, then: Block, elifs: Vec<(Expr, Block)>, els: Option<Block>, span: Span },
    Match { scrutinee: Box<Expr>, arms: Vec<(Pattern, Option<Expr>, Expr)>, span: Span },
    Block(Block),
    Lambda { params: Vec<String>, body: Box<Expr>, span: Span },
    List(Vec<Expr>, Span),
    /// `expr?` — Fehler-Propagation.
    Try { inner: Box<Expr>, span: Span },
    /// `expr as Type`
    Cast { inner: Box<Expr>, ty: Type, span: Span },
    /// `comptime <expr>` / `comptime { … }`
    Comptime { inner: Box<Expr>, span: Span },
    /// `a..b` (exklusiv) / `a..=b` (inklusiv)
    Range { start: Box<Expr>, end: Box<Expr>, inclusive: bool, span: Span },
    /// `capsule(a, b) { … }` — isolierter Arena-Scope: nur `inputs` rein, nur der
    /// Blockwert raus (tief kopiert), Inneres RC-/Kollektor-frei (eigene Arena).
    /// `&`-markierte Inputs sind geborgt/read-only (keine Kopie). Siehe
    /// sprache/CAPSULE-BEWERTUNG.md.
    Capsule { inputs: Vec<(String, bool)>, body: Block, span: Span },
}

#[derive(Debug, Clone)]
pub enum Pattern {
    Wildcard(Span),
    Bind(String, Span),
    Int(i128, Span),
    Str(String, Span),
    Bool(bool, Span),
    /// `Variant(p, p, …)` bzw. `Variant`
    Ctor { name: String, args: Vec<Pattern>, span: Span },
    /// `(p, p, …)`
    Tuple(Vec<Pattern>, Span),
    Or(Vec<Pattern>, Span),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add, Sub, Mul, Div, Rem,
    AddWrap, SubWrap, MulWrap,
    Eq, Ne, Lt, Le, Gt, Ge,
    And, Or,
    BitAnd, BitOr, BitXor, Shl, Shr,
}
