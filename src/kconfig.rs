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

    /// Apply a fragment the way the kernel's own `merge_config.sh` does: drop
    /// each symbol's old line and append the new value at the end of the file.
    ///
    /// Position matters. For a Kconfig `choice`, the *last* member set to `y`
    /// wins, so editing a line in place can silently lose to a different member
    /// that happens to sit further down (x86_64_defconfig's
    /// `CPU_FREQ_DEFAULT_GOV_USERSPACE=y` is a real example). Appending makes
    /// the request win, exactly as it would with the upstream tool.
    ///
    /// Returns the symbols whose effective state changed.
    pub fn merge(&mut self, fragment: &[(String, Value)]) -> Vec<String> {
        let differing: Vec<&(String, Value)> = fragment
            .iter()
            .filter(|(sym, val)| self.values.get(sym) != Some(val))
            .collect();
        if differing.is_empty() {
            return Vec::new();
        }
        // Turning off a symbol the base never mentioned is not a real change.
        let changed: Vec<String> = differing
            .iter()
            .filter(|(sym, val)| val.is_some() || self.get(sym).is_some())
            .map(|(sym, _)| sym.clone())
            .collect();

        let mut lines: Vec<String> = Vec::with_capacity(self.lines.len() + differing.len() + 3);
        for line in self.lines.drain(..) {
            let replaced = parse_line(&line)
                .map(|(sym, _)| differing.iter().any(|(s, _)| s == sym))
                .unwrap_or(false);
            if !replaced {
                lines.push(line);
            }
        }
        lines.push("#".to_string());
        lines.push("# Set by kertweaker".to_string());
        lines.push("#".to_string());
        for (sym, val) in differing {
            lines.push(format_line(sym, val));
        }
        *self = Config::parse(&lines.join("\n"));
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
    check(wanted, result, &[]).unapplied
}

/// The outcome of comparing what was asked for with what kbuild produced.
#[derive(Debug, Clone, Default)]
pub struct Verification {
    /// Requests that did not take effect.
    pub unapplied: Vec<Unapplied>,
    /// Asked for as a module (`=m`), built into the image (`=y`) instead.
    /// The feature is there, so this is information rather than a failure.
    pub builtin_instead: Vec<String>,
}

