//! Vire parser: `Vec<Token> → ast::Module`. Recursive descent for items/
//! statements, Pratt (precedence climbing) for expressions. See language/PARSER.md.

use crate::ast::*;
use crate::diag::{Diag, Span};
use crate::lexer::{Kw, Tok, Token};

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
    pub diags: Vec<Diag>,
}

impl Parser {
    pub fn new(toks: Vec<Token>) -> Self {
        Parser { toks, pos: 0, diags: Vec::new() }
    }

    // --- Token primitives ---
    fn peek(&self) -> &Tok {
        &self.toks[self.pos.min(self.toks.len() - 1)].tok
    }
    fn peek_at(&self, k: usize) -> &Tok {
        &self.toks[(self.pos + k).min(self.toks.len() - 1)].tok
    }
    fn span(&self) -> Span {
        self.toks[self.pos.min(self.toks.len() - 1)].span
    }
    fn bump(&mut self) -> Tok {
        let t = self.toks[self.pos.min(self.toks.len() - 1)].tok.clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }
    fn at(&self, t: &Tok) -> bool {
        self.peek() == t
    }
    fn eat(&mut self, t: &Tok) -> bool {
        if self.at(t) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, t: &Tok, what: &str) {
        if !self.eat(t) {
            self.err(&format!("expected {what}, found {:?}", self.peek()));
        }
    }
    fn err(&mut self, msg: &str) {
        self.diags.push(Diag::error(msg, self.span()));
    }
    /// Skip newlines (soft terminators).
    fn skip_nl(&mut self) {
        while matches!(self.peek(), Tok::Newline) {
            self.bump();
        }
    }
    /// Statement end: newline or `;` (several ok).
    fn stmt_end(&mut self) {
        while matches!(self.peek(), Tok::Newline | Tok::Semi) {
            self.bump();
        }
    }
    fn ident(&mut self) -> String {
        match self.bump() {
            Tok::Ident(s) => s,
            other => {
                self.diags.push(Diag::error(&format!("expected identifier, found {other:?}"), self.span()));
                "_".into()
            }
        }
    }
    fn at_kw(&self, k: Kw) -> bool {
        matches!(self.peek(), Tok::Kw(x) if *x == k)
    }
    fn eat_kw(&mut self, k: Kw) -> bool {
        if self.at_kw(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    // --- Module & items ---
    pub fn parse_module(&mut self) -> Module {
        let mut items = Vec::new();
        // Top-level statements (script style) are collected and combined into an
        // implicit `fn main()` — Python-like, without boilerplate, zero runtime
        // cost (pure frontend sugar).
        let mut top_stmts = Vec::new();
        self.stmt_end();
        while !matches!(self.peek(), Tok::Eof) {
            if self.at_item_start() {
                if let Some(it) = self.parse_item() {
                    items.push(it);
                } else {
                    self.bump();
                }
            } else {
                top_stmts.push(self.parse_stmt());
            }
            self.stmt_end();
        }
        if !top_stmts.is_empty() {
            let has_main = items.iter().any(|it| matches!(it, Item::Fn(f) if f.sig.name == "main"));
            if has_main {
                self.diags.push(crate::diag::Diag::error(
                    "top-level statements AND `fn main` at once are not allowed — pick one",
                    crate::diag::Span(0, 0),
                ));
            } else {
                items.push(synth_main(top_stmts));
            }
        }
        Module { items }
    }

    /// Does an item start here (rather than a top-level statement)?
    fn at_item_start(&self) -> bool {
        matches!(
            self.peek(),
            Tok::Kw(Kw::Fn) | Tok::Kw(Kw::Type) | Tok::Kw(Kw::Trait) | Tok::Kw(Kw::Impl)
                | Tok::Kw(Kw::Const) | Tok::Kw(Kw::Use) | Tok::Kw(Kw::Extern) | Tok::Kw(Kw::Pub)
                | Tok::Kw(Kw::Macro) | Tok::Kw(Kw::Native)
        ) || matches!(self.peek(), Tok::Ident(n) if n == "cxx")
    }

    fn parse_item(&mut self) -> Option<Item> {
        let is_pub = self.eat_kw(Kw::Pub);
        match self.peek() {
            Tok::Kw(Kw::Fn) => Some(Item::Fn(self.parse_fn(is_pub))),
            Tok::Kw(Kw::Native) => Some(self.parse_native()),
            Tok::Kw(Kw::Type) => Some(Item::Type(self.parse_type_def())),
            Tok::Kw(Kw::Trait) => Some(Item::Trait(self.parse_trait())),
            Tok::Kw(Kw::Impl) => Some(Item::Impl(self.parse_impl())),
            Tok::Kw(Kw::Const) => {
                let sp = self.span();
                self.bump();
                let name = self.ident();
                self.expect(&Tok::Eq, "'='");
                let value = self.parse_expr(0);
                Some(Item::Const { name, value, span: sp })
            }
            Tok::Kw(Kw::Use) => {
                let sp = self.span();
                self.bump();
                let mut path = vec![self.ident()];
                while self.eat(&Tok::Dot) {
                    // selective: use a.{b, c} – simplified here, read to end of line
                    if matches!(self.peek(), Tok::LBrace) {
                        break;
                    }
                    path.push(self.ident());
                }
                // skip the rest of the line (e.g. {..} / as ..) for M1
                while !matches!(self.peek(), Tok::Newline | Tok::Semi | Tok::Eof) {
                    self.bump();
                }
                Some(Item::Use { path, span: sp })
            }
            Tok::Kw(Kw::Extern) => Some(self.parse_extern()),
            Tok::Ident(n) if n == "cxx" => Some(self.parse_cxx()),
            Tok::Kw(Kw::Macro) => {
                // `macro name(p, …) = <expr>` — expression macro.
                let sp = self.span();
                self.bump();
                let name = self.ident();
                self.expect(&Tok::LParen, "'('");
                let mut params = Vec::new();
                while !self.at(&Tok::RParen) && !matches!(self.peek(), Tok::Eof) {
                    params.push(self.ident());
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                    self.skip_nl();
                }
                self.expect(&Tok::RParen, "')'");
                self.expect(&Tok::Eq, "'='");
                let body = self.parse_expr(0);
                Some(Item::Macro { name, params, body, span: sp })
            }
            _ => {
                self.err("expected an item (fn/type/trait/impl/const/use/extern)");
                None
            }
        }
    }

    fn parse_generics(&mut self) -> Vec<GenericParam> {
        let mut gs = Vec::new();
        if !self.eat(&Tok::LBracket) {
            return gs;
        }
        self.skip_nl();
        while !self.at(&Tok::RBracket) && !matches!(self.peek(), Tok::Eof) {
            let is_comptime = self.eat_kw(Kw::Comptime);
            let name = self.ident();
            let mut ty = None;
            let mut bounds = Vec::new();
            if self.eat(&Tok::Colon) {
                // either `T: Trait + Trait` or `comptime N: Int`
                if is_comptime {
                    ty = Some(self.parse_type());
                } else {
                    bounds.push(self.ident());
                    while self.eat(&Tok::Plus) {
                        bounds.push(self.ident());
                    }
                }
            }
            gs.push(GenericParam { name, is_comptime, bounds, ty });
            if !self.eat(&Tok::Comma) {
                break;
            }
            self.skip_nl();
        }
        self.expect(&Tok::RBracket, "']'");
        gs
    }

    fn parse_params(&mut self) -> Vec<Param> {
        let mut ps = Vec::new();
        self.expect(&Tok::LParen, "'('");
        self.skip_nl();
        while !self.at(&Tok::RParen) && !matches!(self.peek(), Tok::Eof) {
            // `self` as receiver
            if self.at_kw(Kw::SelfLower) {
                self.bump();
                ps.push(Param { name: "self".into(), ty: None, default: None });
            } else {
                let name = self.ident();
                let ty = if self.eat(&Tok::Colon) { Some(self.parse_type()) } else { None };
                let default = if self.eat(&Tok::Eq) { Some(self.parse_expr(0)) } else { None };
                ps.push(Param { name, ty, default });
            }
            if !self.eat(&Tok::Comma) {
                break;
            }
            self.skip_nl();
        }
        self.expect(&Tok::RParen, "')'");
        ps
    }

    fn parse_fn_sig(&mut self) -> FnSig {
        let sp = self.span();
        self.expect(&Tok::Kw(Kw::Fn), "'fn'");
        let name = self.ident();
        let generics = self.parse_generics();
        let params = self.parse_params();
        // Return type: `-> T` OR the shorter `> T`. After `)` there is no
        // expression context in which `>` could be a comparison → unambiguous.
        // (Match arms/lambdas CANNOT do this: there `>` would collide with the
        // comparison operator or the guard — see language/SYNTAX-SIMPLIFICATION.md.)
        let ret = if self.eat(&Tok::Arrow) || self.eat(&Tok::Gt) { Some(self.parse_type()) } else { None };
        FnSig { name, generics, params, ret, span: sp }
    }

    fn parse_fn(&mut self, is_pub: bool) -> FnDef {
        let sig = self.parse_fn_sig();
        let body = if self.eat(&Tok::Eq) {
            // expression function: `= expr`
            let e = self.parse_expr(0);
            let span = self.span();
            Some(Block { stmts: vec![], tail: Some(Box::new(e)), span })
        } else if self.at(&Tok::LBrace) {
            Some(self.parse_block())
        } else {
            None
        };
        FnDef { sig, body, is_pub }
    }

    fn parse_type_def(&mut self) -> TypeDef {
        let sp = self.span();
        self.expect(&Tok::Kw(Kw::Type), "'type'");
        let name = self.ident();
        let generics = self.parse_generics();
        let mut fields = Vec::new();
        let mut variants = Vec::new();
        let mut methods = Vec::new();
        self.expect(&Tok::LBrace, "'{'");
        self.stmt_end();
        while !self.at(&Tok::RBrace) && !matches!(self.peek(), Tok::Eof) {
            if self.at_kw(Kw::Fn) {
                methods.push(self.parse_fn(false));
            } else {
                let mname = self.ident();
                if self.eat(&Tok::Colon) {
                    // field: name: Type
                    let ty = self.parse_type();
                    fields.push(Field { name: mname, ty });
                } else if self.eat(&Tok::LParen) {
                    // variant with fields: Name(a: T, b: T) or Name(T)
                    let mut vf = Vec::new();
                    let mut positional = true;
                    self.skip_nl();
                    while !self.at(&Tok::RParen) && !matches!(self.peek(), Tok::Eof) {
                        // `name: Type` (named) or just `Type` (positional)
                        if matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::Colon) {
                            positional = false;
                            let fname = self.ident();
                            self.expect(&Tok::Colon, "':'");
                            vf.push(Field { name: fname, ty: self.parse_type() });
                        } else {
                            let ty = self.parse_type();
                            vf.push(Field { name: format!("_{}", vf.len()), ty });
                        }
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                        self.skip_nl();
                    }
                    self.expect(&Tok::RParen, "')'");
                    variants.push(Variant { name: mname, fields: vf, positional });
                } else {
                    // dataless variant
                    variants.push(Variant { name: mname, fields: vec![], positional: true });
                }
            }
            self.stmt_end();
        }
        self.expect(&Tok::RBrace, "'}'");
        TypeDef { name, generics, fields, variants, methods, span: sp }
    }

    fn parse_trait(&mut self) -> TraitDef {
        let sp = self.span();
        self.expect(&Tok::Kw(Kw::Trait), "'trait'");
        let name = self.ident();
        let generics = self.parse_generics();
        let mut methods = Vec::new();
        self.expect(&Tok::LBrace, "'{'");
        self.stmt_end();
        while self.at_kw(Kw::Fn) {
            methods.push(self.parse_fn(false));
            self.stmt_end();
        }
        self.expect(&Tok::RBrace, "'}'");
        TraitDef { name, generics, methods, span: sp }
    }

    fn parse_impl(&mut self) -> ImplDef {
        let sp = self.span();
        self.expect(&Tok::Kw(Kw::Impl), "'impl'");
        // `impl Trait for Type` or `impl Type`
        let first = self.parse_type();
        let (trait_name, for_type) = if self.eat_kw(Kw::For) {
            (Some(first.name), self.parse_type())
        } else {
            (None, first)
        };
        let mut methods = Vec::new();
        self.expect(&Tok::LBrace, "'{'");
        self.stmt_end();
        while self.at_kw(Kw::Fn) {
            methods.push(self.parse_fn(false));
            self.stmt_end();
        }
        self.expect(&Tok::RBrace, "'}'");
        ImplDef { trait_name, for_type, methods, span: sp }
    }

    /// `link "lib"` / `link "a" link "b"` — contextual link directives.
    fn parse_links(&mut self) -> Vec<String> {
        let mut links = Vec::new();
        while matches!(self.peek(), Tok::Ident(n) if n == "link") {
            self.bump();
            if let Tok::Str(s) = self.peek().clone() {
                self.bump();
                links.push(s);
            } else {
                self.err("expected a library name after `link` (string)");
            }
        }
        links
    }

    /// `native "c++" [link "lib"]* """ …code… """` — embedded foreign code.
    fn parse_native(&mut self) -> Item {
        let sp = self.span();
        self.expect(&Tok::Kw(Kw::Native), "'native'");
        let abi = match self.bump() {
            Tok::Str(s) => s,
            _ => "c".into(),
        };
        let links = self.parse_links();
        let code = match self.bump() {
            Tok::Str(s) => s,
            _ => {
                self.err("native: expected the code as a \"\"\"…\"\"\" string");
                String::new()
            }
        };
        Item::Native { abi, code, links, span: sp }
    }

    /// `cxx [link "lib"]* """preamble""" { fn sig = "c++ body" … }`
    fn parse_cxx(&mut self) -> Item {
        let sp = self.span();
        self.bump(); // `cxx`
        let links = self.parse_links();
        // Optional preamble as a triple string (includes/usings).
        let preamble = if let Tok::Str(s) = self.peek().clone() {
            self.bump();
            s
        } else {
            String::new()
        };
        self.expect(&Tok::LBrace, "'{'");
        self.stmt_end();
        let mut fns = Vec::new();
        while self.at_kw(Kw::Fn) {
            let sig = self.parse_fn_sig();
            self.expect(&Tok::Eq, "'=' (C++ body of the trampoline)");
            let body = match self.bump() {
                Tok::Str(s) => s,
                _ => {
                    self.err("cxx: expected the C++ body as a string after `=`");
                    String::new()
                }
            };
            fns.push((sig, body));
            self.stmt_end();
        }
        self.expect(&Tok::RBrace, "'}'");
        Item::Cxx { links, preamble, fns, span: sp }
    }

    fn parse_extern(&mut self) -> Item {
        let sp = self.span();
        self.expect(&Tok::Kw(Kw::Extern), "'extern'");
        let abi = match self.bump() {
            Tok::Str(s) => s,
            _ => "C".into(),
        };
        // Optional: `header "datei.h"` → generate signatures later from the C header
        // (auto-bindgen), no `{}` block.
        let mut header = None;
        if matches!(self.peek(), Tok::Ident(n) if n == "header") {
            self.bump();
            if let Tok::Str(h) = self.peek().clone() {
                self.bump();
                header = Some(h);
            } else {
                self.err("expected a file name after `header` (string)");
            }
        }
        let links = self.parse_links();
        let mut items = Vec::new();
        if header.is_none() {
            self.expect(&Tok::LBrace, "'{'");
            self.stmt_end();
            while self.at_kw(Kw::Fn) {
                items.push(self.parse_fn_sig());
                self.stmt_end();
            }
            self.expect(&Tok::RBrace, "'}'");
        }
        Item::Extern { abi, items, links, header, span: sp }
    }

    fn parse_type(&mut self) -> Type {
        let sp = self.span();
        let borrowed = self.eat(&Tok::Amp);
        let name = match self.peek() {
            Tok::Kw(Kw::SelfType) => {
                self.bump();
                "Self".into()
            }
            _ => self.ident(),
        };
        let mut args = Vec::new();
        if self.eat(&Tok::LBracket) {
            self.skip_nl();
            while !self.at(&Tok::RBracket) && !matches!(self.peek(), Tok::Eof) {
                args.push(self.parse_type());
                if !self.eat(&Tok::Comma) {
                    break;
                }
                self.skip_nl();
            }
            self.expect(&Tok::RBracket, "']'");
        }
        Type { name, args, borrowed, span: sp }
    }

    // --- Blocks & statements ---
    fn parse_block(&mut self) -> Block {
        let sp = self.span();
        self.expect(&Tok::LBrace, "'{'");
        let mut stmts = Vec::new();
        let mut tail = None;
        self.stmt_end();
        while !self.at(&Tok::RBrace) && !matches!(self.peek(), Tok::Eof) {
            let before = self.pos;
            let s = self.parse_stmt();
            // Panic-mode recovery: if parse_stmt made no progress (a syntax error
            // left the cursor stuck), an earlier diagnostic was already recorded;
            // skip to the next statement boundary and keep parsing so the rest of
            // the block still yields diagnostics instead of one error masking all.
            if self.pos == before {
                while !matches!(self.peek(), Tok::Newline | Tok::Semi | Tok::RBrace | Tok::Eof) {
                    self.bump();
                }
                self.stmt_end();
                continue;
            }
            let had_end = matches!(self.peek(), Tok::Newline | Tok::Semi);
            // last expression with no following statement → tail (block value)
            if let Stmt::Expr(e) = &s {
                self.stmt_end();
                if self.at(&Tok::RBrace) {
                    tail = Some(Box::new(e.clone()));
                    break;
                } else {
                    stmts.push(s);
                }
            } else {
                stmts.push(s);
                let _ = had_end;
                self.stmt_end();
            }
        }
        self.expect(&Tok::RBrace, "'}'");
        Block { stmts, tail, span: sp }
    }

    fn parse_stmt(&mut self) -> Stmt {
        let sp = self.span();
        match self.peek() {
            Tok::Kw(Kw::Return) => {
                self.bump();
                let e = if matches!(self.peek(), Tok::Newline | Tok::Semi | Tok::RBrace) {
                    None
                } else {
                    Some(self.parse_expr(0))
                };
                Stmt::Return(e, sp)
            }
            Tok::Kw(Kw::Break) => {
                self.bump();
                Stmt::Break(sp)
            }
            Tok::Kw(Kw::Continue) => {
                self.bump();
                Stmt::Continue(sp)
            }
            Tok::Kw(Kw::While) => {
                self.bump();
                let cond = self.parse_expr(0);
                let body = self.parse_block();
                Stmt::While { cond, body, span: sp }
            }
            Tok::Kw(Kw::For) => {
                self.bump();
                let pat = self.parse_pattern();
                // `for i, x in …` → tuple pattern
                let pat = if self.eat(&Tok::Comma) {
                    let mut ps = vec![pat, self.parse_pattern()];
                    while self.eat(&Tok::Comma) {
                        ps.push(self.parse_pattern());
                    }
                    Pattern::Tuple(ps, sp)
                } else {
                    pat
                };
                self.expect(&Tok::Kw(Kw::In), "'in'");
                let iter = self.parse_expr(0);
                let body = self.parse_block();
                Stmt::For { pat, iter, body, span: sp }
            }
            Tok::Kw(Kw::Mut) => {
                self.bump();
                let name = self.ident();
                let value = if self.eat(&Tok::Eq) { Some(self.parse_expr(0)) } else { None };
                Stmt::Let { mutable: true, name, value, span: sp }
            }
            // `name = expr` (binding) vs. expression: lookahead for `ident =`
            Tok::Ident(_) if matches!(self.peek_at(1), Tok::Eq) => {
                let name = self.ident();
                self.bump(); // =
                let value = self.parse_expr(0);
                Stmt::Let { mutable: false, name, value: Some(value), span: sp }
            }
            _ => {
                let e = self.parse_expr(0);
                // assignment? `lhs [op]= rhs`
                let op = match self.peek() {
                    Tok::Eq => Some(None),
                    Tok::PlusEq => Some(Some(BinOp::Add)),
                    Tok::MinusEq => Some(Some(BinOp::Sub)),
                    Tok::StarEq => Some(Some(BinOp::Mul)),
                    Tok::SlashEq => Some(Some(BinOp::Div)),
                    _ => None,
                };
                if let Some(o) = op {
                    self.bump();
                    let value = self.parse_expr(0);
                    Stmt::Assign { target: e, op: o, value, span: sp }
                } else {
                    Stmt::Expr(e)
                }
            }
        }
    }

    // --- Expressions (Pratt) ---
    fn parse_expr(&mut self, min_bp: u8) -> Expr {
        let mut lhs = self.parse_prefix();
        loop {
            // Postfix (highest binding): . ( [ ? as
            lhs = self.parse_postfix(lhs);
            // Range `a..b` / `a..=b` — lowest binding (bp 1), non-associative.
            if matches!(self.peek(), Tok::DotDot | Tok::DotDotEq) && min_bp <= 1 {
                let sp = self.span();
                let inclusive = matches!(self.peek(), Tok::DotDotEq);
                self.bump();
                let end = self.parse_expr(2);
                lhs = Expr::Range { start: Box::new(lhs), end: Box::new(end), inclusive, span: sp };
                continue;
            }
            let (op, bp) = match self.infix_op() {
                Some(x) => x,
                None => break,
            };
            if bp < min_bp {
                break;
            }
            let sp = self.span();
            self.bump_infix();
            let rhs = self.parse_expr(bp + 1);
            lhs = Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs), span: sp };
        }
        lhs
    }

    /// Current infix operator + left binding power.
    fn infix_op(&self) -> Option<(BinOp, u8)> {
        Some(match self.peek() {
            Tok::Kw(Kw::Or) => (BinOp::Or, 1),
            Tok::Kw(Kw::And) => (BinOp::And, 2),
            Tok::EqEq => (BinOp::Eq, 3),
            Tok::Ne => (BinOp::Ne, 3),
            Tok::Lt => (BinOp::Lt, 3),
            Tok::Le => (BinOp::Le, 3),
            Tok::Gt => (BinOp::Gt, 3),
            Tok::Ge => (BinOp::Ge, 3),
            Tok::Pipe => (BinOp::BitOr, 4),
            Tok::Caret => (BinOp::BitXor, 4),
            Tok::Amp => (BinOp::BitAnd, 5),
            Tok::Shl => (BinOp::Shl, 5),
            Tok::Shr => (BinOp::Shr, 5),
            Tok::Plus => (BinOp::Add, 6),
            Tok::Minus => (BinOp::Sub, 6),
            Tok::PlusPct => (BinOp::AddWrap, 6),
            Tok::MinusPct => (BinOp::SubWrap, 6),
            Tok::Star => (BinOp::Mul, 7),
            Tok::Slash => (BinOp::Div, 7),
            Tok::Percent => (BinOp::Rem, 7),
            Tok::StarPct => (BinOp::MulWrap, 7),
            _ => return None,
        })
    }
    fn bump_infix(&mut self) {
        self.bump();
    }

    fn parse_prefix(&mut self) -> Expr {
        let sp = self.span();
        match self.peek() {
            Tok::Kw(Kw::Not) => {
                self.bump();
                let rhs = self.parse_prefix();
                let rhs = self.parse_postfix(rhs);
                Expr::Unary { op: UnOp::Not, rhs: Box::new(rhs), span: sp }
            }
            Tok::Minus => {
                self.bump();
                let rhs = self.parse_prefix();
                let rhs = self.parse_postfix(rhs);
                Expr::Unary { op: UnOp::Neg, rhs: Box::new(rhs), span: sp }
            }
            Tok::Kw(Kw::Comptime) => {
                self.bump();
                let inner = if self.at(&Tok::LBrace) {
                    Expr::Block(self.parse_block())
                } else {
                    self.parse_expr(1) // fold the entire following expression
                };
                Expr::Comptime { inner: Box::new(inner), span: sp }
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_postfix(&mut self, mut e: Expr) -> Expr {
        loop {
            // Allow leading-dot chains across newlines:
            if matches!(self.peek(), Tok::Newline) {
                let mut k = 0;
                while matches!(self.peek_at(k), Tok::Newline) {
                    k += 1;
                }
                if matches!(self.peek_at(k), Tok::Dot) {
                    self.skip_nl();
                } else {
                    break;
                }
            }
            let sp = self.span();
            match self.peek() {
                Tok::Dot => {
                    self.bump();
                    let name = self.ident();
                    e = Expr::Field { base: Box::new(e), name, span: sp };
                }
                Tok::LParen => {
                    let args = self.parse_call_args();
                    e = Expr::Call { callee: Box::new(e), args, span: sp };
                }
                Tok::LBracket => {
                    self.bump();
                    let index = self.parse_expr(0);
                    self.expect(&Tok::RBracket, "']'");
                    e = Expr::Index { base: Box::new(e), index: Box::new(index), span: sp };
                }
                Tok::Question => {
                    self.bump();
                    e = Expr::Try { inner: Box::new(e), span: sp };
                }
                Tok::Kw(Kw::As) => {
                    self.bump();
                    let ty = self.parse_type();
                    e = Expr::Cast { inner: Box::new(e), ty, span: sp };
                }
                _ => break,
            }
        }
        e
    }

    fn parse_call_args(&mut self) -> Vec<Expr> {
        let mut args = Vec::new();
        self.expect(&Tok::LParen, "'('");
        self.skip_nl();
        while !self.at(&Tok::RParen) && !matches!(self.peek(), Tok::Eof) {
            // named argument `name: expr` → name discarded for M1
            if matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::Colon) {
                self.ident();
                self.bump();
            }
            args.push(self.parse_expr(0));
            if !self.eat(&Tok::Comma) {
                break;
            }
            self.skip_nl();
        }
        self.expect(&Tok::RParen, "')'");
        args
    }

    fn parse_primary(&mut self) -> Expr {
        let sp = self.span();
        match self.peek().clone() {
            Tok::Int(v) => {
                self.bump();
                Expr::Int(v, sp)
            }
            Tok::Float(v) => {
                self.bump();
                Expr::Float(v, sp)
            }
            Tok::Str(s) => {
                self.bump();
                Expr::Str(s, sp)
            }
            Tok::Char(c) => {
                self.bump();
                Expr::Char(c, sp)
            }
            Tok::Kw(Kw::True) => {
                self.bump();
                Expr::Bool(true, sp)
            }
            Tok::Kw(Kw::False) => {
                self.bump();
                Expr::Bool(false, sp)
            }
            Tok::Kw(Kw::SelfLower) => {
                self.bump();
                Expr::SelfExpr(sp)
            }
            Tok::Kw(Kw::If) => self.parse_if(),
            Tok::Kw(Kw::Match) => self.parse_match(),
            Tok::Kw(Kw::Capsule) => {
                self.bump();
                let mut inputs = Vec::new();
                self.expect(&Tok::LParen, "'('");
                self.skip_nl();
                while !self.at(&Tok::RParen) && !matches!(self.peek(), Tok::Eof) {
                    let borrowed = self.eat(&Tok::Amp);
                    inputs.push((self.ident(), borrowed));
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                    self.skip_nl();
                }
                self.expect(&Tok::RParen, "')'");
                let body = self.parse_block();
                Expr::Capsule { inputs, body, span: sp }
            }
            Tok::At => {
                // compiler intrinsic @name(...) — as a call on ident "@name"
                self.bump();
                let name = format!("@{}", self.ident());
                Expr::Ident(name, sp)
            }
            Tok::Ident(_) => {
                // Lambda `x -> e`?
                if matches!(self.peek_at(1), Tok::Arrow) {
                    let p = self.ident();
                    self.bump(); // ->
                    let body = self.parse_expr(0);
                    Expr::Lambda { params: vec![p], body: Box::new(body), span: sp }
                } else {
                    Expr::Ident(self.ident(), sp)
                }
            }
            Tok::LParen => self.parse_paren_or_lambda(),
            Tok::LBracket => {
                // list literal
                self.bump();
                self.skip_nl();
                // empty list `[]` or empty map `[:]`.
                if self.at(&Tok::RBracket) {
                    self.bump();
                    return Expr::List(Vec::new(), sp);
                }
                if self.at(&Tok::Colon) {
                    self.bump();
                    self.expect(&Tok::RBracket, "']'");
                    return Expr::MapLit(Vec::new(), sp);
                }
                let first = self.parse_expr(0);
                // map literal `[k: v, …]`
                if self.eat(&Tok::Colon) {
                    let v0 = self.parse_expr(0);
                    let mut pairs = vec![(first, v0)];
                    while self.eat(&Tok::Comma) {
                        self.skip_nl();
                        if self.at(&Tok::RBracket) {
                            break;
                        }
                        let k = self.parse_expr(0);
                        self.expect(&Tok::Colon, "':'");
                        let v = self.parse_expr(0);
                        pairs.push((k, v));
                    }
                    self.skip_nl();
                    self.expect(&Tok::RBracket, "']'");
                    return Expr::MapLit(pairs, sp);
                }
                // comprehension `[elem for var in iter (if cond)?]`
                if self.at_kw(Kw::For) {
                    self.bump(); // for
                    let var = self.ident();
                    self.expect(&Tok::Kw(Kw::In), "'in'");
                    let iter = self.parse_expr(0);
                    let cond = if self.eat_kw(Kw::If) { Some(Box::new(self.parse_expr(0))) } else { None };
                    self.skip_nl();
                    self.expect(&Tok::RBracket, "']'");
                    return Expr::Comprehension { elem: Box::new(first), var, iter: Box::new(iter), cond, span: sp };
                }
                let mut items = vec![first];
                if self.eat(&Tok::Comma) {
                    self.skip_nl();
                    while !self.at(&Tok::RBracket) && !matches!(self.peek(), Tok::Eof) {
                        items.push(self.parse_expr(0));
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                        self.skip_nl();
                    }
                }
                self.expect(&Tok::RBracket, "']'");
                Expr::List(items, sp)
            }
            Tok::LBrace => Expr::Block(self.parse_block()),
            other => {
                self.err(&format!("unexpected in expression: {other:?}"));
                self.bump();
                Expr::Int(0, sp)
            }
        }
    }

    fn parse_paren_or_lambda(&mut self) -> Expr {
        let sp = self.span();
        // `(a, b) -> e` lambda vs. `(e)` parenthesization: scan to the matching `)`
        // and check whether `->` follows.
        let mut depth = 0;
        let mut k = 0;
        loop {
            match self.peek_at(k) {
                Tok::LParen => depth += 1,
                Tok::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                Tok::Eof => break,
                _ => {}
            }
            k += 1;
        }
        let is_lambda = matches!(self.peek_at(k + 1), Tok::Arrow);
        self.bump(); // (
        if is_lambda {
            let mut params = Vec::new();
            self.skip_nl();
            while !self.at(&Tok::RParen) && !matches!(self.peek(), Tok::Eof) {
                params.push(self.ident());
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen, "')'");
            self.expect(&Tok::Arrow, "'->'");
            let body = self.parse_expr(0);
            Expr::Lambda { params, body: Box::new(body), span: sp }
        } else {
            self.skip_nl();
            let e = self.parse_expr(0);
            self.expect(&Tok::RParen, "')'");
            e
        }
    }

    fn parse_if(&mut self) -> Expr {
        let sp = self.span();
        self.expect(&Tok::Kw(Kw::If), "'if'");
        let cond = self.parse_expr(0);
        let then = self.parse_block();
        let mut elifs = Vec::new();
        let mut els = None;
        loop {
            if self.eat_kw(Kw::Elif) {
                let c = self.parse_expr(0);
                let b = self.parse_block();
                elifs.push((c, b));
            } else if self.eat_kw(Kw::Else) {
                els = Some(self.parse_block());
                break;
            } else {
                break;
            }
        }
        Expr::If { cond: Box::new(cond), then, elifs, els, span: sp }
    }

    fn parse_match(&mut self) -> Expr {
        let sp = self.span();
        self.expect(&Tok::Kw(Kw::Match), "'match'");
        let scrutinee = self.parse_expr(0);
        let mut arms = Vec::new();
        self.expect(&Tok::LBrace, "'{'");
        self.stmt_end();
        while !self.at(&Tok::RBrace) && !matches!(self.peek(), Tok::Eof) {
            let pat = self.parse_pattern();
            let guard = if self.eat_kw(Kw::If) { Some(self.parse_expr(0)) } else { None };
            self.expect(&Tok::Arrow, "'->'");
            let body = if self.at(&Tok::LBrace) {
                Expr::Block(self.parse_block())
            } else {
                self.parse_expr(0)
            };
            arms.push((pat, guard, body));
            self.stmt_end();
        }
        self.expect(&Tok::RBrace, "'}'");
        Expr::Match { scrutinee: Box::new(scrutinee), arms, span: sp }
    }

    fn parse_pattern(&mut self) -> Pattern {
        let sp = self.span();
        let p = match self.peek().clone() {
            Tok::Ident(s) if s == "_" => {
                self.bump();
                Pattern::Wildcard(sp)
            }
            Tok::Int(v) => {
                self.bump();
                Pattern::Int(v, sp)
            }
            Tok::Str(s) => {
                self.bump();
                Pattern::Str(s, sp)
            }
            Tok::Kw(Kw::True) => {
                self.bump();
                Pattern::Bool(true, sp)
            }
            Tok::Kw(Kw::False) => {
                self.bump();
                Pattern::Bool(false, sp)
            }
            Tok::LParen => {
                self.bump();
                let mut ps = Vec::new();
                while !self.at(&Tok::RParen) && !matches!(self.peek(), Tok::Eof) {
                    ps.push(self.parse_pattern());
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RParen, "')'");
                Pattern::Tuple(ps, sp)
            }
            Tok::Ident(name) => {
                self.bump();
                // constructor with fields? `Name(p, …)` or path `Type.Variant`
                let mut full = name;
                while self.eat(&Tok::Dot) {
                    full.push('.');
                    full.push_str(&self.ident());
                }
                if self.at(&Tok::LParen) {
                    self.bump();
                    let mut args = Vec::new();
                    while !self.at(&Tok::RParen) && !matches!(self.peek(), Tok::Eof) {
                        args.push(self.parse_pattern());
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(&Tok::RParen, "')'");
                    Pattern::Ctor { name: full, args, span: sp }
                } else if full.contains('.') || full.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                    Pattern::Ctor { name: full, args: vec![], span: sp }
                } else {
                    Pattern::Bind(full, sp)
                }
            }
            other => {
                self.err(&format!("unexpected in pattern: {other:?}"));
                self.bump();
                Pattern::Wildcard(sp)
            }
        };
        // or-pattern `A | B`
        if self.at(&Tok::Pipe) {
            let mut alts = vec![p];
            while self.eat(&Tok::Pipe) {
                alts.push(self.parse_pattern());
            }
            Pattern::Or(alts, sp)
        } else {
            p
        }
    }
}

/// Convenient entry point: source text → (module, diagnostics).
/// Builds an implicit `fn main()` from the collected top-level statements.
fn synth_main(stmts: Vec<Stmt>) -> Item {
    use crate::ast::*;
    use crate::diag::Span;
    Item::Fn(FnDef {
        sig: FnSig { name: "main".into(), generics: vec![], params: vec![], ret: None, span: Span(0, 0) },
        body: Some(Block { stmts, tail: None, span: Span(0, 0) }),
        is_pub: false,
    })
}

pub fn parse(src: &str) -> (Module, Vec<Diag>) {
    parse_with_syntax(src, crate::syntax::Syntax::default())
}

/// Like `parse`, but with user-defined keyword spelling.
pub fn parse_with_syntax(src: &str, syntax: crate::syntax::Syntax) -> (Module, Vec<Diag>) {
    let (toks, mut diags) = crate::lexer::Lexer::with_syntax(src, syntax).lex();
    let mut p = Parser::new(toks);
    let m = p.parse_module();
    diags.append(&mut p.diags);
    (m, diags)
}
