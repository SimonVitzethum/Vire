//! Vire parser: `Vec<Token> → ast::Module`. Recursive descent for items/
//! statements, Pratt (precedence climbing) for expressions. See language/PARSER.md.

use crate::ast::*;
use crate::diag::{Diag, Span};
use crate::lexer::{Kw, Tok, Token};

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
    pub diags: Vec<Diag>,
    /// True while parsing the body items of an item macro — enables the `##`
    /// token-paste operator to defer concatenation until expansion (when the
    /// parameter substitution is known).
    in_macro_body: bool,
}

impl Parser {
    pub fn new(toks: Vec<Token>) -> Self {
        Parser { toks, pos: 0, diags: Vec::new(), in_macro_body: false }
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
    /// Read an identifier, honoring the `##` token-paste operator (`A ## B`).
    /// Fragments are joined with the reserved sentinel `\u{1}`; item-macro
    /// expansion resolves each fragment (a parameter → its `ident` argument,
    /// otherwise literal) and concatenates them into one identifier. Outside a
    /// macro body every fragment is literal, so they are joined immediately —
    /// no sentinel ever reaches lowering.
    fn paste_ident(&mut self) -> String {
        let mut parts = vec![self.ident()];
        while self.at(&Tok::HashHash) {
            self.bump();
            parts.push(self.ident());
        }
        if parts.len() == 1 {
            parts.pop().unwrap()
        } else if self.in_macro_body {
            parts.join("\u{1}")
        } else {
            parts.concat()
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
            // `@derive(...)`/`@when(...)`/`@gpu` and the `@vulkan` shader stages
            // (`@vertex`/`@fragment`/`@compute`/`@task`/`@mesh`) introduce a
            // declaration item (other `@…` stay expressions/script statements, e.g.
            // inline `@c`/`@asm` blocks).
            || (matches!(self.peek(), Tok::At) && matches!(self.peek_at(1), Tok::Ident(n) if matches!(n.as_str(), "derive" | "when" | "gpu" | "vertex" | "fragment" | "compute" | "task" | "mesh" | "gpuvk")))
            // `name!(…)` — an item-macro invocation.
            || (matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::Bang))
    }

    fn parse_item(&mut self) -> Option<Item> {
        // Leading declaration attributes: `@derive(Eq, Show)` etc. Currently only
        // meaningful on a `type` (attached below); on anything else → a diagnostic.
        let attrs = self.parse_attrs();
        let is_pub = self.eat_kw(Kw::Pub);
        if !attrs.is_empty() && !matches!(self.peek(), Tok::Kw(Kw::Type) | Tok::Kw(Kw::Fn)) {
            self.err("attributes (@derive/@when) are currently only supported on `type` and `fn` declarations");
        }
        match self.peek() {
            Tok::Kw(Kw::Fn) => Some(Item::Fn(self.parse_fn_attrs(is_pub, attrs))),
            Tok::Kw(Kw::Native) => Some(self.parse_native()),
            Tok::Kw(Kw::Type) => {
                let mut t = self.parse_type_def();
                t.attrs = attrs;
                Some(Item::Type(t))
            }
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
            // `name!(args)` — item-macro invocation.
            Tok::Ident(_) if matches!(self.peek_at(1), Tok::Bang) => Some(self.parse_macro_invoke()),
            Tok::Kw(Kw::Macro) => Some(self.parse_macro_def()),
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
        let name = self.paste_ident();
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
        self.parse_fn_attrs(is_pub, vec![])
    }

    fn parse_fn_attrs(&mut self, is_pub: bool, attrs: Vec<Attr>) -> FnDef {
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
        FnDef { sig, body, is_pub, attrs }
    }

    fn parse_type_def(&mut self) -> TypeDef {
        let sp = self.span();
        self.expect(&Tok::Kw(Kw::Type), "'type'");
        let name = self.paste_ident();
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
        TypeDef { name, generics, fields, variants, methods, attrs: Vec::new(), span: sp }
    }

    /// `macro name(params) = <expr>` (expression macro) OR
    /// `macro name(params) { <items> }` (hygienic item macro). Parameters may be
    /// kind-typed (`p: type|ident|expr`); the branch is chosen by `=` vs `{`.
    fn parse_macro_def(&mut self) -> Item {
        let sp = self.span();
        self.bump(); // `macro`
        let name = self.ident();
        self.expect(&Tok::LParen, "'('");
        let mut raw: Vec<(String, Option<ParamKind>, crate::diag::Span)> = Vec::new();
        self.skip_nl();
        while !self.at(&Tok::RParen) && !matches!(self.peek(), Tok::Eof) {
            let psp = self.span();
            let pname = self.ident();
            let kind = if self.eat(&Tok::Colon) {
                // `type` is a keyword; `ident`/`expr` are plain identifiers.
                if self.eat_kw(Kw::Type) {
                    Some(ParamKind::Type)
                } else {
                    let k = self.ident();
                    match k.as_str() {
                        "ident" => Some(ParamKind::Ident),
                        "expr" => Some(ParamKind::Expr),
                        "block" => Some(ParamKind::Block),
                        "pat" => Some(ParamKind::Pat),
                        _ => {
                            self.err("macro parameter kind must be `type`, `ident`, `expr`, `block`, or `pat`");
                            Some(ParamKind::Expr)
                        }
                    }
                }
            } else {
                None
            };
            raw.push((pname, kind, psp));
            if !self.eat(&Tok::Comma) {
                break;
            }
            self.skip_nl();
        }
        self.expect(&Tok::RParen, "')'");
        if self.eat(&Tok::Eq) {
            // Expression macro (kinds, if any, are not meaningful here).
            let params = raw.into_iter().map(|(n, _, _)| n).collect();
            let body = self.parse_expr(0);
            Item::Macro { name, params, body, span: sp }
        } else {
            // Item macro: a braced sequence of items.
            let params = raw
                .into_iter()
                .map(|(n, k, s)| MacroParam { name: n, kind: k.unwrap_or(ParamKind::Expr), span: s })
                .collect();
            self.expect(&Tok::LBrace, "'{' or '=' after macro parameters");
            self.stmt_end();
            let mut items = Vec::new();
            let prev = self.in_macro_body;
            self.in_macro_body = true;
            while !self.at(&Tok::RBrace) && !matches!(self.peek(), Tok::Eof) {
                let before = self.pos;
                if let Some(it) = self.parse_item() {
                    items.push(it);
                }
                self.stmt_end();
                // Always make progress: a malformed body item (e.g. a stray token
                // that starts no item) must never spin the loop — otherwise the
                // diagnostics vec grows without bound until OOM.
                if self.pos == before {
                    self.err("unexpected token in macro body");
                    self.bump();
                }
            }
            self.in_macro_body = prev;
            self.expect(&Tok::RBrace, "'}'");
            Item::ItemMacro { name, params, items, span: sp }
        }
    }

    /// `name!(arg, …)` — invoke an item macro. Arguments parse as expressions; the
    /// expander later checks each against its parameter's declared kind.
    fn parse_macro_invoke(&mut self) -> Item {
        let sp = self.span();
        let name = self.ident();
        self.expect(&Tok::Bang, "'!'");
        self.expect(&Tok::LParen, "'('");
        let mut args = Vec::new();
        self.skip_nl();
        while !self.at(&Tok::RParen) && !matches!(self.peek(), Tok::Eof) {
            args.push(self.parse_expr(0));
            if !self.eat(&Tok::Comma) {
                break;
            }
            self.skip_nl();
        }
        self.expect(&Tok::RParen, "')'");
        Item::MacroInvoke { name, args, span: sp }
    }

    /// Parse leading `@name(arg, …)` declaration attributes (bare-ident args).
    fn parse_attrs(&mut self) -> Vec<Attr> {
        let mut attrs = Vec::new();
        while self.at(&Tok::At) {
            let sp = self.span();
            self.bump(); // '@'
            let name = self.ident();
            let mut args = Vec::new();
            if self.eat(&Tok::LParen) {
                self.skip_nl();
                while !self.at(&Tok::RParen) && !matches!(self.peek(), Tok::Eof) {
                    args.push(self.ident());
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                    self.skip_nl();
                }
                self.expect(&Tok::RParen, "')'");
            }
            attrs.push(Attr { name, args, span: sp });
            self.skip_nl(); // allow a newline between the attribute and the item
        }
        attrs
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
            // `Base ## Box` — a pasted type name (e.g. referencing a type a macro
            // generated). Paste-aware like fn/type-def names.
            _ => self.paste_ident(),
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
            // `with log.span(k, v, …) { … }` — scoped logger context: every `log.*`
            // inside the block prepends these fields. Compile-time only (the push/pop
            // are markers the lowering consumes → zero runtime cost). Desugars to a
            // block bracketed by `__log_span_push(args)` / `__log_span_pop()`.
            Tok::Kw(Kw::With) => {
                self.bump();
                let head = self.parse_expr(0); // expects `log.span(args)`
                let args = match &head {
                    Expr::Call { callee, args, .. } if matches!(callee.as_ref(), Expr::Field { base, name, .. } if name == "span" && matches!(base.as_ref(), Expr::Ident(n, _) if n == "log")) => {
                        args.clone()
                    }
                    _ => {
                        self.err("`with` expects `with log.span(field, value, …) { … }`");
                        vec![]
                    }
                };
                let body = self.parse_block();
                let call = |n: &str, a: Vec<Expr>| Expr::Call { callee: Box::new(Expr::Ident(n.into(), sp)), args: a, span: sp };
                let mut stmts = vec![Stmt::Expr(call("__log_span_push", args))];
                stmts.extend(body.stmts);
                // The block's tail (its last expression) must run BEFORE the pop — the
                // parser turns a trailing `log.*` into the block value; fold it in as a
                // statement so the whole body is inside the span. The `with` is a
                // statement, so the block value is discarded anyway.
                if let Some(t) = body.tail {
                    stmts.push(Stmt::Expr(*t));
                }
                stmts.push(Stmt::Expr(call("__log_span_pop", vec![])));
                Stmt::Expr(Expr::Block(Block { stmts, tail: None, span: sp }))
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
                // Optional `: Type` annotation (an escape hatch for inference).
                let ty = if self.eat(&Tok::Colon) { Some(self.parse_type()) } else { None };
                let value = if self.eat(&Tok::Eq) { Some(self.parse_expr(0)) } else { None };
                Stmt::Let { mutable: true, name, ty, value, span: sp }
            }
            // `name : Type = expr` (annotated binding) — lookahead for `ident :`.
            Tok::Ident(_) if matches!(self.peek_at(1), Tok::Colon) => {
                let name = self.ident();
                self.bump(); // :
                let ty = Some(self.parse_type());
                self.expect(&Tok::Eq, "'='");
                let value = self.parse_expr(0);
                Stmt::Let { mutable: false, name, ty, value: Some(value), span: sp }
            }
            // `name = expr` (binding) vs. expression: lookahead for `ident =`
            Tok::Ident(_) if matches!(self.peek_at(1), Tok::Eq) => {
                let name = self.ident();
                self.bump(); // =
                let value = self.parse_expr(0);
                Stmt::Let { mutable: false, name, ty: None, value: Some(value), span: sp }
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

    /// Body of a lambda (`x -> …`). An expression body is returned as-is; a **braceless
    /// statement body** — an assignment `x -> total = total + x` — is wrapped in a
    /// unit-valued block so statement-bodied lambdas work without explicit `{ … }`
    /// (used by `each`/`forEach`). A `{ … }` body already parses as a block expression.
    fn parse_lambda_body(&mut self) -> Expr {
        let sp = self.span();
        let e = self.parse_expr(0);
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
            let assign = Stmt::Assign { target: e, op: o, value, span: sp };
            Expr::Block(Block { stmts: vec![assign], tail: None, span: sp })
        } else {
            e
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
                } else if self.at_kw(Kw::For) {
                    // `comptime for i in a..b { … }` — a comptime block wrapping the
                    // for-loop, which the comptime pass UNROLLS into runtime statements
                    // (the loop variable substituted by each literal). Bounded metaprog.
                    let for_stmt = self.parse_stmt();
                    Expr::Block(Block { stmts: vec![for_stmt], tail: None, span: sp })
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
                    // `f[T, N](args)` turbofish (explicit generic args) vs `a[i]`
                    // indexing. Only a bare-identifier base can be a turbofish; the
                    // trailing `(` after `]` disambiguates (an array element is never
                    // callable in Vire). The bracket content parses the same for both.
                    self.bump();
                    let mut targs = vec![self.parse_expr(0)];
                    let multi = self.eat(&Tok::Comma);
                    if multi {
                        loop {
                            targs.push(self.parse_expr(0));
                            if !self.eat(&Tok::Comma) {
                                break;
                            }
                        }
                    }
                    self.expect(&Tok::RBracket, "']'");
                    if let (Expr::Ident(name, _), Tok::LParen) = (&e, self.peek()) {
                        let name = name.clone();
                        let args = self.parse_call_args();
                        e = Expr::TurboCall { callee: name, targs, args, span: sp };
                    } else if !multi {
                        let index = targs.pop().unwrap();
                        e = Expr::Index { base: Box::new(e), index: Box::new(index), span: sp };
                    } else {
                        self.err("`[a, b]` is only valid as turbofish `f[..](..)`");
                        e = Expr::Index { base: Box::new(e), index: Box::new(targs.pop().unwrap()), span: sp };
                    }
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
                self.interpolate(&s, sp)
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
            Tok::Kw(Kw::Spawn) => {
                // `spawn f(arg)` — the inner call runs on a new thread. Parse the
                // call at primary+postfix level so `f(arg)` is captured (but not a
                // trailing binary operator).
                self.bump();
                let prim = self.parse_primary();
                let inner = self.parse_postfix(prim);
                Expr::Spawn { call: Box::new(inner), span: sp }
            }
            Tok::At => {
                // compiler intrinsic @name(...) — as a call on ident "@name"
                self.bump();
                let name = format!("@{}", self.ident());
                Expr::Ident(name, sp)
            }
            // Declarative `frame { bg(r, g, b) }` — a render frame described by
            // directives (first: `bg` = the clear/background colour). Desugars at parse
            // time to a `vk_frame_bg(r, g, b)` builtin call. Not a reserved word: only
            // `frame` immediately followed by `{` triggers this, so `frame` stays a
            // usable identifier everywhere else.
            Tok::Ident(ref n) if n == "frame" && matches!(self.peek_at(1), Tok::LBrace) => {
                self.bump(); // frame
                self.expect(&Tok::LBrace, "'{'");
                self.skip_nl();
                let (r, g, b) = if matches!(self.peek(), Tok::Ident(d) if d == "bg") {
                    self.bump(); // bg
                    self.expect(&Tok::LParen, "'('");
                    let r = self.parse_expr(0); self.expect(&Tok::Comma, "','");
                    let g = self.parse_expr(0); self.expect(&Tok::Comma, "','");
                    let b = self.parse_expr(0); self.expect(&Tok::RParen, "')'");
                    (r, g, b)
                } else {
                    (Expr::Float(0.08, sp), Expr::Float(0.08, sp), Expr::Float(0.10, sp))
                };
                self.skip_nl();
                self.expect(&Tok::RBrace, "'}'");
                Expr::Call { callee: Box::new(Expr::Ident("vk_frame_bg".into(), sp)), args: vec![r, g, b], span: sp }
            }
            Tok::Ident(_) => {
                // Lambda `x -> e`?
                if matches!(self.peek_at(1), Tok::Arrow) {
                    let p = self.ident();
                    self.bump(); // ->
                    let body = self.parse_lambda_body();
                    Expr::Lambda { params: vec![p], body: Box::new(body), span: sp }
                } else {
                    // `paste_ident` folds `Base ## _get` into one identifier so a
                    // pasted name works as a call target / value reference.
                    Expr::Ident(self.paste_ident(), sp)
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
            let body = self.parse_lambda_body();
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
            // Allow `else`/`elif` to start on the line *after* the closing `}`
            // (`}\n else {`), not only `} else {`. Skip the soft-newline terminators
            // ONLY when an else/elif actually follows — otherwise a bare `if`
            // statement's own terminating newline must stay unconsumed.
            let mut k = 0;
            while matches!(self.peek_at(k), Tok::Newline) {
                k += 1;
            }
            if matches!(self.peek_at(k), Tok::Kw(Kw::Elif) | Tok::Kw(Kw::Else)) {
                for _ in 0..k {
                    self.bump();
                }
            }
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

    /// String interpolation (C1): `"a{expr}b"` desugars to `"a" + str(expr) + "b"`.
    /// - `{{` / `}}` are literal braces.
    /// - `{}` (empty) is left literal — the logger's positional placeholder still works.
    /// - a `{…}` whose contents don't parse cleanly as an expression is kept literal,
    ///   so existing brace-containing strings never break (backward-compatible).
    /// Only expression-position strings pass through here; extern/native/header strings
    /// are read raw elsewhere and are never interpolated.
    fn interpolate(&mut self, s: &str, sp: Span) -> Expr {
        if !s.contains('{') {
            // No interpolation possible; still fold `}}` → `}` for symmetry.
            if s.contains("}}") {
                return Expr::Str(s.replace("}}", "}"), sp);
            }
            return Expr::Str(s.to_string(), sp);
        }
        let chars: Vec<char> = s.chars().collect();
        let mut parts: Vec<Expr> = Vec::new();
        let mut lit = String::new();
        let mut i = 0;
        let flush = |lit: &mut String, parts: &mut Vec<Expr>| {
            if !lit.is_empty() {
                parts.push(Expr::Str(std::mem::take(lit), sp));
            }
        };
        while i < chars.len() {
            let c = chars[i];
            if c == '{' {
                if i + 1 < chars.len() && chars[i + 1] == '{' {
                    lit.push('{');
                    i += 2;
                    continue;
                }
                // Find the matching close brace on this segment.
                if let Some(rel) = chars[i + 1..].iter().position(|&c| c == '}') {
                    let inner: String = chars[i + 1..i + 1 + rel].iter().collect();
                    if !inner.trim().is_empty() {
                        if let Some(e) = self.try_parse_fragment(&inner) {
                            flush(&mut lit, &mut parts);
                            parts.push(Expr::Call {
                                callee: Box::new(Expr::Ident("str".into(), sp)),
                                args: vec![e],
                                span: sp,
                            });
                            i += 1 + rel + 1;
                            continue;
                        }
                    }
                }
                // `{}` (empty), no close, or an unparseable fragment → literal `{`.
                lit.push('{');
                i += 1;
            } else if c == '}' {
                if i + 1 < chars.len() && chars[i + 1] == '}' {
                    lit.push('}');
                    i += 2;
                } else {
                    lit.push('}');
                    i += 1;
                }
            } else {
                lit.push(c);
                i += 1;
            }
        }
        if parts.is_empty() {
            return Expr::Str(lit, sp); // nothing interpolated
        }
        flush(&mut lit, &mut parts);
        let mut it = parts.into_iter();
        let mut acc = it.next().unwrap();
        for p in it {
            acc = Expr::Binary { op: BinOp::Add, lhs: Box::new(acc), rhs: Box::new(p), span: sp };
        }
        acc
    }

    /// Lex+parse an interpolation fragment as a standalone expression. Returns `None`
    /// (→ keep the braces literal) if it doesn't lex/parse cleanly or leaves tokens
    /// over, so a non-expression `{…}` never turns into a hard error.
    fn try_parse_fragment(&self, src: &str) -> Option<Expr> {
        let (toks, diags) = crate::lexer::lex(src);
        if !diags.is_empty() {
            return None;
        }
        let mut p = Parser::new(toks);
        let e = p.parse_expr(0);
        if !p.diags.is_empty() {
            return None;
        }
        if !matches!(p.peek(), Tok::Eof | Tok::Newline) {
            return None;
        }
        Some(e)
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
        attrs: vec![],
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
