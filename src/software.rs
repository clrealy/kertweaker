//! Software that rides along with a kernel.
//!
//! Applications are not part of a kernel and cannot be compiled into one. What
//! a kernel *can* do is carry the features an application depends on - Steam
//! needs 32-bit syscall support and gamepad drivers, Flatpak needs user
//! namespaces and seccomp, Docker needs a pile of cgroup and netfilter options.
//!
//! So picking an app here does two things:
//!
//! 1. Its kernel requirements are layered onto the config and verified like any
//!    tweak, so a kernel built for Steam can actually run Steam.
//! 2. The app is recorded in the `.kernel` file, and after the build kertweaker
//!    prints the command that installs it. Nothing is installed automatically.

use crate::catalog::{self, Requirement, Resolved, M, Y};
use crate::json::Json;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Sets = &'static [(&'static str, Option<&'static str>)];

pub struct App {
    pub id: &'static str,
    pub title: &'static str,
    pub desc: &'static str,
    /// Kernel options the app needs. Empty when any kernel will do.
    pub kernel: Sets,
    pub deb: &'static [&'static str],
    pub rpm: &'static [&'static str],
    pub arch: &'static [&'static str],
    /// Flathub application id, used when there is no native package.
    pub flatpak: Option<&'static str>,
    /// For software that only ships as an installer script: its https URL.
    ///
    /// kertweaker never fetches or runs these. The plan it prints downloads
    /// the script to a file first, so it can be read before it is run.
    pub script: Option<&'static str>,
    pub note: &'static str,
}

impl App {
    pub fn packages(&self, family: &str) -> &'static [&'static str] {
        match family {
            "deb" => self.deb,
            "rpm" => self.rpm,
            "arch" => self.arch,
            _ => &[],
        }
    }
}

