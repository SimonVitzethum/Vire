//! Diagnosen mit Quell-Span (Byte-Range). Fehler nah an der Ursache melden ist
//! Ergonomie-kritisch (BEWERTUNG §5).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span(pub usize, pub usize);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Level {
    Error,
    Warning,
}

#[derive(Debug, Clone)]
pub struct Diag {
    pub level: Level,
    pub msg: String,
    pub span: Span,
    /// Optionaler Fix-Vorschlag (maschinenlesbar gedacht).
    pub hint: Option<String>,
}

impl Diag {
    pub fn error(msg: &str, span: Span) -> Diag {
        Diag { level: Level::Error, msg: msg.to_string(), span, hint: None }
    }
    pub fn with_hint(mut self, hint: &str) -> Diag {
        self.hint = Some(hint.to_string());
        self
    }
    /// Menschliche Ausgabe mit Zeile:Spalte, aus dem Quelltext berechnet.
    pub fn render(&self, src: &str) -> String {
        let (line, col) = line_col(src, self.span.0);
        let lvl = match self.level {
            Level::Error => "Fehler",
            Level::Warning => "Warnung",
        };
        let mut s = format!("{lvl} {line}:{col}: {}", self.msg);
        if let Some(h) = &self.hint {
            s.push_str(&format!("\n  Hinweis: {h}"));
        }
        s
    }
}

pub fn line_col(src: &str, byte: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for (i, c) in src.char_indices() {
        if i >= byte {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}
