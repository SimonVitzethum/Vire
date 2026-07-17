//! Vire-Lexer: `&str → Vec<Token>`.
//!
//! Besonderheiten (siehe sprache/PARSER.md §2): Newline ist ein *weicher*
//! Anweisungsterminator (wie Go — nur nach Tokens, die eine Anweisung beenden
//! können), Generics stehen in `[]` (nie `<>`), Kommentare `//` und schachtelbare
//! `/* */`, Zahlen mit `_`/Basis/Suffix, Strings mit Escapes.

use crate::diag::{Diag, Span};

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    // Literale
    Int(i128),
    Float(f64),
    Str(String),
    Char(char),
    // Bezeichner / Schlüsselwörter
    Ident(String),
    Kw(Kw),
    // Klammern & Trenner
    LParen, RParen, LBracket, RBracket, LBrace, RBrace,
    Comma, Colon, Semi, Arrow, FatArrow, Dot, DotDot, DotDotEq, At, Question,
    // Operatoren
    Plus, Minus, Star, Slash, Percent, PlusPct, MinusPct, StarPct,
    EqEq, Ne, Lt, Le, Gt, Ge,
    Amp, Pipe, Caret, Shl, Shr,
    Eq, PlusEq, MinusEq, StarEq, SlashEq,
    // Gesteuert
    Newline, Eof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kw {
    Fn, Type, Trait, Impl, Mut, Const, Use, Pub, Extern, Unsafe, Macro, Comptime,
    Match, If, Elif, Else, While, For, In, Break, Continue, Return, Spawn,
    And, Or, Not, As, True, False, SelfLower, SelfType,
}

impl Kw {
    fn from_ident(s: &str) -> Option<Kw> {
        use Kw::*;
        Some(match s {
            "fn" => Fn, "type" => Type, "trait" => Trait, "impl" => Impl,
            "mut" => Mut, "const" => Const, "use" => Use, "pub" => Pub,
            "extern" => Extern, "unsafe" => Unsafe, "macro" => Macro,
            "comptime" => Comptime, "match" => Match, "if" => If, "elif" => Elif,
            "else" => Else, "while" => While, "for" => For, "in" => In,
            "break" => Break, "continue" => Continue, "return" => Return,
            "spawn" => Spawn, "and" => And, "or" => Or, "not" => Not, "as" => As,
            "true" => True, "false" => False, "self" => SelfLower, "Self" => SelfType,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

/// Kann nach diesem Token ein Newline eine Anweisung beenden? (Go-artige Regel.)
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
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Lexer { src: src.as_bytes(), pos: 0, diags: Vec::new() }
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
            let tok = self.next_token();
            out.push(Token { tok, span: Span(start, self.pos) });
        }
        (out, self.diags)
    }

    /// Whitespace/Kommentare überspringen; signifikante Newlines als Token an
    /// `out` anhängen (nur wenn das letzte Token eine Anweisung beenden kann).
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
        match Kw::from_ident(&s) {
            Some(kw) => Tok::Kw(kw),
            None => Tok::Ident(s),
        }
    }

    fn number(&mut self) -> Tok {
        let start = self.pos;
        // Basis-Präfix
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
                    self.diags.push(Diag::error("ungültige Zahl", Span(start, self.pos)));
                    Tok::Int(0)
                }
            };
        }
        // Dezimal, evtl. Float
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
        // i8/i16/i32/i64/u8/.../f32/f64 – für M1 nur konsumieren.
        if matches!(self.peek(), b'i' | b'u' | b'f') {
            self.pos += 1;
            while self.peek().is_ascii_digit() {
                self.pos += 1;
            }
        }
    }

    fn string(&mut self) -> Tok {
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
            self.diags.push(Diag::error("nicht geschlossener String", Span(start, self.pos)));
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
            self.diags.push(Diag::error("nicht geschlossenes Char-Literal", Span(start, self.pos)));
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
                self.diags.push(Diag::error("unerwartetes '!' (nutze `not`)", Span(self.pos - 1, self.pos)));
                Tok::Ne
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
                    &format!("unerwartetes Zeichen '{}'", other as char),
                    Span(self.pos - 1, self.pos),
                ));
                // als Space behandeln: nächster Aufruf macht weiter
                Tok::Newline
            }
        }
    }
}

/// Bequemer Einstieg.
pub fn lex(src: &str) -> (Vec<Token>, Vec<Diag>) {
    Lexer::new(src).lex()
}