pub const APPS: &[App] = &[
    App {
        id: "flatpak",
        title: "Flatpak",
        desc: "Sandboxed desktop apps from Flathub. Needs user namespaces, seccomp and FUSE.",
        kernel: &[
            ("CONFIG_NAMESPACES", Y),
            ("CONFIG_USER_NS", Y),
            ("CONFIG_PID_NS", Y),
            ("CONFIG_NET_NS", Y),
            ("CONFIG_IPC_NS", Y),
            ("CONFIG_UTS_NS", Y),
            ("CONFIG_SECCOMP", Y),
            ("CONFIG_SECCOMP_FILTER", Y),
            ("CONFIG_CGROUPS", Y),
            ("CONFIG_FUSE_FS", M),
        ],
        deb: &["flatpak"],
        rpm: &["flatpak"],
        arch: &["flatpak"],
        flatpak: None,
        script: None,
        note: "",
    },
    App {
        id: "steam",
        title: "Steam",
        desc: "Needs 32-bit program support, the sandbox Steam's runtime uses, and gamepad drivers (Xbox, PlayStation, Switch, Steam Controller/Deck).",
        kernel: &[
            ("CONFIG_IA32_EMULATION", Y),
            ("CONFIG_NAMESPACES", Y),
            ("CONFIG_USER_NS", Y),
            ("CONFIG_PID_NS", Y),
            ("CONFIG_NET_NS", Y),
            ("CONFIG_IPC_NS", Y),
            ("CONFIG_UTS_NS", Y),
            ("CONFIG_SECCOMP", Y),
            ("CONFIG_SECCOMP_FILTER", Y),
            ("CONFIG_FUTEX", Y),
            ("CONFIG_USB_SUPPORT", Y),
            ("CONFIG_USB", Y),
            ("CONFIG_HID", Y),
            ("CONFIG_HIDRAW", Y),
            ("CONFIG_HID_GENERIC", Y),
            ("CONFIG_USB_HID", Y),
            ("CONFIG_INPUT_EVDEV", Y),
            ("CONFIG_INPUT_MISC", Y),
            ("CONFIG_INPUT_UINPUT", M),
            ("CONFIG_INPUT_JOYSTICK", Y),
            ("CONFIG_INPUT_JOYDEV", M),
            ("CONFIG_JOYSTICK_XPAD", M),
            ("CONFIG_NEW_LEDS", Y),
            ("CONFIG_LEDS_CLASS", Y),
            ("CONFIG_LEDS_CLASS_MULTICOLOR", M),
            ("CONFIG_HID_STEAM", M),
            ("CONFIG_HID_PLAYSTATION", M),
            ("CONFIG_HID_NINTENDO", M),
        ],
        deb: &["steam-installer"],
        rpm: &["steam"],
        arch: &["steam"],
        flatpak: Some("com.valvesoftware.Steam"),
        script: None,
        note: "The native package lives in an extra repository: multiverse on Ubuntu, non-free on Debian, RPM Fusion on Fedora, multilib on Arch.",
    },
    App {
        id: "wine",
        title: "Wine",
        desc: "Runs Windows programs. Needs 32-bit program support for older software.",
        kernel: &[("CONFIG_IA32_EMULATION", Y), ("CONFIG_FUTEX", Y)],
        deb: &["wine"],
        rpm: &["wine"],
        arch: &["wine"],
        flatpak: None,
        script: None,
        note: "",
    },
    App {
        id: "docker",
        title: "Docker",
        desc: "Container engine. Needs namespaces, cgroups, overlayfs, virtual networking and the netfilter modules it programs.",
        kernel: &[
            ("CONFIG_NAMESPACES", Y),
            ("CONFIG_USER_NS", Y),
            ("CONFIG_PID_NS", Y),
            ("CONFIG_NET_NS", Y),
            ("CONFIG_IPC_NS", Y),
            ("CONFIG_UTS_NS", Y),
            ("CONFIG_SECCOMP", Y),
            ("CONFIG_SECCOMP_FILTER", Y),
            ("CONFIG_CGROUPS", Y),
            ("CONFIG_MEMCG", Y),
            ("CONFIG_CGROUP_PIDS", Y),
            ("CONFIG_CGROUP_SCHED", Y),
            ("CONFIG_CGROUP_CPUACCT", Y),
            ("CONFIG_CGROUP_DEVICE", Y),
            ("CONFIG_CGROUP_FREEZER", Y),
            ("CONFIG_CPUSETS", Y),
            ("CONFIG_BLK_CGROUP", Y),
            ("CONFIG_KEYS", Y),
            ("CONFIG_POSIX_MQUEUE", Y),
            ("CONFIG_OVERLAY_FS", M),
            ("CONFIG_VETH", M),
            ("CONFIG_BRIDGE", M),
            ("CONFIG_NETFILTER", Y),
            ("CONFIG_NETFILTER_ADVANCED", Y),
            ("CONFIG_BRIDGE_NETFILTER", M),
            ("CONFIG_NF_CONNTRACK", M),
            ("CONFIG_NF_NAT", M),
            ("CONFIG_NF_TABLES", M),
            ("CONFIG_IP_NF_IPTABLES", M),
            ("CONFIG_IP_NF_FILTER", M),
            ("CONFIG_IP_NF_NAT", M),
            ("CONFIG_IP_NF_TARGET_MASQUERADE", M),
            ("CONFIG_NETFILTER_XT_MATCH_ADDRTYPE", M),
            ("CONFIG_NETFILTER_XT_MATCH_CONNTRACK", M),
        ],
        deb: &["docker.io"],
        rpm: &["moby-engine"],
        arch: &["docker"],
        flatpak: None,
        script: None,
        note: "",
    },
    App {
        id: "podman",
        title: "Podman",
        desc: "Daemonless, rootless containers. Needs user namespaces, cgroups, overlayfs and FUSE.",
        kernel: &[
            ("CONFIG_NAMESPACES", Y),
            ("CONFIG_USER_NS", Y),
            ("CONFIG_PID_NS", Y),
            ("CONFIG_NET_NS", Y),
            ("CONFIG_IPC_NS", Y),
            ("CONFIG_UTS_NS", Y),
            ("CONFIG_SECCOMP", Y),
            ("CONFIG_SECCOMP_FILTER", Y),
            ("CONFIG_CGROUPS", Y),
            ("CONFIG_MEMCG", Y),
            ("CONFIG_CGROUP_PIDS", Y),
            ("CONFIG_OVERLAY_FS", M),
            ("CONFIG_FUSE_FS", M),
            ("CONFIG_VETH", M),
            ("CONFIG_BRIDGE", M),
            ("CONFIG_NETFILTER", Y),
            ("CONFIG_NF_TABLES", M),
            ("CONFIG_NF_NAT", M),
        ],
        deb: &["podman"],
        rpm: &["podman"],
        arch: &["podman"],
        flatpak: None,
        script: None,
        note: "",
    },
    App {
        id: "virt-manager",
        title: "Virtual machines (QEMU/KVM + virt-manager)",
        desc: "Run other operating systems at near-native speed. Needs KVM and virtual networking.",
        kernel: &[
            ("CONFIG_VIRTUALIZATION", Y),
            ("CONFIG_KVM", M),
            ("CONFIG_KVM_INTEL", M),
            ("CONFIG_KVM_AMD", M),
            ("CONFIG_TUN", M),
            ("CONFIG_VHOST_MENU", Y),
            ("CONFIG_VHOST_NET", M),
            ("CONFIG_BRIDGE", M),
            ("CONFIG_MACVLAN", M),
            ("CONFIG_MACVTAP", M),
        ],
        deb: &["qemu-system-x86", "libvirt-daemon-system", "virt-manager"],
        rpm: &["qemu-kvm", "libvirt", "virt-manager"],
        arch: &["qemu-full", "libvirt", "virt-manager"],
        flatpak: None,
        script: None,
        note: "",
    },
    App {
        id: "waydroid",
        title: "Waydroid (Android apps)",
        desc: "Runs Android in a container. Needs the Android binder driver, which most distro kernels leave out.",
        kernel: &[
            ("CONFIG_NAMESPACES", Y),
            ("CONFIG_USER_NS", Y),
            ("CONFIG_PID_NS", Y),
            ("CONFIG_NET_NS", Y),
            ("CONFIG_IPC_NS", Y),
            ("CONFIG_UTS_NS", Y),
            ("CONFIG_ANDROID_BINDER_IPC", Y),
            ("CONFIG_ANDROID_BINDERFS", Y),
            ("CONFIG_PSI", Y),
            ("CONFIG_VETH", M),
            ("CONFIG_BRIDGE", M),
        ],
        deb: &["waydroid"],
        rpm: &["waydroid"],
        arch: &[],
        flatpak: None,
        script: None,
        note: "Waydroid ships from its own repository on Debian and Ubuntu, and from the AUR on Arch.",
    },
    App {
        id: "obs",
        title: "OBS Studio",
        desc: "Recording and streaming. Needs webcam and capture-device (V4L2) support.",
        kernel: &[
            ("CONFIG_USB_SUPPORT", Y),
            ("CONFIG_USB", Y),
            ("CONFIG_MEDIA_SUPPORT", M),
            ("CONFIG_MEDIA_CAMERA_SUPPORT", Y),
            ("CONFIG_MEDIA_USB_SUPPORT", Y),
            ("CONFIG_USB_VIDEO_CLASS", M),
        ],
        deb: &["obs-studio"],
        rpm: &["obs-studio"],
        arch: &["obs-studio"],
        flatpak: Some("com.obsproject.Studio"),
        script: None,
        note: "",
    },
    App {
        id: "wireguard-tools",
        title: "WireGuard tools",
        desc: "The wg and wg-quick commands, plus the in-kernel WireGuard driver.",
        kernel: &[("CONFIG_WIREGUARD", M), ("CONFIG_NET_UDP_TUNNEL", M)],
        deb: &["wireguard-tools"],
        rpm: &["wireguard-tools"],
        arch: &["wireguard-tools"],
        flatpak: None,
        script: None,
        note: "",
    },
    App {
        id: "gimp",
        title: "GIMP",
        desc: "Image editor. Runs on any kernel, so this only adds it to your install list.",
        kernel: &[],
        deb: &["gimp"],
        rpm: &["gimp"],
        arch: &["gimp"],
        flatpak: Some("org.gimp.GIMP"),
        script: None,
        note: "",
    },
    App {
        id: "vlc",
        title: "VLC",
        desc: "Media player. Runs on any kernel, so this only adds it to your install list.",
        kernel: &[],
        deb: &["vlc"],
        rpm: &["vlc"],
        arch: &["vlc"],
        flatpak: Some("org.videolan.VLC"),
        script: None,
        note: "",
    },
    App {
        id: "discord",
        title: "Discord",
        desc: "Chat. Not in distribution repositories, so it is installed as a Flatpak.",
        kernel: &[],
        deb: &[],
        rpm: &[],
        arch: &["discord"],
        flatpak: Some("com.discordapp.Discord"),
        script: None,
        note: "",
    },
    App {
        id: "gurt",
        title: "gurt",
        desc: "The gurt package manager. No distribution packages it, so it installs with its own script from gurtproject.org.",
        kernel: &[],
        deb: &[],
        rpm: &[],
        arch: &[],
        flatpak: None,
        script: Some("https://gurtproject.org/install.sh"),
        note: "",
    },
];

