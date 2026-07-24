//! `vire fmt` — a canonical AST→source pretty-printer.
//!
//! Emits Vire source from the *raw parsed AST* (before desugars/expansion). Its
//! contract is **round-trip stability**: `parse(fmt(m))` yields an AST equal to
//! `m` (modulo spans), and `fmt` is idempotent — `fmt(fmt(src)) == fmt(src)`.
//! That makes it parser-fuzz insurance: run every `.vr` through it and it must
//! still compile to the same result. String literals are re-escaped (including
//! `{`→`{{` / `}`→`}}`, since interpolation runs at parse time) so a string with
//! braces survives a round trip.

use crate::ast::*;

pub fn format_module(m: &Module) -> String {
    let mut f = Fmt { out: String::new(), ind: 0 };
    for (i, it) in m.items.iter().enumerate() {
        if i > 0 {
            f.out.push('\n');
        }
        f.item(it);
    }
    if !f.out.ends_with('\n') {
        f.out.push('\n');
    }
    f.out
}

struct Fmt {
    out: String,
    ind: usize,
}

impl Fmt {
    fn line(&mut self, s: &str) {
        for _ in 0..self.ind {
            self.out.push_str("    ");
        }
        self.out.push_str(s);
        self.out.push('\n');
    }

    // --- Items ---------------------------------------------------------------
    fn item(&mut self, it: &Item) {
        match it {
            Item::Fn(fd) => self.fndef(fd, ""),
            Item::Type(t) => self.typedef(t),
            Item::Trait(t) => self.traitdef(t),
            Item::Impl(im) => self.impldef(im),
            Item::Const { name, value, .. } => {
                let v = self.expr(value);
                self.line(&format!("const {name} = {v}"));
            }
            Item::Use { path, .. } => self.line(&format!("use {}", path.join("."))),
            Item::Extern { abi, items, links, header, .. } => {
                let mut head = format!("extern \"{abi}\"");
                for l in links {
                    head.push_str(&format!(" link \"{l}\""));
                }
                if let Some(h) = header {
                    head.push_str(&format!(" header \"{}\"", esc_raw(h)));
                    self.line(&head);
                    return;
                }
                self.line(&format!("{head} {{"));
                self.ind += 1;
                for s in items {
                    let sig = self.fnsig(s);
                    self.line(&sig);
                }
                self.ind -= 1;
                self.line("}");
            }
            Item::Native { abi, code, links, .. } => {
                let mut head = format!("native \"{abi}\"");
                for l in links {
                    head.push_str(&format!(" link \"{l}\""));
                }
                self.line(&format!("{head} \"\"\"{code}\"\"\""));
            }
            Item::Macro { name, params, body, .. } => {
                let b = self.expr(body);
                self.line(&format!("macro {name}({}) = {b}", params.join(", ")));
            }
            Item::ItemMacro { name, params, items, .. } => {
                let ps: Vec<String> = params.iter().map(|p| format!("{}: {}", p.name, kind_name(p.kind))).collect();
                self.line(&format!("macro {name}({}) {{", ps.join(", ")));
                self.ind += 1;
                for (i, sub) in items.iter().enumerate() {
                    if i > 0 {
                        self.out.push('\n');
                    }
                    self.item(sub);
                }
                self.ind -= 1;
                self.line("}");
            }
            Item::MacroInvoke { name, args, .. } => {
                let a: Vec<String> = args.iter().map(|x| self.expr(x)).collect();
                self.line(&format!("{name}!({})", a.join(", ")));
            }
            Item::Cxx { links, preamble, fns, .. } => {
                let mut head = String::from("cxx");
                for l in links {
                    head.push_str(&format!(" link \"{l}\""));
                }
                self.line(&format!("{head} \"\"\"{preamble}\"\"\" {{"));
                self.ind += 1;
                for (sig, body) in fns {
                    let s = self.fnsig(sig);
                    self.line(&format!("{s} = \"{}\"", esc(body)));
                }
                self.ind -= 1;
                self.line("}");
            }
        }
    }

