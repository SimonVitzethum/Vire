//! Vire-Parser: `Vec<Token> → ast::Module`. Rekursiver Abstieg für Items/
//! Statements, Pratt (Präzedenzklettern) für Ausdrücke. Siehe sprache/PARSER.md.

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

    // --- Token-Primitive ---
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
            self.err(&format!("erwartete {what}, fand {:?}", self.peek()));
        }
    }
    fn err(&mut self, msg: &str) {
        self.diags.push(Diag::error(msg, self.span()));
    }
    /// Newlines (weiche Terminatoren) überspringen.
    fn skip_nl(&mut self) {
        while matches!(self.peek(), Tok::Newline) {
            self.bump();
        }
    }
    /// Anweisungsende: Newline oder `;` (mehrere ok).
    fn stmt_end(&mut self) {
        while matches!(self.peek(), Tok::Newline | Tok::Semi) {
            self.bump();
        }
    }
    fn ident(&mut self) -> String {
        match self.bump() {
            Tok::Ident(s) => s,
            other => {
                self.diags.push(Diag::error(&format!("erwartete Bezeichner, fand {other:?}"), self.span()));
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

    // --- Modul & Items ---
    pub fn parse_module(&mut self) -> Module {
        let mut items = Vec::new();
        // Top-Level-Anweisungen (Skript-Stil) werden gesammelt und zu einem
        // impliziten `fn main()` zusammengefasst — Python-artig, ohne Boilerplate,
        // null Laufzeitkosten (reine Frontend-Zucker).
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
                    "Top-Level-Anweisungen UND `fn main` zugleich sind nicht erlaubt — eins von beiden",
                    crate::diag::Span(0, 0),
                ));
            } else {
                items.push(synth_main(top_stmts));
            }
        }
        Module { items }
    }

    /// Beginnt hier ein Item (nicht eine Top-Level-Anweisung)?
    fn at_item_start(&self) -> bool {
        matches!(
            self.peek(),
            Tok::Kw(Kw::Fn) | Tok::Kw(Kw::Type) | Tok::Kw(Kw::Trait) | Tok::Kw(Kw::Impl)
                | Tok::Kw(Kw::Const) | Tok::Kw(Kw::Use) | Tok::Kw(Kw::Extern) | Tok::Kw(Kw::Pub)
                | Tok::Kw(Kw::Macro)
        )
    }

    fn parse_item(&mut self) -> Option<Item> {
        let is_pub = self.eat_kw(Kw::Pub);
        match self.peek() {
            Tok::Kw(Kw::Fn) => Some(Item::Fn(self.parse_fn(is_pub))),
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
                    // selektiv: use a.{b, c} – hier vereinfacht bis Zeilenende lesen
                    if matches!(self.peek(), Tok::LBrace) {
                        break;
                    }
                    path.push(self.ident());
                }
                // Rest der Zeile (z.B. {..} / as ..) für M1 überspringen
                while !matches!(self.peek(), Tok::Newline | Tok::Semi | Tok::Eof) {
                    self.bump();
                }
                Some(Item::Use { path, span: sp })
            }
            Tok::Kw(Kw::Extern) => Some(self.parse_extern()),
            _ => {
                self.err("erwartete ein Item (fn/type/trait/impl/const/use/extern)");
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
                // entweder `T: Trait + Trait` oder `comptime N: Int`
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
            // `self` als Empfänger
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
        let ret = if self.eat(&Tok::Arrow) { Some(self.parse_type()) } else { None };
        FnSig { name, generics, params, ret, span: sp }
    }

    fn parse_fn(&mut self, is_pub: bool) -> FnDef {
        let sig = self.parse_fn_sig();
        let body = if self.eat(&Tok::Eq) {
            // Ausdrucksfunktion: `= expr`
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
                    // Feld: name: Type
                    let ty = self.parse_type();
                    fields.push(Field { name: mname, ty });
                } else if self.eat(&Tok::LParen) {
                    // Variante mit Feldern: Name(a: T, b: T) oder Name(T)
                    let mut vf = Vec::new();
                    let mut positional = true;
                    self.skip_nl();
                    while !self.at(&Tok::RParen) && !matches!(self.peek(), Tok::Eof) {
                        // `name: Type` (benannt) oder nur `Type` (positional)
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
                    // datenlose Variante
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
        // `impl Trait for Type` oder `impl Type`
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

    fn parse_extern(&mut self) -> Item {
        let sp = self.span();
        self.expect(&Tok::Kw(Kw::Extern), "'extern'");
        let abi = match self.bump() {
            Tok::Str(s) => s,
            _ => "C".into(),
        };
        let mut items = Vec::new();
        self.expect(&Tok::LBrace, "'{'");
        self.stmt_end();
        while self.at_kw(Kw::Fn) {
            items.push(self.parse_fn_sig());
            self.stmt_end();
        }
        self.expect(&Tok::RBrace, "'}'");
        Item::Extern { abi, items, span: sp }
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

    // --- Blöcke & Statements ---
    fn parse_block(&mut self) -> Block {
        let sp = self.span();
        self.expect(&Tok::LBrace, "'{'");
        let mut stmts = Vec::new();
        let mut tail = None;
        self.stmt_end();
        while !self.at(&Tok::RBrace) && !matches!(self.peek(), Tok::Eof) {
            let s = self.parse_stmt();
            let had_end = matches!(self.peek(), Tok::Newline | Tok::Semi);
            // Letzter Ausdruck ohne folgende Anweisung → tail (Blockwert)
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
                // `for i, x in …` → Tupelmuster
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
            // `name = expr` (Bindung) vs. Ausdruck: Lookahead auf `ident =`
            Tok::Ident(_) if matches!(self.peek_at(1), Tok::Eq) => {
                let name = self.ident();
                self.bump(); // =
                let value = self.parse_expr(0);
                Stmt::Let { mutable: false, name, value: Some(value), span: sp }
            }
            _ => {
                let e = self.parse_expr(0);
                // Zuweisung? `lhs [op]= rhs`
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

    // --- Ausdrücke (Pratt) ---
    fn parse_expr(&mut self, min_bp: u8) -> Expr {
        let mut lhs = self.parse_prefix();
        loop {
            // Postfix (höchste Bindung): . ( [ ? as
            lhs = self.parse_postfix(lhs);
            // Bereich `a..b` / `a..=b` — niedrigste Bindung (bp 1), nicht-assoziativ.
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

    /// Aktueller Infix-Operator + linke Bindungsstärke.
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
                    self.parse_expr(9)
                };
                Expr::Comptime { inner: Box::new(inner), span: sp }
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_postfix(&mut self, mut e: Expr) -> Expr {
        loop {
            // Leading-dot-Chains über Newlines hinweg zulassen:
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
            // benanntes Argument `name: expr` → Name für M1 verworfen
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
                // Compiler-Intrinsic @name(...) — als Call auf Ident "@name"
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
                // Listen-Literal
                self.bump();
                self.skip_nl();
                let mut items = Vec::new();
                while !self.at(&Tok::RBracket) && !matches!(self.peek(), Tok::Eof) {
                    items.push(self.parse_expr(0));
                    // Comprehension `[e for …]` – für M1 nicht unterstützt
                    if self.at_kw(Kw::For) {
                        self.err("List-Comprehension noch nicht unterstützt (M1)");
                        while !self.at(&Tok::RBracket) && !matches!(self.peek(), Tok::Eof) {
                            self.bump();
                        }
                        break;
                    }
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                    self.skip_nl();
                }
                self.expect(&Tok::RBracket, "']'");
                Expr::List(items, sp)
            }
            Tok::LBrace => Expr::Block(self.parse_block()),
            other => {
                self.err(&format!("unerwartet in Ausdruck: {other:?}"));
                self.bump();
                Expr::Int(0, sp)
            }
        }
    }

    fn parse_paren_or_lambda(&mut self) -> Expr {
        let sp = self.span();
        // `(a, b) -> e` Lambda vs. `(e)` Klammerung: bis zur passenden `)` scannen
        // und prüfen, ob danach `->` kommt.
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
                // Konstruktor mit Feldern? `Name(p, …)` oder Pfad `Type.Variant`
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
                self.err(&format!("unerwartet in Muster: {other:?}"));
                self.bump();
                Pattern::Wildcard(sp)
            }
        };
        // Oder-Muster `A | B`
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

/// Bequemer Einstieg: Quelltext → (Modul, Diagnosen).
/// Baut aus gesammelten Top-Level-Anweisungen ein implizites `fn main()`.
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

/// Wie `parse`, aber mit nutzerdefinierter Schlüsselwort-Schreibweise.
pub fn parse_with_syntax(src: &str, syntax: crate::syntax::Syntax) -> (Module, Vec<Diag>) {
    let (toks, mut diags) = crate::lexer::Lexer::with_syntax(src, syntax).lex();
    let mut p = Parser::new(toks);
    let m = p.parse_module();
    diags.append(&mut p.diags);
    (m, diags)
}