pub fn app(id: &str) -> Option<&'static App> {
    APPS.iter().find(|a| a.id == id)
}

/// The origin tag used in diffs and conflicts for an app's kernel options.
pub fn origin(id: &str) -> String {
    format!("app:{id}")
}

/// Kernel requirements of the selected apps, ready for [`catalog::resolve_full`].
pub fn requirements(ids: &[String]) -> Vec<Requirement> {
    ids.iter()
        .filter_map(|id| app(id))
        .filter(|a| !a.kernel.is_empty())
        .map(|a| Requirement {
            origin: origin(a.id),
            sets: a.kernel.to_vec(),
        })
        .collect()
}

/// Resolve tweaks, software requirements and extra options together.
pub fn resolve(
    selections: &BTreeMap<String, String>,
    software: &[String],
    extra: &str,
) -> Resolved {
    let mut resolved = catalog::resolve_full(selections, &requirements(software), extra);
    for id in software {
        if app(id).is_none() {
            resolved.unknown.push(origin(id));
        }
    }
    resolved.unknown.sort();
    resolved.unknown.dedup();
    resolved
}

/// A package name safe to place on a command line: what apt, dnf and pacman
/// themselves accept, and nothing a shell would interpret.
pub fn valid_package(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'_' | b'-' | b':'))
}

