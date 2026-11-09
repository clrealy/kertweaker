//! Parsing, editing and diffing Linux kernel `.config` files.
//!
//! Every meaningful line in a `.config` is one of:
//!
//! ```text
//! CONFIG_FOO=y
//! CONFIG_FOO="a string"
//! CONFIG_FOO=0x1000
//! # CONFIG_FOO is not set
//! ```
//!
//! Original lines are preserved, so round-tripping an untouched file is
//! byte-identical and diffs stay readable.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

/// A config value. `None` means the symbol is explicitly "not set".
pub type Value = Option<String>;

#[derive(Debug, Clone, Default)]
pub struct Config {
    lines: Vec<String>,
    values: HashMap<String, Value>,
    index: HashMap<String, usize>,
}

/// Format a symbol/value pair the way kbuild writes it.
pub fn format_line(symbol: &str, value: &Value) -> String {
    match value {
        None => format!("# {symbol} is not set"),
        Some(v) => format!("{symbol}={v}"),
    }
}

/// Split a `.config` line into `(symbol, value)` if it declares one.
fn parse_line(line: &str) -> Option<(&str, Value)> {
    let t = line.trim_end();
    if let Some(rest) = t.strip_prefix("# ") {
        // "# CONFIG_X is not set"
        let sym = rest.strip_suffix(" is not set")?;
        if is_symbol(sym) {
            return Some((sym, None));
        }
        return None;
    }
    if t.starts_with('#') || t.is_empty() {
        return None;
    }
    let (sym, val) = t.split_once('=')?;
    if is_symbol(sym) {
        Some((sym, Some(val.to_string())))
    } else {
        None
    }
}

