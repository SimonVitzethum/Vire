//! Nutzer-konfigurierbare Oberflächen-Syntax: die Schlüsselwort-*Schreibweisen*
//! sind austauschbar, ohne den Compiler zu ändern. Eine `vire.syntax`-Datei neben
//! der Quelle (oder `--syntax DATEI`) bildet kanonische Schlüsselwörter auf eigene
//! Schreibweisen ab — z.B. `fn = func`, `capsule = box`. Die Grammatik/Semantik
//! bleibt unberührt: nur der Lexer schlägt Bezeichner gegen diese Tabelle nach.
//!
//! Format (eine Zuordnung pro Zeile, `#` = Kommentar):
//! ```text
//! # kanonisch = neue_schreibweise
//! fn      = func
//! capsule = box
//! ```

use std::collections::HashMap;

use crate::lexer::{Kw, KW_TABLE};

/// Schreibweise → Schlüsselwort. Default = die kanonische Tabelle (Identität).
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
    /// Nachschlagen: ist dieser Bezeichner (unter aktueller Syntax) ein Schlüsselwort?
    pub fn keyword(&self, s: &str) -> Option<Kw> {
        self.map.get(s).copied()
    }

    /// Ein Schlüsselwort umbenennen: die alte Schreibweise entfällt, die neue gilt.
    /// Fehler, wenn die neue Schreibweise schon von einem ANDEREN Schlüsselwort
    /// belegt ist (sonst würde die Grammatik mehrdeutig).
    pub fn rename(&mut self, kw: Kw, spelling: &str) -> Result<(), String> {
        if let Some(other) = self.map.get(spelling) {
            if *other != kw {
                return Err(format!(
                    "Schreibweise `{spelling}` ist schon `{}` — kann nicht auch `{}` sein",
                    other.canonical(),
                    kw.canonical()
                ));
            }
        }
        // alte Schreibweise(n) dieses Schlüsselworts entfernen
        self.map.retain(|_, v| *v != kw);
        self.map.insert(spelling.to_string(), kw);
        Ok(())
    }

    /// Eine `vire.syntax`-Konfiguration parsen und anwenden (auf Default-Basis).
    pub fn parse(text: &str) -> Result<Syntax, Vec<String>> {
        let mut syn = Syntax::default();
        let mut errs = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((lhs, rhs)) = line.split_once('=') else {
                errs.push(format!("Zeile {}: erwarte `kanonisch = schreibweise`", i + 1));
                continue;
            };
            let (canon, spelling) = (lhs.trim(), rhs.trim());
            let Some(kw) = KW_TABLE.iter().find(|(sp, _)| *sp == canon).map(|(_, k)| *k) else {
                errs.push(format!("Zeile {}: unbekanntes Schlüsselwort `{canon}`", i + 1));
                continue;
            };
            if !is_ident(spelling) {
                errs.push(format!("Zeile {}: `{spelling}` ist kein gültiger Bezeichner", i + 1));
                continue;
            }
            if let Err(e) = syn.rename(kw, spelling) {
                errs.push(format!("Zeile {}: {e}", i + 1));
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