/// Split a free-form list ("htop, gimp  vim") into valid names and rejects.
pub fn parse_packages(text: &str) -> (Vec<String>, Vec<String>) {
    let mut good: Vec<String> = Vec::new();
    let mut bad = Vec::new();
    for raw in text.split(|c: char| c.is_whitespace() || c == ',') {
        let name = raw.trim();
        if name.is_empty() {
            continue;
        }
        if valid_package(name) {
            if !good.iter().any(|g| g == name) {
                good.push(name.to_string());
            }
        } else {
            bad.push(name.to_string());
        }
    }
    (good, bad)
}

/// How long a package list or a per-package answer is trusted before asking
/// the package manager again.
const INDEX_TTL: Duration = Duration::from_secs(600);

/// Every package name the distribution knows, loaded with one command.
///
/// Asking per package is far too slow to do on every preview: `apt-cache`
/// can spend over a second rebuilding its cache on each call, so a dozen names
/// meant a dozen seconds. One listing, kept in memory, makes each lookup free.
struct Index {
    family: String,
    names: Option<Arc<HashSet<String>>>,
    loaded: Instant,
}

static INDEX: Mutex<Option<Index>> = Mutex::new(None);
/// A remembered answer and when it was given.
type Remembered = (Option<bool>, Instant);
static PER_PACKAGE: Mutex<Option<HashMap<String, Remembered>>> = Mutex::new(None);

