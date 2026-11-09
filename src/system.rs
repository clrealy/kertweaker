//! Host inspection: what machine are we building for, and can we build at all.

use crate::json::Json;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct ToolSpec {
    pub tool: &'static str,
    pub why: &'static str,
    pub deb: Option<&'static str>,
    pub rpm: Option<&'static str>,
    pub arch: Option<&'static str>,
    /// A normal build fails without it.
    pub required: bool,
    /// Set when the tool is only needed for one output format (`deb`, `rpm`).
    pub output: Option<&'static str>,
}

impl ToolSpec {
    /// The package that ships this tool on the given distro family.
    pub fn package(&self, family: &str) -> Option<&'static str> {
        match family {
            "deb" => self.deb,
            "rpm" => self.rpm,
            "arch" => self.arch,
            _ => None,
        }
    }
}

type S = &'static str;

const fn spec(
    tool: S,
    why: S,
    deb: S,
    rpm: S,
    arch: S,
    required: bool,
    output: Option<S>,
) -> ToolSpec {
    ToolSpec {
        tool,
        why,
        deb: Some(deb),
        rpm: Some(rpm),
        arch: Some(arch),
        required,
        output,
    }
}
/// Every kernel build needs it.
const fn required(tool: S, why: S, deb: S, rpm: S, arch: S) -> ToolSpec {
    spec(tool, why, deb, rpm, arch, true, None)
}
/// Only some selections need it.
const fn optional(tool: S, why: S, deb: S, rpm: S, arch: S) -> ToolSpec {
    spec(tool, why, deb, rpm, arch, false, None)
}
/// Only one output format needs it.
const fn only_for(output: S, tool: S, why: S, deb: S, rpm: S, arch: S) -> ToolSpec {
    spec(tool, why, deb, rpm, arch, false, Some(output))
}

/// What kbuild needs on the PATH, and the package that provides each tool on
/// Debian-, Red Hat- and Arch-family distributions.
#[rustfmt::skip]
pub const BUILD_TOOLS: &[ToolSpec] = &[
    required("gcc", "compiles the kernel", "gcc", "gcc", "gcc"),
    required("make", "drives kbuild", "make", "make", "make"),
    required("ld", "links the kernel image", "binutils", "binutils", "binutils"),
    required("flex", "builds the kconfig parser", "flex", "flex", "flex"),
    required("bison", "builds the kconfig parser", "bison", "bison", "bison"),
    required("bc", "computes timeconst.h", "bc", "bc", "bc"),
    required("openssl", "module signing and certificates", "libssl-dev", "openssl-devel", "openssl"),
    required("tar", "unpacks kernel tarballs", "tar", "tar", "tar"),
    required("xz", "decompresses kernel tarballs", "xz-utils", "xz", "xz"),
    required("gzip", "reads /proc/config.gz", "gzip", "gzip", "gzip"),
    required("depmod", "installs modules", "kmod", "kmod", "kmod"),
    optional("curl", "downloads kernel tarballs", "curl", "curl", "curl"),
    optional("pahole", "only for CONFIG_DEBUG_INFO_BTF", "dwarves", "dwarves", "pahole"),
    optional("clang", "only for LTO builds", "clang", "clang", "clang"),
    optional("ld.lld", "linker for clang LTO builds", "lld", "lld", "lld"),
    optional("zstd", "zstd-compressed images and modules", "zstd", "zstd", "zstd"),
    optional("lz4", "lz4-compressed kernel images", "lz4", "lz4", "lz4"),
    optional("cpio", "initramfs generation", "cpio", "cpio", "cpio"),
    // `make bindeb-pkg` runs dpkg-buildpackage, which reads the generated
    // debian/control and refuses to start without debhelper and rsync.
    only_for("deb", "dpkg-buildpackage", "builds .deb packages", "dpkg-dev", "dpkg-dev", "dpkg"),
    only_for("deb", "dh", "debhelper, used by the kernel's debian/rules", "debhelper", "debhelper", "debhelper"),
    only_for("deb", "rsync", "copies headers into the .deb", "rsync", "rsync", "rsync"),
    only_for("rpm", "rpmbuild", "builds .rpm packages", "rpm", "rpm-build", "rpm-tools"),
];