    fn attrs(&mut self, attrs: &[Attr]) {
        for a in attrs {
            if a.args.is_empty() {
                self.line(&format!("@{}", a.name));
            } else {
                self.line(&format!("@{}({})", a.name, a.args.join(", ")));
            }
        }
    }

    fn fndef(&mut self, fd: &FnDef, prefix: &str) {
        self.attrs(&fd.attrs);
        let pub_kw = if fd.is_pub { "pub " } else { "" };
        let sig = self.fnsig(&fd.sig);
        match &fd.body {
            None => self.line(&format!("{prefix}{pub_kw}{sig}")),
            Some(b) => {
                // Expression body `= expr` when the block is a bare tail expression.
                if b.stmts.is_empty() {
                    if let Some(t) = &b.tail {
                        let e = self.expr(t);
                        self.line(&format!("{prefix}{pub_kw}{sig} = {e}"));
                        return;
                    }
                }
                self.line(&format!("{prefix}{pub_kw}{sig} {{"));
                self.ind += 1;
                self.block_inner(b);
                self.ind -= 1;
                self.line("}");
            }
        }
    }

    fn fnsig(&self, s: &FnSig) -> String {
        let g = self.generics(&s.generics);
        let ps: Vec<String> = s
            .params
            .iter()
            .map(|p| {
                let mut r = p.name.clone();
                if let Some(t) = &p.ty {
                    r.push_str(&format!(": {}", type_str(t)));
                }
                if let Some(d) = &p.default {
                    r.push_str(&format!(" = {}", self.expr(d)));
                }
                r
            })
            .collect();
        let ret = s.ret.as_ref().map(|t| format!(" -> {}", type_str(t))).unwrap_or_default();
        format!("fn {}{g}({}){ret}", s.name, ps.join(", "))
    }

    fn generics(&self, gs: &[GenericParam]) -> String {
        if gs.is_empty() {
            return String::new();
        }
        let items: Vec<String> = gs
            .iter()
            .map(|g| {
                let mut r = String::new();
                if g.is_comptime {
                    r.push_str("comptime ");
                }
                r.push_str(&g.name);
                if let Some(t) = &g.ty {
                    r.push_str(&format!(": {}", type_str(t)));
                } else if !g.bounds.is_empty() {
                    r.push_str(&format!(": {}", g.bounds.join(" + ")));
                }
                r
            })
            .collect();
        format!("[{}]", items.join(", "))
    }

    fn typedef(&mut self, t: &TypeDef) {
        self.attrs(&t.attrs);
        let g = self.generics(&t.generics);
        self.line(&format!("type {}{g} {{", t.name));
        self.ind += 1;
        for fld in &t.fields {
            self.line(&format!("{}: {}", fld.name, type_str(&fld.ty)));
        }
        for v in &t.variants {
            if v.fields.is_empty() {
                self.line(&v.name);
            } else {
                let fs: Vec<String> = v.fields.iter().map(|f| format!("{}: {}", f.name, type_str(&f.ty))).collect();
                self.line(&format!("{}({})", v.name, fs.join(", ")));
            }
        }
        for md in &t.methods {
            self.fndef(md, "");
        }
        self.ind -= 1;
        self.line("}");
    }

    fn traitdef(&mut self, t: &TraitDef) {
        let g = self.generics(&t.generics);
        self.line(&format!("trait {}{g} {{", t.name));
        self.ind += 1;
        for md in &t.methods {
            self.fndef(md, "");
        }
        self.ind -= 1;
        self.line("}");
    }

    fn impldef(&mut self, im: &ImplDef) {
        let head = match &im.trait_name {
            Some(tn) => format!("impl {tn} for {}", type_str(&im.for_type)),
            None => format!("impl {}", type_str(&im.for_type)),
        };
        self.line(&format!("{head} {{"));
        self.ind += 1;
        for md in &im.methods {
            self.fndef(md, "");
        }
        self.ind -= 1;
        self.line("}");
    }

