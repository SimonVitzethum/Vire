//! Vire lexer: `&str → Vec<Token>`.
//!
//! Peculiarities (see language/PARSER.md §2): newline is a *soft*
//! statement terminator (like Go — only after tokens that can end a statement),
//! generics go in `[]` (never `<>`), comments `//` and nestable
//! `/* */`, numbers with `_`/base/suffix, strings with escapes.

use crate::diag::{Diag, Span};

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    // literals
    Int(i128),
    Float(f64),
    Str(String),
    Char(char),
    // identifiers / keywords
    Ident(String),
    Kw(Kw),
    // brackets & separators
    LParen, RParen, LBracket, RBracket, LBrace, RBrace,
    Comma, Colon, Semi, Arrow, FatArrow, Dot, DotDot, DotDotEq, At, Question, Bang,
    // operators
    Plus, Minus, Star, Slash, Percent, PlusPct, MinusPct, StarPct,
    EqEq, Ne, Lt, Le, Gt, Ge,
    Amp, Pipe, Caret, Shl, Shr,
    Eq, PlusEq, MinusEq, StarEq, SlashEq,
    // control
    Newline, Eof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kw {
    Fn, Type, Trait, Impl, Mut, Const, Use, Pub, Extern, Unsafe, Macro, Comptime,
    Match, If, Elif, Else, While, For, In, Break, Continue, Return, Spawn, Capsule,
    And, Or, Not, As, True, False, SelfLower, SelfType, Native,
}

/// Canonical default spelling of each keyword. Single source of truth —
/// both `from_ident` AND the configurable `Syntax` table derive from this.
pub const KW_TABLE: &[(&str, Kw)] = {
    use Kw::*;
    &[
        ("fn", Fn), ("type", Type), ("trait", Trait), ("impl", Impl),
        ("mut", Mut), ("const", Const), ("use", Use), ("pub", Pub),
        ("extern", Extern), ("unsafe", Unsafe), ("macro", Macro),
        ("comptime", Comptime), ("match", Match), ("if", If), ("elif", Elif),
        ("else", Else), ("while", While), ("for", For), ("in", In),
        ("break", Break), ("continue", Continue), ("return", Return),
        ("spawn", Spawn), ("capsule", Capsule), ("native", Native),
        ("and", And), ("or", Or), ("not", Not), ("as", As),
        ("true", True), ("false", False), ("self", SelfLower), ("Self", SelfType),
    ]
};