fn is_symbol(s: &str) -> bool {
    s.starts_with("CONFIG_")
        && s.len() > 7
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl Config {
    pub fn parse(text: &str) -> Config {
        let mut cfg = Config::default();
        for raw in text.lines() {
            cfg.lines.push(raw.to_string());
            if let Some((sym, val)) = parse_line(raw) {
                cfg.values.insert(sym.to_string(), val);
                cfg.index.insert(sym.to_string(), cfg.lines.len() - 1);
            }
        }
        cfg
    }

    /// Read a `.config`, transparently decompressing `/proc/config.gz`.
    pub fn load(path: &Path) -> io::Result<Config> {
        let text = if path.extension().map(|e| e == "gz").unwrap_or(false) {
            read_gzip(path)?
        } else {
            fs::read_to_string(path)?
        };
        Ok(Config::parse(&text))
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
    pub fn has(&self, symbol: &str) -> bool {
        self.values.contains_key(symbol)
    }
    /// `Some(None)` = explicitly unset, `None` = absent entirely.
    pub fn raw(&self, symbol: &str) -> Option<&Value> {
        self.values.get(symbol)
    }
    pub fn get(&self, symbol: &str) -> Option<&str> {
        self.values.get(symbol).and_then(|v| v.as_deref())
    }
    /// True for `=y` or `=m`.
    pub fn enabled(&self, symbol: &str) -> bool {
        matches!(self.get(symbol), Some("y") | Some("m"))
    }
    /// Value with surrounding quotes removed.
    pub fn string(&self, symbol: &str) -> Option<String> {
        self.get(symbol).map(|v| {
            let b = v.as_bytes();
            if b.len() >= 2 && b[0] == b'"' && b[b.len() - 1] == b'"' {
                v[1..v.len() - 1].to_string()
            } else {
                v.to_string()
            }
        })
    }
    pub fn symbols(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.values.iter()
    }
    pub fn enabled_count(&self) -> usize {
        self.values
            .values()
            .filter(|v| matches!(v.as_deref(), Some("y") | Some("m")))
            .count()
    }
    pub fn module_count(&self) -> usize {
        self.values
            .values()
            .filter(|v| v.as_deref() == Some("m"))
            .count()
    }

    /// Set a symbol. Returns true when the file actually changed.
    pub fn set(&mut self, symbol: &str, value: Value) -> bool {
        if self.values.get(symbol) == Some(&value) {
            return false;
        }
        let line = format_line(symbol, &value);
        match self.index.get(symbol) {
            Some(&i) => self.lines[i] = line,
            None => {
                self.lines.push(line);
                self.index.insert(symbol.to_string(), self.lines.len() - 1);
            }
        }
        self.values.insert(symbol.to_string(), value);
        true
    }

    /// Apply a fragment, returning the symbols that changed.
    pub fn apply(&mut self, fragment: &[(String, Value)]) -> Vec<String> {
        let mut changed = Vec::new();
        for (sym, val) in fragment {
            if self.set(sym, val.clone()) {
                changed.push(sym.clone());
            }
        }
        changed
    }

    pub fn render(&self) -> String {
        let mut out = String::with_capacity(self.lines.iter().map(|l| l.len() + 1).sum());
        for line in &self.lines {
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        fs::write(path, self.render())
    }
}

/// Parse a free-form fragment (what a user pastes into the "extra options" box).
pub fn parse_fragment(text: &str) -> Vec<(String, Value)> {
    let mut out: Vec<(String, Value)> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let parsed = parse_line(line).or_else(|| {
            // Tolerate "CONFIG_X n" and "CONFIG_X=n" as "unset".
            let (sym, val) = line
                .split_once('=')
                .or_else(|| line.split_once(char::is_whitespace))?;
            let sym = sym.trim();
            if is_symbol(sym) && val.trim() == "n" {
                Some((sym, None))
            } else {
                None
            }
        });
        let Some((sym, val)) = parsed else { continue };
        // kbuild never writes "=n", so in a hand-written fragment it can only
        // mean "turn this off".
        let val = if val.as_deref() == Some("n") { None } else { val };
        if let Some(slot) = out.iter_mut().find(|(s, _)| s == sym) {
            slot.1 = val;
        } else {
            out.push((sym.to_string(), val));
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Enabled,
    Disabled,
    Changed,
}

impl ChangeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChangeKind::Added => "added",
            ChangeKind::Enabled => "enabled",
            ChangeKind::Disabled => "disabled",
            ChangeKind::Changed => "changed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Change {
    pub symbol: String,
    pub before: Value,
    pub after: Value,
    pub present_before: bool,
}

impl Change {
    pub fn kind(&self) -> ChangeKind {
        if !self.present_before {
            ChangeKind::Added
        } else if self.after.is_none() {
            ChangeKind::Disabled
        } else if self.before.is_none() {
            ChangeKind::Enabled
        } else {
            ChangeKind::Changed
        }
    }
    pub fn before_line(&self) -> Option<String> {
        self.present_before
            .then(|| format_line(&self.symbol, &self.before))
    }
    pub fn after_line(&self) -> String {
        format_line(&self.symbol, &self.after)
    }
}

/// Symbol-level diff, sorted by symbol name.
pub fn diff(old: &Config, new: &Config) -> Vec<Change> {
    let mut symbols: Vec<&String> = old.values.keys().chain(new.values.keys()).collect();
    symbols.sort();
    symbols.dedup();

    let mut out = Vec::new();
    for sym in symbols {
        let before = old.values.get(sym);
        let after = new.values.get(sym);
        match (before, after) {
            (Some(b), None) => {
                // Symbol vanished from the new config: report as disabled.
                if b.is_some() {
                    out.push(Change {
                        symbol: sym.clone(),
                        before: b.clone(),
                        after: None,
                        present_before: true,
                    });
                }
            }
            (b, Some(a)) => {
                let present_before = b.is_some();
                let before = b.cloned().flatten();
                if present_before && &before == a {
                    continue;
                }
                // A brand-new "is not set" line is noise, not a change.
                if !present_before && a.is_none() {
                    continue;
                }
                out.push(Change {
                    symbol: sym.clone(),
                    before,
                    after: a.clone(),
                    present_before,
                });
            }
            (None, None) => unreachable!(),
        }
    }
    out
}

/// Which requested symbols did *not* survive `make olddefconfig`.
///
/// This is how kertweaker reports an impossible request: rather than
/// reimplementing Kconfig dependency rules, it asks kbuild and then checks.
#[derive(Debug, Clone)]
pub struct Unapplied {
    pub symbol: String,
    pub wanted: Value,
    pub actual: Value,
    pub present: bool,
}

pub fn verify(wanted: &[(String, Value)], result: &Config) -> Vec<Unapplied> {
    let mut out = Vec::new();
    for (sym, want) in wanted {
        match result.raw(sym) {
            Some(actual) if actual == want => {}
            Some(actual) => out.push(Unapplied {
                symbol: sym.clone(),
                wanted: want.clone(),
                actual: actual.clone(),
                present: true,
            }),
            None => {
                // An absent symbol satisfies a request to turn it off.
                if want.is_some() {
                    out.push(Unapplied {
                        symbol: sym.clone(),
                        wanted: want.clone(),
                        actual: None,
                        present: false,
                    });
                }
            }
        }
    }
    out
}

/// Decompress with the system gzip, which every Linux box has. Avoids pulling
/// in an inflate implementation just to read `/proc/config.gz`.
fn read_gzip(path: &Path) -> io::Result<String> {
    let out = Command::new("gzip").arg("-dc").arg(path).output()?;
    if !out.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "gzip -dc {} failed: {}",
                path.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# Automatically generated\nCONFIG_A=y\n# CONFIG_B is not set\nCONFIG_S=\"hi\"\nCONFIG_N=0x1000\n\n# a comment\n";

    #[test]
    fn roundtrip_is_byte_identical() {
        assert_eq!(Config::parse(SAMPLE).render(), SAMPLE);
    }

    #[test]
    fn reads_values() {
        let c = Config::parse(SAMPLE);
        assert_eq!(c.get("CONFIG_A"), Some("y"));
        assert!(c.enabled("CONFIG_A"));
        assert!(!c.enabled("CONFIG_B"));
        assert!(c.has("CONFIG_B"));
        assert_eq!(c.string("CONFIG_S").unwrap(), "hi");
        assert_eq!(c.get("CONFIG_N"), Some("0x1000"));
        assert_eq!(c.len(), 4);
        assert!(!c.has("CONFIG_MISSING"));
    }

    #[test]
    fn ignores_non_symbol_lines() {
        let c = Config::parse(
            "notconfig=y\nCONFIG_=y\n# CONFIG_X is not set!\n# just prose\nCONFIG BAD=y\nCONFIG_HY-PHEN=y\n",
        );
        assert_eq!(c.len(), 0, "parsed: {:?}", c.symbols().collect::<Vec<_>>());
        // Lowercase is unusual but legal in Kconfig, so it must still parse.
        assert_eq!(Config::parse("CONFIG_lower=y\n").len(), 1);
    }

    #[test]
    fn set_edits_in_place_and_appends() {
        let mut c = Config::parse(SAMPLE);
        assert!(c.set("CONFIG_B", Some("m".into())));
        assert!(!c.set("CONFIG_B", Some("m".into())), "no-op must report false");
        assert!(c.set("CONFIG_NEW", Some("y".into())));
        let text = c.render();
        assert!(text.contains("CONFIG_B=m"));
        assert!(!text.contains("# CONFIG_B is not set"));
        assert!(text.ends_with("CONFIG_NEW=y\n"));
        assert_eq!(text.lines().count(), SAMPLE.lines().count() + 1);
    }

    #[test]
    fn diff_classifies_changes() {
        let old = Config::parse(SAMPLE);
        let mut new = old.clone();
        new.apply(&[
            ("CONFIG_A".into(), None),
            ("CONFIG_B".into(), Some("m".into())),
            ("CONFIG_S".into(), Some("\"bye\"".into())),
            ("CONFIG_NEW".into(), Some("y".into())),
        ]);
        let changes = diff(&old, &new);
        let kinds: Vec<_> = changes
            .iter()
            .map(|c| (c.symbol.as_str(), c.kind()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("CONFIG_A", ChangeKind::Disabled),
                ("CONFIG_B", ChangeKind::Enabled),
                ("CONFIG_NEW", ChangeKind::Added),
                ("CONFIG_S", ChangeKind::Changed),
            ]
        );
        assert_eq!(changes[0].after_line(), "# CONFIG_A is not set");
        assert_eq!(changes[2].before_line(), None);
    }

    #[test]
    fn diff_ignores_new_unset_noise() {
        let old = Config::parse("CONFIG_A=y\n");
        let mut new = old.clone();
        new.set("CONFIG_ZZZ", None);
        assert!(diff(&old, &new).is_empty());
    }

    #[test]
    fn fragment_parsing_tolerates_shorthand() {
        let f = parse_fragment(
            "CONFIG_A=y\n# CONFIG_B is not set\nCONFIG_C n\nCONFIG_D=n\ngarbage\n# note\nCONFIG_A=m\n",
        );
        assert_eq!(
            f,
            vec![
                ("CONFIG_A".to_string(), Some("m".to_string())),
                ("CONFIG_B".to_string(), None),
                ("CONFIG_C".to_string(), None),
                ("CONFIG_D".to_string(), None),
            ]
        );
    }

    #[test]
    fn verify_reports_what_kbuild_dropped() {
        let wanted = vec![
            ("CONFIG_OK".to_string(), Some("y".to_string())),
            ("CONFIG_FORCED".to_string(), Some("y".to_string())),
            ("CONFIG_GONE".to_string(), Some("m".to_string())),
            ("CONFIG_OFF".to_string(), None),
        ];
        let result = Config::parse("CONFIG_OK=y\n# CONFIG_FORCED is not set\n");
        let bad = verify(&wanted, &result);
        assert_eq!(bad.len(), 2);
        assert_eq!(bad[0].symbol, "CONFIG_FORCED");
        assert!(bad[0].present);
        assert_eq!(bad[1].symbol, "CONFIG_GONE");
        assert!(!bad[1].present);
    }
}
