//! Vire AST. Purely syntactic (no type knowledge — that comes later via `infer`).
//! Nodes carry spans for diagnostics and debug info.

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
    /// `extern "C" [header "h.h"] [link "lib"]* { fn … }` — foreign functions.
    /// With `header`: the signatures are generated at compile time from the C header
    /// (auto-bindgen), no `{}` block needed.
    Extern { abi: String, items: Vec<FnSig>, links: Vec<String>, header: Option<String>, span: Span },
    /// `native "c++" [link "lib"]* """ …raw code… """` — embedded foreign code
    /// that is automatically compiled and linked alongside (no extra file/flag).
    Native { abi: String, code: String, links: Vec<String>, span: Span },
    /// `macro name(p, …) = <expr>` — hygienic expression macro. Applied before
    /// type inference via AST→AST expansion: parameters are replaced by the
    /// argument subtrees, macro-local bindings are gensym-renamed
    /// (hygiene). See `expand.rs`.
    Macro { name: String, params: Vec<String>, body: Expr, span: Span },
    /// `macro name(P: type, n: ident, e: expr) { <items> }` — a **hygienic item
    /// macro** producing declarations. Parameters are *kind-typed* (`type`/`ident`/
    /// `expr`); the expander checks each argument against its declared kind, so an
    /// expression can never be spliced where a type is expected (no C-preprocessor
    /// blind substitution). See `itemmacro.rs`.
    ItemMacro { name: String, params: Vec<MacroParam>, items: Vec<Item>, span: Span },
    /// `name!(args)` — invoke an item macro at item position, expanding to the
    /// macro's (substituted, hygienic) items.
    MacroInvoke { name: String, args: Vec<Expr>, span: Span },
    /// `cxx [link "lib"]* """preamble""" { fn sig = "c++ body" … }` — C++ bridge
    /// generator: for each `fn` an `extern "C"` trampoline is generated (with a
    /// C++ body) that is compiled/linked via the `native "c++"` path; the
    /// signatures are registered as Vire `extern`. Saves the
    /// handwritten facade. See language/CPP-INTEROP.md.
    Cxx { links: Vec<String>, preamble: String, fns: Vec<(FnSig, String)>, span: Span },
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
    /// `= expr` (expression function) or `{ … }` (block). None = signature only.
    pub body: Option<Block>,
    pub is_pub: bool,
    /// Declaration attributes, e.g. `@when(linux)` (platform conditional compilation).
    pub attrs: Vec<Attr>,
}

#[derive(Debug, Clone)]
pub struct GenericParam {
    pub name: String,
    pub is_comptime: bool,
    /// Trait bound(s), e.g. `T: Ord + Hash`.
    pub bounds: Vec<String>,
    /// The type in the case of `comptime N: Int`.
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
    /// Attributes attached to the declaration, e.g. `@derive(Eq, Show)`.
    pub attrs: Vec<Attr>,
    pub span: Span,
}

/// A declaration attribute: `@name(arg, arg, …)` (args are bare identifiers).
/// Drives the compile-time programming layer (currently `@derive`).
#[derive(Debug, Clone)]
pub struct Attr {
    pub name: String,
    pub args: Vec<String>,
    pub span: Span,
}

/// Kind of an item-macro parameter — what the argument is allowed to be. The
/// expander enforces this, which is what keeps item macros type-safe (an `expr`
/// cannot be used where a `type` is required).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    Type,
    Ident,
    Expr,
}

/// A kind-typed item-macro parameter, e.g. `T: type`.
#[derive(Debug, Clone)]
pub struct MacroParam {
    pub name: String,
    pub kind: ParamKind,
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
    /// Fields of the variant (empty for a dataless variant).
    pub fields: Vec<Field>,
    /// True if positional (`Circle(radius: Float)` vs. types only).
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
    /// `&T` borrowed.
    pub borrowed: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    /// Last expression = block value (if there is no terminating `;`).
    pub tail: Option<Box<Expr>>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    /// `[mut] name [: Type] [= expr]` — the optional type annotation is an escape
    /// hatch for cases the monomorphic unifier can't reach (e.g. binding an object
    /// whose class the RHS doesn't carry), so `n.field` still resolves.
    Let { mutable: bool, name: String, ty: Option<Type>, value: Option<Expr>, span: Span },
    /// `lhs op= rhs` or `lhs = rhs`
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
    /// Turbofish call `f[T, N](args)`: explicit generic arguments. Each `targ` is
    /// a type name (`Expr::Ident`) or a comptime value (`Expr::Int`). Used to bind
    /// value generics `[comptime N: Int]` at the call site.
    TurboCall { callee: String, targs: Vec<Expr>, args: Vec<Expr>, span: Span },
    Field { base: Box<Expr>, name: String, span: Span },
    Index { base: Box<Expr>, index: Box<Expr>, span: Span },
    /// Constructor/call with type arguments or generics application: here as a call.
    If { cond: Box<Expr>, then: Block, elifs: Vec<(Expr, Block)>, els: Option<Block>, span: Span },
    Match { scrutinee: Box<Expr>, arms: Vec<(Pattern, Option<Expr>, Expr)>, span: Span },
    Block(Block),
    Lambda { params: Vec<String>, body: Box<Expr>, span: Span },
    List(Vec<Expr>, Span),
    /// `[elem for var in iter (if cond)?]` — list comprehension.
    Comprehension { elem: Box<Expr>, var: String, iter: Box<Expr>, cond: Option<Box<Expr>>, span: Span },
    /// `[k: v, …]` / `[:]` — map literal.
    MapLit(Vec<(Expr, Expr)>, Span),
    /// `expr?` — error propagation.
    Try { inner: Box<Expr>, span: Span },
    /// `expr as Type`
    Cast { inner: Box<Expr>, ty: Type, span: Span },
    /// `comptime <expr>` / `comptime { … }`
    Comptime { inner: Box<Expr>, span: Span },
    /// `a..b` (exklusiv) / `a..=b` (inklusiv)
    Range { start: Box<Expr>, end: Box<Expr>, inclusive: bool, span: Span },
    /// `capsule(a, b) { … }` — isolated arena scope: only `inputs` in, only the
    /// block value out (deep-copied), the interior RC-/collector-free (own arena).
    /// `&`-marked inputs are borrowed/read-only (no copy). See
    /// language/CAPSULE-EVALUATION.md.
    Capsule { inputs: Vec<(String, bool)>, body: Block, span: Span },
    /// `spawn f(arg)` — run a call on a new thread, safe by construction. The
    /// inner expression is the spawned call; lowering (see `spawn.rs`) desugars
    /// it to a generated worker shim + `jrt_spawn`, yielding a thread handle that
    /// `join(h)` awaits. See language/REFERENCE.md §10.
    Spawn { call: Box<Expr>, span: Span },
}

#[derive(Debug, Clone)]
pub enum Pattern {
    Wildcard(Span),
    Bind(String, Span),
    Int(i128, Span),
    Str(String, Span),
    Bool(bool, Span),
    /// `Variant(p, p, …)` or `Variant`
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