/// Headers with no binary to probe for, so they are always in the install line.
const EXTRA_PACKAGES: &[(&str, &[&str])] = &[
    ("deb", &["libelf-dev", "libncurses-dev"]),
    ("rpm", &["elfutils-libelf-devel", "ncurses-devel"]),
    ("arch", &["libelf", "ncurses"]),
];

pub fn which(tool: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(tool);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn run(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Parse `/etc/os-release` into key/value pairs.
pub fn os_release() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in read("/etc/os-release").lines() {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
        }
    }
    map
}

/// `deb`, `rpm`, `arch` or `unknown`.
pub fn package_family() -> &'static str {
    if which("dpkg").is_some() {
        return "deb";
    }
    if which("rpm").is_some() {
        return "rpm";
    }
    if which("pacman").is_some() {
        return "arch";
    }
    let os = os_release();
    let ids = format!(
        "{} {}",
        os.get("ID").cloned().unwrap_or_default(),
        os.get("ID_LIKE").cloned().unwrap_or_default()
    )
    .to_lowercase();
    for (family, keys) in [
        ("deb", ["debian", "ubuntu", "mint"]),
        ("rpm", ["fedora", "rhel", "suse"]),
        ("arch", ["arch", "manjaro", "endeavour"]),
    ] {
        if keys.iter().any(|k| ids.contains(k)) {
            return family;
        }
    }
    "unknown"
}

pub struct Cpu {
    pub model: String,
    pub vendor: String,
    pub threads: usize,
    pub hypervisor: bool,
}

pub fn cpu() -> Cpu {
    let text = read("/proc/cpuinfo");
    let mut model = String::new();
    let mut vendor = String::new();
    let mut flags = String::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "model name" if model.is_empty() => model = v.to_string(),
            "Model" if model.is_empty() => model = v.to_string(),
            "vendor_id" if vendor.is_empty() => vendor = v.to_string(),
            "flags" if flags.is_empty() => flags = v.to_string(),
            _ => {}
        }
    }
    Cpu {
        model: if model.is_empty() {
            "unknown".into()
        } else {
            model
        },
        vendor,
        threads: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
        hypervisor: flags.split_whitespace().any(|f| f == "hypervisor"),
    }
}

/// Which KVM option suits this CPU, for pre-selecting the right choice.
pub fn kvm_flavour() -> &'static str {
    match cpu().vendor.as_str() {
        "GenuineIntel" => "intel",
        "AuthenticAMD" => "amd",
        _ => "both",
    }
}

pub fn memory_gib() -> f64 {
    for line in read("/proc/meminfo").lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            if let Some(kb) = rest.split_whitespace().next() {
                if let Ok(kb) = kb.parse::<f64>() {
                    return (kb / 1024.0 / 1024.0 * 10.0).round() / 10.0;
                }
            }
        }
    }
    0.0
}

/// Free space in GiB, via `df -k` (statvfs would need libc).
pub fn free_disk_gib(path: &Path) -> f64 {
    let out = run("df", &["-Pk", &path.to_string_lossy()]);
    out.lines()
        .nth(1)
        .and_then(|l| l.split_whitespace().nth(3))
        .and_then(|kb| kb.parse::<f64>().ok())
        .map(|kb| (kb / 1024.0 / 1024.0 * 10.0).round() / 10.0)
        .unwrap_or(0.0)
}

pub fn uname(flag: &str) -> String {
    run("uname", &[flag])
}

/// kbuild's ARCH name for this machine.
pub fn kernel_arch() -> String {
    let m = uname("-m");
    match m.as_str() {
        "x86_64" | "amd64" => "x86_64",
        "i686" | "i386" => "i386",
        "aarch64" => "arm64",
        "armv7l" | "armv6l" => "arm",
        "riscv64" => "riscv",
        "ppc64le" => "powerpc",
        "s390x" => "s390",
        "loongarch64" => "loongarch",
        other => other,
    }
    .to_string()
}

/// Where the running kernel's config lives, if it is available at all.
pub fn running_config_path() -> Option<PathBuf> {
    let release = uname("-r");
    let candidates = [
        "/proc/config.gz".to_string(),
        format!("/boot/config-{release}"),
        format!("/lib/modules/{release}/build/.config"),
        format!("/usr/lib/modules/{release}/.config"),
    ];
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists() && fs::File::open(p).is_ok())
}