    // --- Statements / blocks -------------------------------------------------
    fn block_inner(&mut self, b: &Block) {
        for s in &b.stmts {
            self.stmt(s);
        }
        if let Some(t) = &b.tail {
            let e = self.expr(t);
            self.line(&e);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let { mutable, name, ty, value, .. } => {
                let m = if *mutable { "mut " } else { "" };
                let ann = ty.as_ref().map(|t| format!(": {}", type_str(t))).unwrap_or_default();
                match value {
                    Some(v) => {
                        let e = self.expr(v);
                        self.line(&format!("{m}{name}{ann} = {e}"));
                    }
                    None => self.line(&format!("{m}{name}{ann}")),
                }
            }
            Stmt::Assign { target, op, value, .. } => {
                let t = self.expr(target);
                let v = self.expr(value);
                let o = op.map(|o| format!("{}=", binop_str(o))).unwrap_or_else(|| "=".into());
                self.line(&format!("{t} {o} {v}"));
            }
            Stmt::Expr(e) => {
                let s = self.expr(e);
                self.line(&s);
            }
            Stmt::Return(v, _) => match v {
                Some(e) => {
                    let s = self.expr(e);
                    self.line(&format!("return {s}"));
                }
                None => self.line("return"),
            },
            Stmt::Break(_) => self.line("break"),
            Stmt::Continue(_) => self.line("continue"),
            Stmt::While { cond, body, .. } => {
                let c = self.expr(cond);
                self.line(&format!("while {c} {{"));
                self.ind += 1;
                self.block_inner(body);
                self.ind -= 1;
                self.line("}");
            }
            Stmt::For { pat, iter, body, .. } => {
                let it = self.expr(iter);
                self.line(&format!("for {} in {it} {{", pat_str(pat)));
                self.ind += 1;
                self.block_inner(body);
                self.ind -= 1;
                self.line("}");
            }
        }
    }

    // --- Expressions (single-line) -------------------------------------------
    // A block expression is emitted multi-line via `block_expr`; everything else
    // is a single string. If/Match nested inside a value position also render
    // multi-line through a small buffer.
    fn expr(&self, e: &Expr) -> String {
        match e {
            Expr::Int(v, _) => v.to_string(),
            Expr::Float(v, _) => fmt_float(*v),
            Expr::Str(s, _) => format!("\"{}\"", esc(s)),
            Expr::Char(c, _) => format!("'{}'", esc_char(*c)),
            Expr::Bool(b, _) => b.to_string(),
            Expr::Ident(n, _) => n.clone(),
            Expr::SelfExpr(_) => "self".into(),
            Expr::Unary { op, rhs, .. } => {
                let r = self.expr(rhs);
                match op {
                    UnOp::Neg => format!("-{}", paren_if_binary(rhs, &r)),
                    UnOp::Not => format!("not {}", paren_if_binary(rhs, &r)),
                }
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                let l = self.expr(lhs);
                let r = self.expr(rhs);
                format!("{} {} {}", paren_if_binary(lhs, &l), binop_str(*op), paren_if_binary(rhs, &r))
            }
            Expr::Call { callee, args, .. } => {
                let c = self.expr(callee);
                let a: Vec<String> = args.iter().map(|x| self.expr(x)).collect();
                format!("{c}({})", a.join(", "))
            }
            Expr::TurboCall { callee, targs, args, .. } => {
                let t: Vec<String> = targs.iter().map(|x| self.expr(x)).collect();
                let a: Vec<String> = args.iter().map(|x| self.expr(x)).collect();
                format!("{callee}[{}]({})", t.join(", "), a.join(", "))
            }
            Expr::Field { base, name, .. } => {
                let b = self.expr(base);
                format!("{}.{name}", paren_if_binary(base, &b))
            }
            Expr::Index { base, index, .. } => {
                let b = self.expr(base);
                let i = self.expr(index);
                format!("{}[{i}]", paren_if_binary(base, &b))
            }
            Expr::If { cond, then, elifs, els, .. } => self.if_str(cond, then, elifs, els),
            Expr::Match { scrutinee, arms, .. } => self.match_str(scrutinee, arms),
            Expr::Block(b) => self.block_str(b),
            Expr::Lambda { params, body, .. } => {
                let b = self.expr(body);
                if params.len() == 1 {
                    format!("{} -> {b}", params[0])
                } else {
                    format!("({}) -> {b}", params.join(", "))
                }
            }
            Expr::List(xs, _) => {
                let a: Vec<String> = xs.iter().map(|x| self.expr(x)).collect();
                format!("[{}]", a.join(", "))
            }
            Expr::Comprehension { elem, var, iter, cond, .. } => {
                let el = self.expr(elem);
                let it = self.expr(iter);
                let c = cond.as_ref().map(|c| format!(" if {}", self.expr(c))).unwrap_or_default();
                format!("[{el} for {var} in {it}{c}]")
            }
            Expr::MapLit(kvs, _) => {
                if kvs.is_empty() {
                    return "[:]".into();
                }
                let a: Vec<String> = kvs.iter().map(|(k, v)| format!("{}: {}", self.expr(k), self.expr(v))).collect();
                format!("[{}]", a.join(", "))
            }
            Expr::Try { inner, .. } => {
                let i = self.expr(inner);
                format!("{}?", paren_if_binary(inner, &i))
            }
            Expr::Cast { inner, ty, .. } => {
                let i = self.expr(inner);
                format!("{} as {}", paren_if_binary(inner, &i), type_str(ty))
            }
            Expr::Comptime { inner, .. } => format!("comptime {}", self.expr(inner)),
            Expr::Range { start, end, inclusive, .. } => {
                let op = if *inclusive { "..=" } else { ".." };
                format!("{}{op}{}", self.expr(start), self.expr(end))
            }
            Expr::Capsule { inputs, body, .. } => {
                let ins: Vec<String> = inputs.iter().map(|(n, borrowed)| if *borrowed { format!("&{n}") } else { n.clone() }).collect();
                format!("capsule({}) {}", ins.join(", "), self.block_str(body))
            }
            Expr::Spawn { call, .. } => format!("spawn {}", self.expr(call)),
        }
    }