/// Compare a requested fragment against the config kbuild settled on.
///
/// `aliases` lists symbols that were renamed between kernel versions. A catalog
/// option requests every spelling; if one of them took effect, the others
/// being absent is expected and not worth a warning.
pub fn check(wanted: &[(String, Value)], result: &Config, aliases: &[&[&str]]) -> Verification {
    let satisfied = |sym: &str, want: &Value| -> bool {
        match result.raw(sym) {
            Some(actual) => actual == want,
            // An absent symbol satisfies a request to turn it off.
            None => want.is_none(),
        }
    };
    let mut out = Verification::default();
    for (sym, want) in wanted {
        if satisfied(sym, want) {
            continue;
        }
        // m -> y happens when the config has no loadable-module support, or
        // when something built in selects the symbol. Either way it is present.
        if want.as_deref() == Some("m") && result.get(sym) == Some("y") {
            out.builtin_instead.push(sym.clone());
            continue;
        }
        let covered_by_alias = aliases
            .iter()
            .filter(|group| group.contains(&sym.as_str()))
            .flat_map(|group| group.iter())
            .filter(|other| **other != sym.as_str())
            .any(|other| {
                wanted
                    .iter()
                    .any(|(s, v)| s == other && v == want && satisfied(s, v))
            });
        if covered_by_alias {
            continue;
        }
        let actual = result.raw(sym);
        out.unapplied.push(Unapplied {
            symbol: sym.clone(),
            wanted: want.clone(),
            actual: actual.cloned().flatten(),
            present: actual.is_some(),
        });
    }
    // Sorted, so the lists are stable however the fragment was ordered.
    out.unapplied.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    out.builtin_instead.sort();
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
    fn merge_appends_so_a_choice_request_wins() {
        // USERSPACE=y sits *below* PERFORMANCE in the base, as in x86_64_defconfig.
        let mut c = Config::parse(
            "# CONFIG_GOV_PERFORMANCE is not set\nCONFIG_OTHER=y\nCONFIG_GOV_USERSPACE=y\n",
        );
        let changed = c.merge(&[
            ("CONFIG_GOV_PERFORMANCE".into(), Some("y".into())),
            ("CONFIG_OTHER".into(), Some("y".into())), // already set: untouched
            ("CONFIG_NEVER_SEEN".into(), None),        // off and absent: no real change
        ]);
        assert_eq!(changed, vec!["CONFIG_GOV_PERFORMANCE"]);
        let text = c.render();
        let perf = text.find("CONFIG_GOV_PERFORMANCE=y").unwrap();
        let user = text.find("CONFIG_GOV_USERSPACE=y").unwrap();
        assert!(perf > user, "the request must come last so kconfig prefers it:\n{text}");
        assert!(!text.contains("# CONFIG_GOV_PERFORMANCE is not set"));
        assert_eq!(text.matches("CONFIG_OTHER=y").count(), 1);
        // Index is rebuilt, so later edits still work.
        assert_eq!(c.get("CONFIG_GOV_PERFORMANCE"), Some("y"));
        assert!(c.set("CONFIG_GOV_PERFORMANCE", None));
        assert!(c.render().contains("# CONFIG_GOV_PERFORMANCE is not set"));
    }

    #[test]
    fn merge_with_nothing_to_do_leaves_the_file_alone() {
        let mut c = Config::parse(SAMPLE);
        assert!(c.merge(&[("CONFIG_A".into(), Some("y".into()))]).is_empty());
        assert_eq!(c.render(), SAMPLE);
    }

    #[test]
    fn renamed_symbols_do_not_raise_false_alarms() {
        let aliases: &[&[&str]] = &[&["CONFIG_NEW_NAME", "CONFIG_OLD_NAME"]];
        let wanted = vec![
            ("CONFIG_NEW_NAME".to_string(), Some("y".to_string())),
            ("CONFIG_OLD_NAME".to_string(), Some("y".to_string())),
        ];
        // This kernel only knows the new spelling, and it took effect.
        let newer = Config::parse("CONFIG_NEW_NAME=y\n");
        assert!(check(&wanted, &newer, aliases).unapplied.is_empty());
        // An older kernel only knows the old spelling.
        let older = Config::parse("CONFIG_OLD_NAME=y\n");
        assert!(check(&wanted, &older, aliases).unapplied.is_empty());
        // Neither took effect: that is a real problem and both are reported.
        let neither = Config::parse("# CONFIG_NEW_NAME is not set\n");
        assert_eq!(check(&wanted, &neither, aliases).unapplied.len(), 2);
        // Without alias knowledge the absent spelling is flagged.
        assert_eq!(verify(&wanted, &newer).len(), 1);
    }

    #[test]
    fn a_module_built_in_instead_is_information_not_failure() {
        let wanted = vec![
            ("CONFIG_BTRFS_FS".to_string(), Some("m".to_string())),
            ("CONFIG_XFS_FS".to_string(), Some("m".to_string())),
            ("CONFIG_KPROBES".to_string(), Some("y".to_string())),
            ("CONFIG_WANT_BUILTIN".to_string(), Some("y".to_string())),
        ];
        // A monolithic config: no module support, so m became y; kprobes needs
        // modules and is gone; and y -> m is a genuine downgrade.
        let result = Config::parse(
            "# CONFIG_MODULES is not set\nCONFIG_BTRFS_FS=y\nCONFIG_XFS_FS=y\nCONFIG_WANT_BUILTIN=m\n",
        );
        let v = check(&wanted, &result, &[]);
        assert_eq!(v.builtin_instead, vec!["CONFIG_BTRFS_FS", "CONFIG_XFS_FS"]);
        let failed: Vec<&str> = v.unapplied.iter().map(|u| u.symbol.as_str()).collect();
        assert_eq!(failed, vec!["CONFIG_KPROBES", "CONFIG_WANT_BUILTIN"]);
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
