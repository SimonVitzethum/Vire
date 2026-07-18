//! User-configurable surface syntax: the keyword *spellings* are
//! interchangeable without changing the compiler. A `vire.syntax` file next to
//! the source (or `--syntax FILE`) maps canonical keywords to custom
//! spellings — e.g. `fn = func`, `capsule = box`. The grammar/semantics
//! remain untouched: only the lexer looks up identifiers against this table.
//!
//! Format (one mapping per line, `#` = comment):
//! ```text
//! # canonical = new_spelling
//! fn      = func
//! capsule = box
//! ```

use std::collections::HashMap;

use crate::lexer::{Kw, KW_TABLE};

/// Spelling → keyword. Default = the canonical table (identity).
#[derive(Debug, Clone)]
pub struct Syntax {
    map: HashMap<String, Kw>,
}

impl Default for Syntax {
    fn default() -> Self {
        Syntax { map: KW_TABLE.iter().map(|(sp, k)| (sp.to_string(), *k)).collect() }
    }
}

impl Syntax {
    /// Lookup: is this identifier (under the current syntax) a keyword?
    pub fn keyword(&self, s: &str) -> Option<Kw> {
        self.map.get(s).copied()
    }

    /// Rename a keyword: the old spelling is dropped, the new one takes effect.
    /// Errors if the new spelling is already taken by a DIFFERENT keyword
    /// (otherwise the grammar would become ambiguous).
    pub fn rename(&mut self, kw: Kw, spelling: &str) -> Result<(), String> {
        if let Some(other) = self.map.get(spelling) {
            if *other != kw {
                return Err(format!(
                    "spelling `{spelling}` is already `{}` — cannot also be `{}`",
                    other.canonical(),
                    kw.canonical()
                ));
            }
        }
        // remove the old spelling(s) of this keyword
        self.map.retain(|_, v| *v != kw);
        self.map.insert(spelling.to_string(), kw);
        Ok(())
    }

    /// Parse and apply a `vire.syntax` configuration (on top of the default).
    pub fn parse(text: &str) -> Result<Syntax, Vec<String>> {
        let mut syn = Syntax::default();
        let mut errs = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((lhs, rhs)) = line.split_once('=') else {
                errs.push(format!("line {}: expected `canonical = spelling`", i + 1));
                continue;
            };
            let (canon, spelling) = (lhs.trim(), rhs.trim());
            let Some(kw) = KW_TABLE.iter().find(|(sp, _)| *sp == canon).map(|(_, k)| *k) else {
                errs.push(format!("line {}: unknown keyword `{canon}`", i + 1));
                continue;
            };
            if !is_ident(spelling) {
                errs.push(format!("line {}: `{spelling}` is not a valid identifier", i + 1));
                continue;
            }
            if let Err(e) = syn.rename(kw, spelling) {
                errs.push(format!("line {}: {e}", i + 1));
            }
        }
        if errs.is_empty() {
            Ok(syn)
        } else {
            Err(errs)
        }
    }
}

fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c == '_' || c.is_alphabetic())
        && s.chars().all(|c| c == '_' || c.is_alphanumeric())
}