pub fn bootloader() -> &'static str {
    if Path::new("/boot/grub").is_dir() || Path::new("/boot/grub2").is_dir() {
        "grub"
    } else if Path::new("/boot/loader/entries").is_dir() || which("bootctl").is_some() {
        "systemd-boot"
    } else {
        "unknown"
    }
}

/// `Some(true/false)` when EFI tells us, `None` when we cannot know.
///
/// This matters: a self-built kernel is unsigned, so Secure Boot will refuse
/// to boot it unless it is signed with an enrolled key.
pub fn secure_boot() -> Option<bool> {
    let dir = fs::read_dir("/sys/firmware/efi/efivars").ok()?;
    for entry in dir.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("SecureBoot-") {
            let data = fs::read(entry.path()).ok()?;
            return data.last().map(|b| *b != 0);
        }
    }
    None
}

pub fn loaded_modules() -> Vec<String> {
    let mut mods: Vec<String> = read("/proc/modules")
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect();
    mods.sort();
    mods
}

/// Is there a graphical session on this machine? Used only to pick a default profile.
fn has_gui() -> bool {
    Path::new("/sys/class/drm").is_dir()
        && fs::read_dir("/sys/class/drm")
            .map(|d| {
                d.flatten()
                    .any(|e| e.file_name().to_string_lossy().starts_with("card"))
            })
            .unwrap_or(false)
}

pub struct ToolStatus {
    pub spec: &'static ToolSpec,
    pub found: bool,
    pub path: Option<PathBuf>,
}

pub fn tool_report() -> Vec<ToolStatus> {
    BUILD_TOOLS
        .iter()
        .map(|spec| {
            let path = which(spec.tool);
            ToolStatus {
                found: path.is_some(),
                path,
                spec,
            }
        })
        .collect()
}

pub struct Packages {
    pub family: &'static str,
    pub missing_required: Vec<&'static str>,
    pub missing_optional: Vec<&'static str>,
    pub install_command: Option<String>,
}

pub fn missing_packages(report: &[ToolStatus]) -> Packages {
    let family = package_family();
    let mut missing_required = Vec::new();
    let mut missing_optional = Vec::new();
    let mut pkgs: Vec<&str> = Vec::new();

    for item in report.iter().filter(|i| !i.found) {
        if item.spec.required {
            missing_required.push(item.spec.tool);
            if let Some(p) = item.spec.package(family) {
                pkgs.push(p);
            }
        } else if item.spec.output.is_none() {
            // Output-format tools are reported against the format that needs them.
            missing_optional.push(item.spec.tool);
        }
    }

    let extras = EXTRA_PACKAGES
        .iter()
        .find(|(f, _)| *f == family)
        .map(|(_, p)| *p)
        .unwrap_or(&[]);
    pkgs.sort();
    pkgs.dedup();
    pkgs.extend_from_slice(extras);

    let install_command = install_prefix(family).map(|p| format!("{p} {}", pkgs.join(" ")));

    Packages {
        family,
        missing_required,
        missing_optional,
        install_command,
    }
}

/// The command that installs packages on this distro family.
pub fn install_prefix(family: &str) -> Option<&'static str> {
    match family {
        "deb" => Some("sudo apt install -y"),
        "rpm" => Some("sudo dnf install -y"),
        "arch" => Some("sudo pacman -S --needed"),
        _ => None,
    }
}

/// Tools a given output format needs that are not installed, and how to get them.
pub struct OutputTools {
    pub missing: Vec<&'static str>,
    pub install_command: Option<String>,
}

pub fn output_tools(output: &str, report: &[ToolStatus]) -> OutputTools {
    let family = package_family();
    let lacking: Vec<&ToolStatus> = report
        .iter()
        .filter(|t| t.spec.output == Some(output) && !t.found)
        .collect();
    let mut pkgs: Vec<&str> = lacking
        .iter()
        .filter_map(|t| t.spec.package(family))
        .collect();
    pkgs.sort();
    pkgs.dedup();
    OutputTools {
        missing: lacking.iter().map(|t| t.spec.tool).collect(),
        install_command: match (install_prefix(family), pkgs.is_empty()) {
            (Some(prefix), false) => Some(format!("{prefix} {}", pkgs.join(" "))),
            _ => None,
        },
    }
}

pub struct Warning {
    pub level: &'static str,
    pub text: String,
}