impl Kw {
    /// Canonical name (for config files and reverse mapping).
    pub fn canonical(self) -> &'static str {
        KW_TABLE.iter().find(|(_, k)| *k == self).map(|(sp, _)| *sp).unwrap_or("?")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

/// After this token, can a newline end a statement? (Go-like rule.)
fn ends_stmt(t: &Tok) -> bool {
    matches!(
        t,
        Tok::Int(_) | Tok::Float(_) | Tok::Str(_) | Tok::Char(_) | Tok::Ident(_)
            | Tok::RParen | Tok::RBracket | Tok::RBrace | Tok::Question
            | Tok::Kw(Kw::Break) | Tok::Kw(Kw::Continue) | Tok::Kw(Kw::Return)
            | Tok::Kw(Kw::True) | Tok::Kw(Kw::False) | Tok::Kw(Kw::SelfLower)
    )
}

pub struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    pub diags: Vec<Diag>,
    syntax: crate::syntax::Syntax,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Lexer { src: src.as_bytes(), pos: 0, diags: Vec::new(), syntax: Default::default() }
    }
    /// Lexer with user-defined keyword spelling.
    pub fn with_syntax(src: &'a str, syntax: crate::syntax::Syntax) -> Self {
        Lexer { src: src.as_bytes(), pos: 0, diags: Vec::new(), syntax }
    }

    fn peek(&self) -> u8 {
        *self.src.get(self.pos).unwrap_or(&0)
    }
    fn peek2(&self) -> u8 {
        *self.src.get(self.pos + 1).unwrap_or(&0)
    }
    fn bump(&mut self) -> u8 {
        let c = self.peek();
        self.pos += 1;
        c
    }

    pub fn lex(mut self) -> (Vec<Token>, Vec<Diag>) {
        let mut out: Vec<Token> = Vec::new();
        loop {
            self.skip_trivia(&mut out);
            let start = self.pos;
            if self.pos >= self.src.len() {
                out.push(Token { tok: Tok::Eof, span: Span(start, start) });
                break;
            }
            if self.try_inline(&mut out) {
                continue;
            }
            let tok = self.next_token();
            out.push(Token { tok, span: Span(start, self.pos) });
        }
        (out, self.diags)
    }

    /// Sugar for first-class inline blocks: `inline:c(cap1, cap2) { …C… }` and
    /// `inline:asm(cap) { …asm… }` lex into the same token stream as the intrinsic
    /// form `@c(""" …C… """, cap1, cap2)` — the body is captured RAW (the Vire lexer
    /// never tokenizes the foreign code), so `->`, `%`, `#include`, `{}` all pass
    /// through untouched. Parser and the @c/@asm desugar are unchanged.
    fn try_inline(&mut self, out: &mut Vec<Token>) -> bool {
        let lang = if self.src[self.pos..].starts_with(b"inline:c(") {
            "c"
        } else if self.src[self.pos..].starts_with(b"inline:asm(") {
            "asm"
        } else {
            return false;
        };
        let start = self.pos;
        self.pos += "inline:".len() + lang.len() + 1; // past `inline:LANG(`
        // Capture list: raw text up to the matching ')'.
        let cap_start = self.pos;
        while self.pos < self.src.len() && self.src[self.pos] != b')' {
            self.pos += 1;
        }
        let caps_raw = String::from_utf8_lossy(&self.src[cap_start..self.pos]).into_owned();
        self.pos += 1; // ')'
        while self.pos < self.src.len() && (self.src[self.pos] as char).is_whitespace() {
            self.pos += 1;
        }
        if self.src.get(self.pos) != Some(&b'{') {
            self.diags.push(Diag::error("inline:c/asm: expected `{` before the code body", Span(start, self.pos)));
            return true;
        }
        self.pos += 1; // '{'
        let body_start = self.pos;
        let mut depth = 1;
        while self.pos < self.src.len() {
            match self.src[self.pos] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            self.pos += 1;
        }
        let body = String::from_utf8_lossy(&self.src[body_start..self.pos]).into_owned();
        self.pos += 1; // closing '}'
        let sp = Span(start, self.pos);
        // Emit @LANG("""body""", cap1, cap2, …).
        out.push(Token { tok: Tok::At, span: sp });
        out.push(Token { tok: Tok::Ident(lang.into()), span: sp });
        out.push(Token { tok: Tok::LParen, span: sp });
        out.push(Token { tok: Tok::Str(body), span: sp });
        for cap in caps_raw.split(',').map(|c| c.trim()).filter(|c| !c.is_empty()) {
            out.push(Token { tok: Tok::Comma, span: sp });
            out.push(Token { tok: Tok::Ident(cap.into()), span: sp });
        }
        out.push(Token { tok: Tok::RParen, span: sp });
        true
    }

    /// Skip whitespace/comments; append significant newlines as tokens to
    /// `out` (only when the last token can end a statement).
    fn skip_trivia(&mut self, out: &mut Vec<Token>) {
        loop {
            match self.peek() {
                b' ' | b'\t' | b'\r' => {
                    self.pos += 1;
                }
                b'\n' => {
                    let at = self.pos;
                    self.pos += 1;
                    if out.last().map(|t| ends_stmt(&t.tok)).unwrap_or(false) {
                        out.push(Token { tok: Tok::Newline, span: Span(at, at + 1) });
                    }
                }
                b'/' if self.peek2() == b'/' => {
                    while self.peek() != b'\n' && self.pos < self.src.len() {
                        self.pos += 1;
                    }
                }
                b'/' if self.peek2() == b'*' => {
                    self.pos += 2;
                    let mut depth = 1;
                    while depth > 0 && self.pos < self.src.len() {
                        if self.peek() == b'/' && self.peek2() == b'*' {
                            self.pos += 2;
                            depth += 1;
                        } else if self.peek() == b'*' && self.peek2() == b'/' {
                            self.pos += 2;
                            depth -= 1;
                        } else {
                            self.pos += 1;
                        }
                    }
                }
                _ => break,
            }
        }
    }

    fn next_token(&mut self) -> Tok {
        let c = self.peek();
        if c == b'_' || c.is_ascii_alphabetic() || c >= 0x80 {
            return self.ident_or_kw();
        }
        if c.is_ascii_digit() {
            return self.number();
        }
        match c {
            b'"' => self.string(),
            b'\'' => self.char_lit(),
            _ => self.operator(),
        }
    }

    fn ident_or_kw(&mut self) -> Tok {
        let start = self.pos;
        while {
            let c = self.peek();
            c == b'_' || c.is_ascii_alphanumeric() || c >= 0x80
        } {
            self.pos += 1;
        }
        let s = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("").to_string();
        match self.syntax.keyword(&s) {
            Some(kw) => Tok::Kw(kw),
            None => Tok::Ident(s),
        }
    }

    fn number(&mut self) -> Tok {
        let start = self.pos;
        // base prefix
        if self.peek() == b'0' && matches!(self.peek2(), b'x' | b'X' | b'b' | b'B' | b'o' | b'O') {
            let base_ch = self.peek2();
            self.pos += 2;
            let base = match base_ch {
                b'x' | b'X' => 16,
                b'b' | b'B' => 2,
                _ => 8,
            };
            let ds = self.pos;
            while self.peek() == b'_' || (self.peek() as char).is_digit(base) {
                self.pos += 1;
            }
            let digits: String = std::str::from_utf8(&self.src[ds..self.pos])
                .unwrap_or("").chars().filter(|c| *c != '_').collect();
            self.skip_num_suffix();
            return match i128::from_str_radix(&digits, base) {
                Ok(v) => Tok::Int(v),
                Err(_) => {
                    self.diags.push(Diag::error("invalid number", Span(start, self.pos)));
                    Tok::Int(0)
                }
            };
        }
        // decimal, possibly float
        while self.peek() == b'_' || self.peek().is_ascii_digit() {
            self.pos += 1;
        }
        let mut is_float = false;
        if self.peek() == b'.' && self.peek2().is_ascii_digit() {
            is_float = true;
            self.pos += 1;
            while self.peek() == b'_' || self.peek().is_ascii_digit() {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), b'e' | b'E') {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), b'+' | b'-') {
                self.pos += 1;
            }
            while self.peek().is_ascii_digit() {
                self.pos += 1;
            }
        }
        let raw: String = std::str::from_utf8(&self.src[start..self.pos])
            .unwrap_or("").chars().filter(|c| *c != '_').collect();
        self.skip_num_suffix();
        if is_float {
            Tok::Float(raw.parse().unwrap_or(0.0))
        } else {
            Tok::Int(raw.parse().unwrap_or(0))
        }
    }

    fn skip_num_suffix(&mut self) {
        // i8/i16/i32/i64/u8/.../f32/f64 – for M1 just consume.
        if matches!(self.peek(), b'i' | b'u' | b'f') {
            self.pos += 1;
            while self.peek().is_ascii_digit() {
                self.pos += 1;
            }
        }
    }

    fn string(&mut self) -> Tok {
        // Multi-line raw string `"""…"""`: no escapes, ideal for embedded
        // foreign code (native blocks) and long texts.
        if self.peek() == b'"' && self.peek2() == b'"' && self.src.get(self.pos + 2) == Some(&b'"') {
            let open = self.pos;
            self.pos += 3;
            let start = self.pos;
            while self.pos < self.src.len() {
                if self.peek() == b'"' && self.peek2() == b'"' && self.src.get(self.pos + 2) == Some(&b'"') {
                    let raw = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("").to_string();
                    self.pos += 3;
                    return Tok::Str(raw);
                }
                self.pos += 1;
            }
            self.diags.push(Diag::error("unterminated \"\"\"-string", Span(open, self.pos)));
            return Tok::Str(std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("").to_string());
        }
        let start = self.pos;
        self.pos += 1; // "
        let mut s = String::new();
        loop {
            let c = self.peek();
            if c == 0 || c == b'"' {
                break;
            }
            if c == b'\\' {
                self.pos += 1;
                let e = self.bump();
                s.push(match e {
                    b'n' => '\n', b't' => '\t', b'r' => '\r', b'\\' => '\\',
                    b'"' => '"', b'0' => '\0', b'{' => '{', b'}' => '}',
                    other => other as char,
                });
            } else {
                s.push(self.bump() as char);
            }
        }
        if self.peek() == b'"' {
            self.pos += 1;
        } else {
            self.diags.push(Diag::error("unterminated string", Span(start, self.pos)));
        }
        Tok::Str(s)
    }

    fn char_lit(&mut self) -> Tok {
        let start = self.pos;
        self.pos += 1; // '
        let ch = if self.peek() == b'\\' {
            self.pos += 1;
            match self.bump() {
                b'n' => '\n', b't' => '\t', b'r' => '\r', b'\\' => '\\',
                b'\'' => '\'', b'0' => '\0', other => other as char,
            }
        } else {
            self.bump() as char
        };
        if self.peek() == b'\'' {
            self.pos += 1;
        } else {
            self.diags.push(Diag::error("unterminated char literal", Span(start, self.pos)));
        }
        Tok::Char(ch)
    }

    fn operator(&mut self) -> Tok {
        let a = self.bump();
        let b = self.peek();
        macro_rules! two {
            ($snd:expr, $t:expr) => {
                if b == $snd {
                    self.pos += 1;
                    return $t;
                }
            };
        }
        match a {
            b'(' => Tok::LParen,
            b')' => Tok::RParen,
            b'[' => Tok::LBracket,
            b']' => Tok::RBracket,
            b'{' => Tok::LBrace,
            b'}' => Tok::RBrace,
            b',' => Tok::Comma,
            b':' => Tok::Colon,
            b';' => Tok::Semi,
            b'@' => Tok::At,
            b'?' => Tok::Question,
            b'^' => Tok::Caret,
            b'.' => {
                if b == b'.' {
                    self.pos += 1;
                    if self.peek() == b'=' {
                        self.pos += 1;
                        return Tok::DotDotEq;
                    }
                    return Tok::DotDot;
                }
                Tok::Dot
            }
            b'-' => {
                two!(b'>', Tok::Arrow);
                two!(b'=', Tok::MinusEq);
                two!(b'%', Tok::MinusPct);
                Tok::Minus
            }
            b'+' => {
                two!(b'=', Tok::PlusEq);
                two!(b'%', Tok::PlusPct);
                Tok::Plus
            }
            b'*' => {
                two!(b'=', Tok::StarEq);
                two!(b'%', Tok::StarPct);
                Tok::Star
            }
            b'/' => {
                two!(b'=', Tok::SlashEq);
                Tok::Slash
            }
            b'%' => Tok::Percent,
            b'=' => {
                two!(b'=', Tok::EqEq);
                two!(b'>', Tok::FatArrow);
                Tok::Eq
            }
            b'!' => {
                two!(b'=', Tok::Ne);
                // A lone `!` is a macro invocation marker (`name!(…)`); boolean
                // negation is the `not` keyword, so there is no ambiguity.
                Tok::Bang
            }
            b'<' => {
                two!(b'=', Tok::Le);
                two!(b'<', Tok::Shl);
                Tok::Lt
            }
            b'>' => {
                two!(b'=', Tok::Ge);
                two!(b'>', Tok::Shr);
                Tok::Gt
            }
            b'&' => Tok::Amp,
            b'|' => Tok::Pipe,
            other => {
                self.diags.push(Diag::error(
                    &format!("unexpected character '{}'", other as char),
                    Span(self.pos - 1, self.pos),
                ));
                // treat as whitespace: the next call continues
                Tok::Newline
            }
        }
    }
}

/// Convenient entry point.
pub fn lex(src: &str) -> (Vec<Token>, Vec<Diag>) {
    Lexer::new(src).lex()
}