    /// A value-position block/if/match must render as multi-line text. Because
    /// `expr` returns a String, we render the nested lines here with the current
    /// indentation baked in, then the caller places the first line inline.
    fn block_str(&self, b: &Block) -> String {
        let mut inner = Fmt { out: String::new(), ind: self.ind + 1 };
        inner.block_inner(b);
        let mut s = String::from("{\n");
        s.push_str(&inner.out);
        for _ in 0..self.ind {
            s.push_str("    ");
        }
        s.push('}');
        s
    }

    fn if_str(&self, cond: &Expr, then: &Block, elifs: &[(Expr, Block)], els: &Option<Block>) -> String {
        let mut s = format!("if {} {}", self.expr(cond), self.block_str(then));
        for (c, b) in elifs {
            s.push_str(&format!(" elif {} {}", self.expr(c), self.block_str(b)));
        }
        if let Some(b) = els {
            s.push_str(&format!(" else {}", self.block_str(b)));
        }
        s
    }

    fn match_str(&self, scrut: &Expr, arms: &[(Pattern, Option<Expr>, Expr)]) -> String {
        let mut s = format!("match {} {{\n", self.expr(scrut));
        for (p, guard, body) in arms {
            for _ in 0..self.ind + 1 {
                s.push_str("    ");
            }
            let g = guard.as_ref().map(|g| format!(" if {}", self.expr(g))).unwrap_or_default();
            let body_s = match body {
                Expr::Block(b) => {
                    let mut inner = Fmt { out: String::new(), ind: self.ind + 1 };
                    inner.block_inner(b);
                    let mut bs = String::from("{\n");
                    bs.push_str(&inner.out);
                    for _ in 0..self.ind + 1 {
                        bs.push_str("    ");
                    }
                    bs.push('}');
                    bs
                }
                other => self.expr(other),
            };
            s.push_str(&format!("{}{g} -> {body_s}\n", pat_str(p)));
        }
        for _ in 0..self.ind {
            s.push_str("    ");
        }
        s.push('}');
        s
    }
}

// --- Leaves ------------------------------------------------------------------