fn warnings(report: &[ToolStatus], workdir: &Path) -> Vec<Warning> {
    let mut out = Vec::new();
    if secure_boot() == Some(true) {
        out.push(Warning {
            level: "warn",
            text: "Secure Boot is enabled. A kernel you build here is unsigned, so the \
                   firmware will refuse it unless you sign it with an enrolled MOK key or \
                   turn Secure Boot off."
                .into(),
        });
    }
    let missing: Vec<&str> = report
        .iter()
        .filter(|i| i.spec.required && !i.found)
        .map(|i| i.spec.tool)
        .collect();
    if !missing.is_empty() {
        out.push(Warning {
            level: "error",
            text: format!(
                "Missing build tools: {}. Install them before starting a build.",
                missing.join(", ")
            ),
        });
    }
    let free = free_disk_gib(workdir);
    if free > 0.0 && free < 25.0 {
        out.push(Warning {
            level: "warn",
            text: format!(
                "Only {free} GiB free where builds are staged. A full build with debug \
                 info can use 20-30 GiB; turn debug info off or free some space."
            ),
        });
    }
    let mem = memory_gib();
    if mem > 0.0 && mem < 4.0 {
        out.push(Warning {
            level: "warn",
            text: "Under 4 GiB of RAM. Use fewer parallel jobs, or the link step may be \
                   OOM-killed."
                .into(),
        });
    }
    out
}

