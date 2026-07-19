//! Platform conditional compilation: the `@when(os)` declaration attribute.
//!
//! `@when(linux)` / `@when(macos)` / `@when(windows)` / `@when(unix)` on a `fn` or
//! `type` includes that declaration only when the compile target matches; otherwise
//! the item is dropped before inference. This is the surface syntax over the same
//! compile-time selection that `comptime if` does for expressions — so two functions
//! of the same name can be provided per platform and exactly one survives (no
//! duplicate-definition error). `@when(a, b)` keeps the item on *either* platform.
//!
//! The target OS is the host by default (Vire compiles for the host, `-march=native`)
//! or is derived from the `--target <triple>` when cross-compiling.

use crate::ast::{Attr, Item, Module};

/// The set of recognised platform names (an unknown name in `@when` is an error).
const KNOWN: &[&str] = &["linux", "macos", "windows", "unix"];

/// Resolve the target OS: from a `--target` triple if given, else the host OS.
/// Returns one of "linux" / "macos" / "windows" (the canonical names).
pub fn target_os(target: Option<&str>) -> &'static str {
    if let Some(t) = target {
        let t = t.to_ascii_lowercase();
        if t.contains("windows") || t.contains("mingw") {
            return "windows";
        }
        if t.contains("darwin") || t.contains("apple") || t.contains("macos") {
            return "macos";
        }
        if t.contains("linux") {
            return "linux";
        }
        // Unknown triple → fall through to the host.
    }
    match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux", // linux + the other unixes map to the linux/unix family
    }
}

/// Does a single `@when` argument match the resolved target OS?
/// `unix` matches the unix family (linux + macos), the rest match by name.
fn arg_matches(arg: &str, os: &str) -> bool {
    match arg {
        "unix" => os == "linux" || os == "macos",
        other => other == os,
    }
}

/// Collect the `@when` args of a declaration; returns `None` if it has no `@when`
/// (→ unconditional), else `Some(args)`. Validation of unknown names is done by the
/// caller so it can attach a diagnostic.
fn when_args(attrs: &[Attr]) -> Option<Vec<String>> {
    let mut args: Vec<String> = Vec::new();
    let mut saw = false;
    for a in attrs {
        if a.name == "when" {
            saw = true;
            args.extend(a.args.iter().cloned());
        }
    }
    if saw {
        Some(args)
    } else {
        None
    }
}

/// Apply `@when` platform gating to a module in place: drop `fn`/`type` items whose
/// `@when` does not match `os`, and strip the (now-consumed) `@when` attributes from
/// the items that stay (so later passes such as `@derive` never see them). Returns a
/// list of diagnostics for unknown platform names.
pub fn apply(m: &mut Module, os: &str) -> Vec<String> {
    let mut errs = Vec::new();
    m.items.retain(|it| {
        let attrs: &[Attr] = match it {
            Item::Fn(f) => &f.attrs,
            Item::Type(t) => &t.attrs,
            _ => return true,
        };
        match when_args(attrs) {
            None => true,
            Some(args) => {
                for a in &args {
                    if !KNOWN.contains(&a.as_str()) {
                        errs.push(format!(
                            "@when: unknown platform `{a}` (known: {})",
                            KNOWN.join(", ")
                        ));
                    }
                }
                args.iter().any(|a| arg_matches(a, os))
            }
        }
    });
    // Strip the consumed `@when` attributes from the survivors.
    for it in &mut m.items {
        match it {
            Item::Fn(f) => f.attrs.retain(|a| a.name != "when"),
            Item::Type(t) => t.attrs.retain(|a| a.name != "when"),
            _ => {}
        }
    }
    errs
}