fn kind_name(k: ParamKind) -> &'static str {
    match k {
        ParamKind::Type => "type",
        ParamKind::Ident => "ident",
        ParamKind::Expr => "expr",
        ParamKind::Block => "block",
        ParamKind::Pat => "pat",
    }
}

fn type_str(t: &Type) -> String {
    let amp = if t.borrowed { "&" } else { "" };
    if t.args.is_empty() {
        format!("{amp}{}", t.name)
    } else {
        let a: Vec<String> = t.args.iter().map(type_str).collect();
        format!("{amp}{}[{}]", t.name, a.join(", "))
    }
}

fn pat_str(p: &Pattern) -> String {
    match p {
        Pattern::Wildcard(_) => "_".into(),
        Pattern::Bind(n, _) => n.clone(),
        Pattern::Int(v, _) => v.to_string(),
        Pattern::Str(s, _) => format!("\"{}\"", esc(s)),
        Pattern::Bool(b, _) => b.to_string(),
        Pattern::Ctor { name, args, .. } => {
            if args.is_empty() {
                name.clone()
            } else {
                let a: Vec<String> = args.iter().map(pat_str).collect();
                format!("{name}({})", a.join(", "))
            }
        }
        Pattern::Tuple(ps, _) => {
            let a: Vec<String> = ps.iter().map(pat_str).collect();
            format!("({})", a.join(", "))
        }
        Pattern::Or(ps, _) => {
            let a: Vec<String> = ps.iter().map(pat_str).collect();
            a.join(" | ")
        }
    }
}

fn binop_str(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+", BinOp::Sub => "-", BinOp::Mul => "*", BinOp::Div => "/", BinOp::Rem => "%",
        BinOp::AddWrap => "+%", BinOp::SubWrap => "-%", BinOp::MulWrap => "*%",
        BinOp::Eq => "==", BinOp::Ne => "!=", BinOp::Lt => "<", BinOp::Le => "<=", BinOp::Gt => ">", BinOp::Ge => ">=",
        BinOp::And => "and", BinOp::Or => "or",
        BinOp::BitAnd => "&", BinOp::BitOr => "|", BinOp::BitXor => "^", BinOp::Shl => "<<", BinOp::Shr => ">>",
    }
}

/// Parenthesize a sub-expression when it is a binary/lambda/range whose
/// precedence a naive re-parse could break. Conservative: wrap binaries.
fn paren_if_binary(e: &Expr, rendered: &str) -> String {
    match e {
        Expr::Binary { .. } | Expr::Lambda { .. } | Expr::Range { .. } | Expr::Cast { .. } => format!("({rendered})"),
        _ => rendered.to_string(),
    }
}

/// Escape a normal `"…"` string: control chars + `"`/`\`, and braces as `{{`/`}}`
/// (interpolation runs at parse time, so a literal brace must be doubled to survive
/// a round trip).
fn esc(s: &str) -> String {
    let mut r = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => r.push_str("\\\\"),
            '"' => r.push_str("\\\""),
            '\n' => r.push_str("\\n"),
            '\t' => r.push_str("\\t"),
            '\r' => r.push_str("\\r"),
            '\0' => r.push_str("\\0"),
            '{' => r.push_str("{{"),
            '}' => r.push_str("}}"),
            _ => r.push(c),
        }
    }
    r
}

fn esc_char(c: char) -> String {
    match c {
        '\\' => "\\\\".into(),
        '\'' => "\\'".into(),
        '\n' => "\\n".into(),
        '\t' => "\\t".into(),
        '\r' => "\\r".into(),
        '\0' => "\\0".into(),
        _ => c.to_string(),
    }
}

/// A header/lib string: quotes/backslashes only (no brace doubling — not interpolated).
fn esc_raw(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn fmt_float(v: f64) -> String {
    if v == v.trunc() && v.is_finite() {
        format!("{v:.1}")
    } else {
        let s = format!("{v}");
        if s.contains('.') || s.contains('e') || s.contains("inf") || s.contains("NaN") {
            s
        } else {
            format!("{s}.0")
        }
    }
}