/// Everything the GUI's "Your machine" panel shows.
pub fn describe(workdir: &Path) -> Json {
    let c = cpu();
    let report = tool_report();
    let pkgs = missing_packages(&report);
    let running = running_config_path();
    let sb = secure_boot();
    let os = os_release();
    let mem = memory_gib();
    let gui = has_gui();

    Json::obj(vec![
        ("hostname", Json::s(uname("-n"))),
        (
            "distro",
            Json::s(
                os.get("PRETTY_NAME")
                    .or_else(|| os.get("NAME"))
                    .cloned()
                    .unwrap_or_else(|| "unknown".into()),
            ),
        ),
        (
            "distro_id",
            Json::s(os.get("ID").cloned().unwrap_or_default()),
        ),
        ("package_family", Json::s(pkgs.family)),
        ("running_kernel", Json::s(uname("-r"))),
        ("machine", Json::s(uname("-m"))),
        ("arch", Json::s(kernel_arch())),
        (
            "cpu",
            Json::obj(vec![
                ("model", Json::s(c.model)),
                ("vendor", Json::s(c.vendor)),
                ("threads", Json::u(c.threads)),
                ("hypervisor", Json::b(c.hypervisor)),
            ]),
        ),
        ("kvm_flavour", Json::s(kvm_flavour())),
        ("memory_gib", Json::n(mem)),
        ("free_disk_gib", Json::n(free_disk_gib(workdir))),
        ("workdir", Json::s(workdir.to_string_lossy())),
        ("gcc_version", Json::s(run("gcc", &["-dumpfullversion"]))),
        (
            "running_config",
            Json::opt_s(running.as_ref().map(|p| p.to_string_lossy().into_owned())),
        ),
        ("loaded_module_count", Json::u(loaded_modules().len())),
        ("bootloader", Json::s(bootloader())),
        (
            "secure_boot",
            match sb {
                Some(v) => Json::b(v),
                None => Json::Null,
            },
        ),
        ("is_root", Json::b(is_root())),
        ("has_gui", Json::b(gui)),
        (
            "suggested_profile",
            Json::s(crate::catalog::profile_hint(
                c.hypervisor,
                mem,
                c.threads,
                gui,
            )),
        ),
        ("suggested_jobs", Json::u(suggested_jobs())),
        (
            "tools",
            Json::arr(
                report
                    .iter()
                    .map(|t| {
                        Json::obj(vec![
                            ("tool", Json::s(t.spec.tool)),
                            ("why", Json::s(t.spec.why)),
                            ("required", Json::b(t.spec.required)),
                            ("output", Json::opt_s(t.spec.output)),
                            ("package", Json::opt_s(t.spec.package(pkgs.family))),
                            ("found", Json::b(t.found)),
                            (
                                "path",
                                Json::opt_s(
                                    t.path.as_ref().map(|p| p.to_string_lossy().into_owned()),
                                ),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "packages",
            Json::obj(vec![
                ("family", Json::s(pkgs.family)),
                (
                    "missing_required",
                    Json::strs(pkgs.missing_required.clone()),
                ),
                (
                    "missing_optional",
                    Json::strs(pkgs.missing_optional.clone()),
                ),
                ("install_command", Json::opt_s(pkgs.install_command.clone())),
                ("install_prefix", Json::opt_s(install_prefix(pkgs.family))),
            ]),
        ),
        (
            "warnings",
            Json::arr(
                warnings(&report, workdir)
                    .into_iter()
                    .map(|w| {
                        Json::obj(vec![("level", Json::s(w.level)), ("text", Json::s(w.text))])
                    })
                    .collect(),
            ),
        ),
    ])
}

pub fn is_root() -> bool {
    run("id", &["-u"]) == "0"
}

/// Parallel jobs to suggest: one per thread, but capped by RAM, since each
/// compiler process wants roughly 1 GiB when debug info is on.
pub fn suggested_jobs() -> usize {
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let mem = memory_gib();
    if mem <= 0.0 {
        return threads;
    }
    let by_mem = (mem as usize).max(1);
    threads.min(by_mem).max(1)
}

/// Where kertweaker keeps sources, configs, logs and artifacts.
pub fn default_workdir() -> PathBuf {
    if let Some(dir) = std::env::var_os("KERTWEAKER_HOME") {
        return PathBuf::from(dir);
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("kertweaker")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_finds_a_real_binary_and_not_a_fake_one() {
        assert!(which("sh").is_some());
        assert!(which("definitely-not-a-real-binary-xyz").is_none());
    }

    #[test]
    fn arch_maps_to_a_kbuild_name() {
        let a = kernel_arch();
        assert!(!a.is_empty());
        assert!(!a.contains(' '));
    }

    #[test]
    fn describe_produces_the_fields_the_ui_reads() {
        let j = describe(Path::new("/tmp"));
        for key in [
            "hostname",
            "distro",
            "running_kernel",
            "arch",
            "cpu",
            "memory_gib",
            "free_disk_gib",
            "tools",
            "packages",
            "warnings",
            "suggested_jobs",
            "suggested_profile",
        ] {
            assert!(j.get(key).is_some(), "describe() is missing {key}");
        }
        assert!(j.get("suggested_jobs").unwrap().as_usize().unwrap() >= 1);
        let tools = j.get("tools").unwrap().as_array().unwrap();
        assert_eq!(tools.len(), BUILD_TOOLS.len());
        // Round-trips through the JSON writer, so the UI can actually read it.
        assert!(crate::json::parse(&j.to_string()).is_ok());
    }

    #[test]
    fn missing_packages_builds_an_install_line() {
        let report = tool_report();
        let p = missing_packages(&report);
        if let Some(cmd) = p.install_command {
            assert!(cmd.starts_with("sudo "));
            // Header packages are always included.
            assert!(cmd.contains("elf"), "{cmd}");
        }
    }

    #[test]
    fn output_formats_declare_their_own_tools() {
        let deb: Vec<&str> = BUILD_TOOLS
            .iter()
            .filter(|t| t.output == Some("deb"))
            .map(|t| t.tool)
            .collect();
        // What `make bindeb-pkg` actually refuses to run without.
        assert_eq!(deb, vec!["dpkg-buildpackage", "dh", "rsync"]);
        assert!(BUILD_TOOLS.iter().any(|t| t.output == Some("rpm")));
        // Format-specific tools never block an ordinary build.
        assert!(BUILD_TOOLS
            .iter()
            .filter(|t| t.output.is_some())
            .all(|t| !t.required));

        // A host with none of them is told which packages to install.
        let report: Vec<ToolStatus> = BUILD_TOOLS
            .iter()
            .map(|spec| ToolStatus {
                spec,
                found: false,
                path: None,
            })
            .collect();
        let need = output_tools("deb", &report);
        assert_eq!(need.missing, vec!["dpkg-buildpackage", "dh", "rsync"]);
        if let Some(cmd) = need.install_command {
            assert!(cmd.contains("debhelper") && cmd.contains("rsync"), "{cmd}");
        }
        assert!(output_tools("image", &report).missing.is_empty());
    }

    #[test]
    fn cpu_and_memory_are_plausible() {
        assert!(cpu().threads >= 1);
        assert!(memory_gib() >= 0.0);
        assert!(suggested_jobs() >= 1);
    }

    #[test]
    fn workdir_is_absolute() {
        assert!(default_workdir().is_absolute());
    }
}