fn run_stdout(cmd: &str, args: &[&str]) -> Option<(bool, String)> {
    let out = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    Some((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

/// Turn a package manager's one-name-per-line listing into a set.
fn parse_names(listing: &str) -> HashSet<String> {
    listing
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether `package` is in a listing. A Debian `name:arch` qualifier is
/// ignored, since listings carry bare names.
fn listed(names: &HashSet<String>, package: &str) -> bool {
    let bare = package.split(':').next().unwrap_or(package);
    names.contains(bare)
}

/// The full package list for a family, or `None` if it cannot be listed.
fn package_index(family: &str) -> Option<Arc<HashSet<String>>> {
    let mut slot = INDEX.lock().ok()?;
    if let Some(index) = slot.as_ref() {
        if index.family == family && index.loaded.elapsed() < INDEX_TTL {
            return index.names.clone();
        }
    }
    let listing = match family {
        // Real packages only: virtual names and missing dependencies are
        // left out unless --all-names is given.
        "deb" => run_stdout("apt-cache", &["pkgnames"]),
        "arch" => run_stdout("pacman", &["-Slq"]),
        _ => None,
    };
    let names = listing
        .filter(|(ok, _)| *ok)
        .map(|(_, text)| parse_names(&text))
        // An empty list means the package cache was never downloaded, which
        // says nothing about whether any particular package exists.
        .filter(|set| !set.is_empty())
        .map(Arc::new);
    *slot = Some(Index {
        family: family.to_string(),
        names: names.clone(),
        loaded: Instant::now(),
    });
    names
}

/// Load the package list ahead of time, so the first preview is not the one
/// that pays for it.
pub fn warm_cache() {
    let _ = package_index(crate::system::package_family());
}

/// Whether the distribution's package manager knows a package.
///
/// `None` means "could not tell" (no supported package manager, or its cache
/// is empty); callers then assume the package exists rather than hide it.
pub fn available(family: &str, package: &str) -> Option<bool> {
    if !valid_package(package) {
        return Some(false);
    }
    match family {
        "deb" | "arch" => package_index(family).map(|names| listed(&names, package)),
        "rpm" => {
            // dnf has no cheap full listing, so ask per package and remember.
            if let Ok(memo) = PER_PACKAGE.lock() {
                if let Some((answer, at)) = memo.as_ref().and_then(|m| m.get(package)) {
                    if at.elapsed() < INDEX_TTL {
                        return *answer;
                    }
                }
            }
            // -C: cache only, so this never blocks on the network.
            let answer = match run_stdout("dnf", &["-q", "-C", "repoquery", package]) {
                Some((true, text)) => Some(!text.trim().is_empty()),
                _ => None,
            };
            if let Ok(mut memo) = PER_PACKAGE.lock() {
                memo.get_or_insert_with(HashMap::new)
                    .insert(package.to_string(), (answer, Instant::now()));
            }
            answer
        }
        _ => None,
    }
}

/// What to run to install the selected software, and what could not be found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// Commands to run, in order.
    pub commands: Vec<String>,
    /// Things worth knowing: fallbacks taken, extra repositories needed.
    pub notes: Vec<String>,
    /// Names left out because no package by that name exists.
    pub not_found: Vec<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty() && self.notes.is_empty() && self.not_found.is_empty()
    }

    pub fn to_json(&self) -> Json {
        Json::obj(vec![
            ("commands", Json::strs(self.commands.clone())),
            ("notes", Json::strs(self.notes.clone())),
            ("not_found", Json::strs(self.not_found.clone())),
        ])
    }
}

/// Work out the install commands.
///
/// Native packages are preferred. An app whose native package is missing from
/// the enabled repositories falls back to its Flatpak; a name nothing knows is
/// reported and left out, so one bad name cannot fail the whole command.
pub fn plan(
    software: &[String],
    packages: &[String],
    family: &str,
    check: &dyn Fn(&str) -> Option<bool>,
) -> Plan {
    let mut out = Plan::default();
    let mut native: Vec<String> = Vec::new();
    let mut flatpaks: Vec<&str> = Vec::new();
    let mut scripts: Vec<&App> = Vec::new();
    let push = |list: &mut Vec<String>, name: &str| {
        if !list.iter().any(|n| n == name) {
            list.push(name.to_string());
        }
    };

    // A package name the distribution does not have, but which is an app
    // kertweaker knows how to install some other way, is treated as that app:
    // typing "gurt" or "discord" should work, not be reported as missing.
    let mut ids: Vec<&str> = software.iter().map(String::as_str).collect();
    let mut plain: Vec<&String> = Vec::new();
    for name in packages {
        if app(name).is_some() && check(name) != Some(true) {
            if !ids.contains(&name.as_str()) {
                ids.push(name);
            }
        } else {
            plain.push(name);
        }
    }

    for id in ids {
        let Some(a) = app(id) else { continue };
        let pkgs = a.packages(family);
        let usable = !pkgs.is_empty() && pkgs.iter().all(|p| check(p) != Some(false));
        if usable {
            for p in pkgs {
                push(&mut native, p);
            }
        } else if let Some(fp) = a.flatpak {
            flatpaks.push(fp);
            if !pkgs.is_empty() {
                out.notes.push(format!(
                    "{} is not in your enabled repositories, so it is installed as a Flatpak instead. {}",
                    a.title, a.note
                ));
            }
        } else if a.script.is_some() {
            if !scripts.iter().any(|s| s.id == a.id) {
                scripts.push(a);
            }
        } else {
            out.not_found.push(a.title.to_string());
            out.notes.push(if a.note.is_empty() {
                format!(
                    "{} has no package kertweaker knows for this distribution.",
                    a.title
                )
            } else {
                format!("{} was left out of the command. {}", a.title, a.note)
            });
        }
    }

    for name in plain {
        if !valid_package(name) {
            continue;
        }
        if check(name) == Some(false) {
            out.not_found.push(name.clone());
            out.notes.push(format!(
                "'{name}' is not a package in your distribution's repositories, so it was left out."
            ));
        } else {
            push(&mut native, name);
        }
    }

    if !flatpaks.is_empty() && check("flatpak") != Some(false) {
        push(&mut native, "flatpak");
    }
    if !native.is_empty() {
        match crate::system::install_prefix(family) {
            Some(prefix) => out.commands.push(format!("{prefix} {}", native.join(" "))),
            None => out.notes.push(format!(
                "Install these with your package manager: {}",
                native.join(" ")
            )),
        }
    }
    if !flatpaks.is_empty() {
        flatpaks.dedup();
        out.commands.push(
            "sudo flatpak remote-add --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo"
                .to_string(),
        );
        out.commands
            .push(format!("flatpak install -y flathub {}", flatpaks.join(" ")));
    }
    // Installer scripts come last and in two steps - fetch, then run - so the
    // script sits on disk to be read first. Never `curl | sh`: that runs
    // whatever the server sends, unseen.
    for a in scripts {
        let Some(url) = a.script else { continue };
        let file = script_file(a);
        out.commands.push(format!("curl -fsSL {url} -o {file}"));
        out.commands.push(format!("sh {file}"));
        out.notes.push(format!(
            "{} installs with a script downloaded from {}. kertweaker has not seen that script \
             and cannot vouch for it: read {file} before you run it.",
            a.title,
            script_host(url)
        ));
    }
    out
}

/// The local file name an app's installer script is saved under.
pub fn script_file(a: &App) -> String {
    format!("{}-install.sh", a.id)
}

/// The host part of a script URL, for telling the user where it comes from.
pub fn script_host(url: &str) -> &str {
    url.trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap_or(url)
}

pub fn to_json(family: &str) -> Json {
    Json::arr(
        APPS.iter()
            .map(|a| {
                Json::obj(vec![
                    ("id", Json::s(a.id)),
                    ("title", Json::s(a.title)),
                    ("description", Json::s(a.desc)),
                    ("note", Json::s(a.note)),
                    ("packages", Json::strs(a.packages(family).to_vec())),
                    ("flatpak", Json::opt_s(a.flatpak)),
                    ("script", Json::opt_s(a.script)),
                    ("script_host", Json::opt_s(a.script.map(script_host))),
                    (
                        "kernel",
                        Json::Obj(
                            a.kernel
                                .iter()
                                .map(|(k, v)| {
                                    (
                                        k.to_string(),
                                        match v {
                                            Some(x) => Json::s(*x),
                                            None => Json::Null,
                                        },
                                    )
                                })
                                .collect(),
                        ),
                    ),
                ])
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_catalog_is_wellformed() {
        let mut seen = Vec::new();
        for a in APPS {
            assert!(!seen.contains(&a.id), "duplicate app id {}", a.id);
            seen.push(a.id);
            assert!(!a.title.is_empty() && !a.desc.is_empty(), "{}", a.id);
            for (sym, _) in a.kernel {
                assert!(sym.starts_with("CONFIG_"), "{}: {sym}", a.id);
            }
            for list in [a.deb, a.rpm, a.arch] {
                for p in list {
                    assert!(valid_package(p), "{}: bad package name {p}", a.id);
                }
            }
            // Every app can be installed somehow on a Debian-family system.
            assert!(
                !a.deb.is_empty() || a.flatpak.is_some() || a.script.is_some(),
                "{} has no way to install on Debian/Ubuntu",
                a.id
            );
        }
    }

    #[test]
    fn apps_never_disagree_with_each_other_about_a_symbol() {
        let mut clashes = Vec::new();
        for (i, a) in APPS.iter().enumerate() {
            for b in APPS.iter().skip(i + 1) {
                for (sa, va) in a.kernel {
                    for (sb, vb) in b.kernel {
                        if sa == sb && va != vb {
                            clashes.push(format!("{} vs {} on {sa}", a.id, b.id));
                        }
                    }
                }
            }
        }
        assert!(clashes.is_empty(), "{clashes:#?}");
    }

    #[test]
    fn selecting_an_app_adds_its_kernel_requirements() {
        let r = resolve(&BTreeMap::new(), &ids(&["steam"]), "");
        let frag: BTreeMap<_, _> = r.fragment.iter().cloned().collect();
        assert_eq!(frag["CONFIG_IA32_EMULATION"], Some("y".to_string()));
        assert_eq!(frag["CONFIG_HID_STEAM"], Some("m".to_string()));
        assert_eq!(r.origin["CONFIG_IA32_EMULATION"], "app:steam");
        assert!(r.unknown.is_empty());

        // An app with no kernel needs changes nothing in the config.
        let gimp = resolve(&BTreeMap::new(), &ids(&["gimp"]), "");
        assert!(gimp.fragment.is_empty());
    }

    #[test]
    fn an_app_overriding_a_tweak_is_reported() {
        let mut sel = BTreeMap::new();
        sel.insert("kvm".to_string(), "off".to_string());
        let r = resolve(&sel, &ids(&["virt-manager"]), "");
        let frag: BTreeMap<_, _> = r.fragment.iter().cloned().collect();
        // The app's need wins...
        assert_eq!(frag["CONFIG_KVM"], Some("m".to_string()));
        // ...and the user is told their "No KVM" choice was overridden.
        let c = r
            .conflicts
            .iter()
            .find(|c| c.symbol == "CONFIG_KVM")
            .unwrap();
        assert_eq!(c.previous_tweak, "kvm");
        assert_eq!(c.winning_tweak, "app:virt-manager");
        // Extra options still have the last word over an app.
        let r2 = resolve(
            &BTreeMap::new(),
            &ids(&["steam"]),
            "CONFIG_IA32_EMULATION=n\n",
        );
        let frag2: BTreeMap<_, _> = r2.fragment.iter().cloned().collect();
        assert_eq!(frag2["CONFIG_IA32_EMULATION"], None);
        assert_eq!(r2.conflicts[0].previous_tweak, "app:steam");
    }

    #[test]
    fn unknown_apps_are_surfaced() {
        let r = resolve(&BTreeMap::new(), &ids(&["steam", "zzqx-nope"]), "");
        assert_eq!(r.unknown, vec!["app:zzqx-nope"]);
    }

    #[test]
    fn package_names_are_validated() {
        for good in [
            "gimp",
            "libc6-dev",
            "g++",
            "python3.12",
            "qemu-system-x86",
            "foo:i386",
        ] {
            assert!(valid_package(good), "{good}");
        }
        for bad in [
            "",
            "-rf",
            "a b",
            "gimp;reboot",
            "$(id)",
            "`id`",
            "a|b",
            "../x",
            "a&&b",
            "x>y",
            &"n".repeat(65),
        ] {
            assert!(!valid_package(bad), "{bad:?} must be rejected");
        }
        let (good, bad) = parse_packages("htop, gimp  vim\ngimp ;rm $(x)");
        assert_eq!(good, vec!["htop", "gimp", "vim"]);
        assert_eq!(bad, vec![";rm", "$(x)"]);
    }

    #[test]
    fn package_listings_are_parsed_and_searched() {
        let names = parse_names("gimp\n  htop \n\nlibc6\nwine\n");
        assert_eq!(names.len(), 4);
        assert!(listed(&names, "gimp"));
        assert!(listed(&names, "htop"));
        // A Debian architecture qualifier still finds the bare name.
        assert!(listed(&names, "wine:i386"));
        assert!(!listed(&names, "gurt"));
        assert!(!listed(&names, "gim"));
        assert!(parse_names("").is_empty());
        // An invalid name is never looked up at all.
        assert_eq!(available("deb", "gimp; reboot"), Some(false));
        // A family with no supported package manager cannot be checked.
        assert_eq!(available("unknown", "gimp"), None);
    }

    #[test]
    fn plan_prefers_native_packages() {
        let everything = |_: &str| Some(true);
        let p = plan(
            &ids(&["flatpak", "gimp", "steam"]),
            &ids(&["htop"]),
            "deb",
            &everything,
        );
        assert_eq!(
            p.commands,
            vec!["sudo apt install -y flatpak gimp steam-installer htop"]
        );
        assert!(p.notes.is_empty() && p.not_found.is_empty());

        let arch = plan(&ids(&["steam"]), &[], "arch", &everything);
        assert_eq!(arch.commands, vec!["sudo pacman -S --needed steam"]);
    }

    #[test]
    fn plan_falls_back_to_flatpak_and_skips_unknown_names() {
        // steam-installer is not in the enabled repos; "zzqx-nope" does not exist.
        let check = |name: &str| Some(!matches!(name, "steam-installer" | "zzqx-nope"));
        let p = plan(
            &ids(&["steam", "gimp"]),
            &ids(&["zzqx-nope", "htop"]),
            "deb",
            &check,
        );
        assert_eq!(
            p.commands,
            vec![
                "sudo apt install -y gimp htop flatpak",
                "sudo flatpak remote-add --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo",
                "flatpak install -y flathub com.valvesoftware.Steam",
            ]
        );
        assert_eq!(p.not_found, vec!["zzqx-nope"]);
        assert!(p
            .notes
            .iter()
            .any(|n| n.contains("Steam") && n.contains("Flatpak")));
        assert!(p.notes.iter().any(|n| n.contains("'zzqx-nope'")));
        // The unknown name is nowhere in what gets run.
        assert!(!p.commands.iter().any(|c| c.contains("zzqx-nope")));
    }

    #[test]
    fn gurt_installs_with_its_script_in_two_readable_steps() {
        let nothing_native = |name: &str| Some(name != "gurt");
        let p = plan(&ids(&["gurt", "gimp"]), &[], "deb", &nothing_native);
        assert_eq!(
            p.commands,
            vec![
                "sudo apt install -y gimp",
                "curl -fsSL https://gurtproject.org/install.sh -o gurt-install.sh",
                "sh gurt-install.sh",
            ]
        );
        assert!(
            p.not_found.is_empty(),
            "gurt is a known app, not a missing package"
        );
        // The user is told it is an unvetted script and where it comes from.
        let note = p.notes.iter().find(|n| n.contains("gurt")).unwrap();
        assert!(
            note.contains("gurtproject.org") && note.contains("read gurt-install.sh"),
            "{note}"
        );
        // Fetched to a file, never piped straight into a shell.
        assert!(
            !p.commands.iter().any(|c| c.contains('|')),
            "{:?}",
            p.commands
        );
    }

    #[test]
    fn gurt_typed_as_a_package_name_means_the_gurt_app() {
        let no_gurt_package = |name: &str| Some(name != "gurt");
        let typed = plan(&[], &ids(&["gurt", "htop"]), "deb", &no_gurt_package);
        assert_eq!(
            typed.commands,
            vec![
                "sudo apt install -y htop",
                "curl -fsSL https://gurtproject.org/install.sh -o gurt-install.sh",
                "sh gurt-install.sh",
            ]
        );
        assert!(typed.not_found.is_empty());

        // Listed both ways, it is still installed once.
        let both = plan(&ids(&["gurt"]), &ids(&["gurt"]), "deb", &no_gurt_package);
        assert_eq!(both.commands.len(), 2);

        // If a distribution ever does package something called gurt, an
        // explicit package name means that package.
        let packaged = plan(&[], &ids(&["gurt"]), "deb", &|_| Some(true));
        assert_eq!(packaged.commands, vec!["sudo apt install -y gurt"]);
    }

    #[test]
    fn installer_scripts_are_https_and_shell_safe() {
        for a in APPS {
            let Some(url) = a.script else { continue };
            assert!(url.starts_with("https://"), "{}: {url} must be https", a.id);
            assert!(
                url.bytes()
                    .all(|b| b.is_ascii_alphanumeric()
                        || matches!(b, b':' | b'/' | b'.' | b'-' | b'_')),
                "{}: {url} has characters a shell could interpret",
                a.id
            );
            assert!(valid_package(&script_file(a)), "{}", script_file(a));
            assert!(!script_host(url).is_empty() && !script_host(url).contains('/'));
        }
        assert_eq!(
            script_host("https://gurtproject.org/install.sh"),
            "gurtproject.org"
        );
        assert_eq!(
            app("gurt").unwrap().script,
            Some("https://gurtproject.org/install.sh")
        );
    }

    #[test]
    fn plan_handles_flatpak_only_apps_and_unknown_distros() {
        let everything = |_: &str| Some(true);
        let p = plan(&ids(&["discord"]), &[], "deb", &everything);
        assert_eq!(p.commands.len(), 3);
        assert_eq!(p.commands[0], "sudo apt install -y flatpak");
        assert!(p.commands[2].ends_with("com.discordapp.Discord"));

        // No known package manager: say what to install instead of guessing.
        let unknown = plan(&ids(&["gimp"]), &[], "unknown", &|_| None);
        assert!(unknown.commands[0].starts_with("sudo flatpak remote-add"));

        assert!(plan(&[], &[], "deb", &everything).is_empty());
    }

    #[test]
    fn injection_attempts_never_reach_a_command() {
        let everything = |_: &str| Some(true);
        let p = plan(
            &[],
            &ids(&["gimp; rm -rf ~", "$(curl evil)", "vim"]),
            "deb",
            &everything,
        );
        assert_eq!(p.commands, vec!["sudo apt install -y vim"]);
    }
}
