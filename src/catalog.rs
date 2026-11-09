//! The catalog of tweaks the GUI offers, plus ready-made profiles.
//!
//! A [`Tweak`] is one knob a human can reason about ("how low-latency should
//! the scheduler be"), which expands into one or more `CONFIG_` symbols.
//!
//! Kconfig's dependency solver is deliberately *not* reimplemented here. The
//! fragment is written to `.config`, `make olddefconfig` is asked to make it
//! consistent, and [`crate::kconfig::verify`] then reports which requests did
//! not survive. An impossible combination becomes a clear warning instead of a
//! silent surprise after a two-hour build.

use crate::json::Json;
use crate::kconfig::Value;
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Shorthands for config values used throughout the catalog.
pub const Y: Option<&str> = Some("y");
pub const M: Option<&str> = Some("m");
pub const OFF: Option<&str> = None;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk {
    /// Normal tuning; boots anywhere.
    #[default]
    Safe,
    /// Changes behaviour noticeably, or needs extra userspace.
    Caution,
    /// Can make the machine unbootable or materially less secure.
    Risky,
}

impl Risk {
    pub fn as_str(self) -> &'static str {
        match self {
            Risk::Safe => "safe",
            Risk::Caution => "caution",
            Risk::Risky => "risky",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Pick one of several options.
    Choice,
    /// On / off.
    Toggle,
    /// Free text written to a single symbol (quoted).
    Text,
    /// Integer written to a single symbol.
    Number,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Choice => "choice",
            Kind::Toggle => "toggle",
            Kind::Text => "text",
            Kind::Number => "number",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Opt {
    pub id: &'static str,
    pub label: &'static str,
    pub desc: &'static str,
    pub sets: Vec<(&'static str, Option<&'static str>)>,
    pub risk: Risk,
    pub note: &'static str,
    /// Host tools this option needs; kbuild silently drops it without them.
    pub needs: Vec<&'static str>,
}

impl Opt {
    pub fn needs(mut self, tools: &[&'static str]) -> Self {
        self.needs = tools.to_vec();
        self
    }
    pub fn risk(mut self, risk: Risk) -> Self {
        self.risk = risk;
        self
    }
    pub fn note(mut self, note: &'static str) -> Self {
        self.note = note;
        self
    }
    fn to_json(&self) -> Json {
        Json::obj(vec![
            ("id", Json::s(self.id)),
            ("label", Json::s(self.label)),
            ("description", Json::s(self.desc)),
            ("risk", Json::s(self.risk.as_str())),
            ("note", Json::s(self.note)),
            ("needs", Json::strs(self.needs.clone())),
            (
                "sets",
                Json::Obj(
                    self.sets
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
    }
}

/// Build an option. `sets` is the config fragment it applies.
pub fn opt(
    id: &'static str,
    label: &'static str,
    desc: &'static str,
    sets: &[(&'static str, Option<&'static str>)],
) -> Opt {
    Opt {
        id,
        label,
        desc,
        sets: sets.to_vec(),
        risk: Risk::Safe,
        note: "",
        needs: Vec::new(),
    }
}

/// The "leave this alone" option every tweak starts with.
fn keep(desc: &'static str) -> Opt {
    opt("keep", "Leave as-is", desc, &[])
}

#[derive(Debug, Clone)]
pub struct Tweak {
    pub id: &'static str,
    pub group: &'static str,
    pub title: &'static str,
    pub desc: &'static str,
    pub kind: Kind,
    pub symbol: &'static str,
    pub placeholder: &'static str,
    pub risk: Risk,
    pub docs: &'static str,
    pub tags: Vec<&'static str>,
    pub opts: Vec<Opt>,
}

impl Tweak {
    fn new(id: &'static str, group: &'static str, title: &'static str, desc: &'static str) -> Self {
        Tweak {
            id,
            group,
            title,
            desc,
            kind: Kind::Choice,
            symbol: "",
            placeholder: "",
            risk: Risk::Safe,
            docs: "",
            tags: Vec::new(),
            opts: vec![keep("Leave whatever the base config has.")],
        }
    }
    fn toggle(mut self) -> Self {
        self.kind = Kind::Toggle;
        self
    }
    fn text(mut self, symbol: &'static str, placeholder: &'static str) -> Self {
        self.kind = Kind::Text;
        self.symbol = symbol;
        self.placeholder = placeholder;
        self
    }
    fn number(mut self, symbol: &'static str, placeholder: &'static str) -> Self {
        self.kind = Kind::Number;
        self.symbol = symbol;
        self.placeholder = placeholder;
        self
    }
    fn risk(mut self, risk: Risk) -> Self {
        self.risk = risk;
        self
    }
    fn docs(mut self, url: &'static str) -> Self {
        self.docs = url;
        self
    }
    fn tags(mut self, tags: &[&'static str]) -> Self {
        self.tags = tags.to_vec();
        self
    }
    /// Override the wording of the "leave as-is" option.
    fn keep_text(mut self, desc: &'static str) -> Self {
        self.opts[0] = keep(desc);
        self
    }
    fn opt(mut self, o: Opt) -> Self {
        self.opts.push(o);
        self
    }

    pub fn option(&self, id: &str) -> Option<&Opt> {
        self.opts.iter().find(|o| o.id == id)
    }

    fn to_json(&self) -> Json {
        Json::obj(vec![
            ("id", Json::s(self.id)),
            ("group", Json::s(self.group)),
            ("title", Json::s(self.title)),
            ("description", Json::s(self.desc)),
            ("kind", Json::s(self.kind.as_str())),
            ("symbol", Json::s(self.symbol)),
            ("placeholder", Json::s(self.placeholder)),
            ("risk", Json::s(self.risk.as_str())),
            ("docs", Json::s(self.docs)),
            ("tags", Json::strs(self.tags.clone())),
            (
                "options",
                Json::arr(self.opts.iter().map(Opt::to_json).collect()),
            ),
        ])
    }
}

pub struct Group {
    pub id: &'static str,
    pub title: &'static str,
    pub blurb: &'static str,
}

pub const GROUPS: &[Group] = &[
    Group { id: "latency", title: "Latency & scheduling",
        blurb: "How quickly the kernel reacts. Desktop audio and games want low latency; servers want throughput." },
    Group { id: "cpu", title: "CPU & power",
        blurb: "Frequency scaling defaults, sleep states and how the kernel is tuned for your chip." },
    Group { id: "memory", title: "Memory",
        blurb: "Huge pages, compressed swap and page reclaim behaviour." },
    Group { id: "storage", title: "Storage & filesystems",
        blurb: "I/O schedulers and which filesystems get built in." },
    Group { id: "network", title: "Networking",
        blurb: "Congestion control, queueing and VPN support." },
    Group { id: "graphics", title: "Graphics & hardware",
        blurb: "GPU drivers. Build yours as modules and drop the rest to cut build time." },
    Group { id: "virt", title: "Virtualisation & containers",
        blurb: "KVM, virtio guests, VFIO passthrough and the bits Docker and Podman look for." },
    Group { id: "security", title: "Security & hardening",
        blurb: "Memory hardening, lockdown and attack-surface trimming." },
    Group { id: "size", title: "Build size & speed",
        blurb: "The options that decide whether your build takes 15 minutes or 2 hours." },
    Group { id: "identity", title: "Naming",
        blurb: "What your kernel calls itself in uname and under /lib/modules." },
];

fn build_catalog() -> Vec<Tweak> {
    vec![
        // ----------------------------------------------------------- latency
        Tweak::new("preempt", "latency", "Preemption model",
            "How aggressively the kernel lets a high-priority task interrupt whatever is running. Lower latency costs a little throughput.")
            .docs("https://docs.kernel.org/scheduler/sched-design-CFS.html")
            .tags(&["desktop", "audio", "gaming", "server"])
            .opt(opt("none", "No forced preemption (server)",
                "Best raw throughput, worst worst-case latency. The classic server choice.",
                &[("CONFIG_PREEMPT_NONE", Y), ("CONFIG_PREEMPT_VOLUNTARY", OFF),
                  ("CONFIG_PREEMPT", OFF), ("CONFIG_PREEMPT_RT", OFF)]))
            .opt(opt("voluntary", "Voluntary preemption (balanced)",
                "Adds explicit reschedule points. The usual distro default.",
                &[("CONFIG_PREEMPT_NONE", OFF), ("CONFIG_PREEMPT_VOLUNTARY", Y),
                  ("CONFIG_PREEMPT", OFF), ("CONFIG_PREEMPT_RT", OFF)]))
            .opt(opt("full", "Full preemption (desktop / low latency)",
                "Preemptible kernel. What you want for a responsive desktop, audio work and games.",
                &[("CONFIG_PREEMPT_NONE", OFF), ("CONFIG_PREEMPT_VOLUNTARY", OFF),
                  ("CONFIG_PREEMPT", Y), ("CONFIG_PREEMPT_RT", OFF)]))
            .opt(opt("rt", "Real-time (PREEMPT_RT)",
                "Hard real-time. Needs mainline 6.12 or later, or an RT patch set.",
                &[("CONFIG_PREEMPT_NONE", OFF), ("CONFIG_PREEMPT_VOLUNTARY", OFF),
                  ("CONFIG_PREEMPT", OFF), ("CONFIG_PREEMPT_RT", Y), ("CONFIG_EXPERT", Y)])
                .risk(Risk::Caution)
                .note("Some drivers misbehave under RT, and RT is unavailable on kernels older than 6.12 without a patch.")),

        Tweak::new("hz", "latency", "Timer frequency",
            "How often the scheduler tick fires. Higher means finer-grained timing and slightly more overhead.")
            .tags(&["desktop", "gaming", "server"])
            .opt(opt("100", "100 Hz - servers and batch work", "Least overhead, coarsest timing.",
                &[("CONFIG_HZ_100", Y), ("CONFIG_HZ_250", OFF), ("CONFIG_HZ_300", OFF),
                  ("CONFIG_HZ_1000", OFF), ("CONFIG_HZ", Some("100"))]))
            .opt(opt("250", "250 Hz - balanced", "A common distro default.",
                &[("CONFIG_HZ_100", OFF), ("CONFIG_HZ_250", Y), ("CONFIG_HZ_300", OFF),
                  ("CONFIG_HZ_1000", OFF), ("CONFIG_HZ", Some("250"))]))
            .opt(opt("300", "300 Hz - video friendly",
                "Divides evenly into 24, 30 and 60 fps, so it is kind to video playback.",
                &[("CONFIG_HZ_100", OFF), ("CONFIG_HZ_250", OFF), ("CONFIG_HZ_300", Y),
                  ("CONFIG_HZ_1000", OFF), ("CONFIG_HZ", Some("300"))]))
            .opt(opt("1000", "1000 Hz - desktop and interactive",
                "Finest timing. Pairs with full preemption for a snappy desktop.",
                &[("CONFIG_HZ_100", OFF), ("CONFIG_HZ_250", OFF), ("CONFIG_HZ_300", OFF),
                  ("CONFIG_HZ_1000", Y), ("CONFIG_HZ", Some("1000"))])),

        Tweak::new("tickless", "latency", "Tickless operation",
            "Whether the timer tick stops on idle CPUs (saves power) or on busy ones too (helps compute and real-time work).")
            .opt(opt("periodic", "Always tick", "Simplest, highest idle power draw.",
                &[("CONFIG_HZ_PERIODIC", Y), ("CONFIG_NO_HZ_IDLE", OFF), ("CONFIG_NO_HZ_FULL", OFF)]))
            .opt(opt("idle", "Tickless when idle (recommended)",
                "Stops the tick on idle CPUs. The sane default for laptops and desktops.",
                &[("CONFIG_HZ_PERIODIC", OFF), ("CONFIG_NO_HZ_IDLE", Y), ("CONFIG_NO_HZ_FULL", OFF),
                  ("CONFIG_NO_HZ_COMMON", Y), ("CONFIG_NO_HZ", Y)]))
            .opt(opt("full", "Full tickless (CPU isolation)",
                "Can stop the tick on busy CPUs too. Needs nohz_full= on the command line to do anything.",
                &[("CONFIG_HZ_PERIODIC", OFF), ("CONFIG_NO_HZ_IDLE", OFF), ("CONFIG_NO_HZ_FULL", Y),
                  ("CONFIG_NO_HZ_COMMON", Y), ("CONFIG_NO_HZ", Y), ("CONFIG_CPU_ISOLATION", Y),
                  ("CONFIG_RCU_NOCB_CPU", Y)])
                .risk(Risk::Caution)),

        Tweak::new("autogroup", "latency", "Scheduler task autogrouping",
            "Groups tasks by session so one busy compile does not starve your browser. Great on desktops, usually off on servers.")
            .toggle()
            .opt(opt("on", "Enable", "Fairer sharing between sessions.", &[("CONFIG_SCHED_AUTOGROUP", Y)]))
            .opt(opt("off", "Disable", "Per-task fairness only.", &[("CONFIG_SCHED_AUTOGROUP", OFF)])),

        // --------------------------------------------------------------- cpu
        Tweak::new("cpufreq_gov", "cpu", "Default CPU frequency governor",
            "Which governor the kernel starts with. You can still switch at runtime; this only picks the default.")
            .tags(&["laptop", "desktop", "server"])
            .opt(opt("schedutil", "schedutil (modern default)",
                "Driven by scheduler utilisation. Best all-rounder on recent kernels.",
                &[("CONFIG_CPU_FREQ_DEFAULT_GOV_SCHEDUTIL", Y), ("CONFIG_CPU_FREQ_DEFAULT_GOV_PERFORMANCE", OFF),
                  ("CONFIG_CPU_FREQ_DEFAULT_GOV_POWERSAVE", OFF), ("CONFIG_CPU_FREQ_DEFAULT_GOV_ONDEMAND", OFF),
                  ("CONFIG_CPU_FREQ_GOV_SCHEDUTIL", Y),
                  ("CONFIG_CPU_FREQ_DEFAULT_GOV_USERSPACE", OFF), ("CONFIG_CPU_FREQ_DEFAULT_GOV_CONSERVATIVE", OFF)]))
            .opt(opt("performance", "performance",
                "Pin clocks high. Lowest latency, highest power draw and heat.",
                &[("CONFIG_CPU_FREQ_DEFAULT_GOV_PERFORMANCE", Y), ("CONFIG_CPU_FREQ_DEFAULT_GOV_SCHEDUTIL", OFF),
                  ("CONFIG_CPU_FREQ_DEFAULT_GOV_POWERSAVE", OFF), ("CONFIG_CPU_FREQ_DEFAULT_GOV_ONDEMAND", OFF),
                  ("CONFIG_CPU_FREQ_GOV_PERFORMANCE", Y),
                  ("CONFIG_CPU_FREQ_DEFAULT_GOV_USERSPACE", OFF), ("CONFIG_CPU_FREQ_DEFAULT_GOV_CONSERVATIVE", OFF)]))
            .opt(opt("ondemand", "ondemand", "The older ramp-up-on-load governor.",
                &[("CONFIG_CPU_FREQ_DEFAULT_GOV_ONDEMAND", Y), ("CONFIG_CPU_FREQ_DEFAULT_GOV_SCHEDUTIL", OFF),
                  ("CONFIG_CPU_FREQ_DEFAULT_GOV_PERFORMANCE", OFF), ("CONFIG_CPU_FREQ_DEFAULT_GOV_POWERSAVE", OFF),
                  ("CONFIG_CPU_FREQ_GOV_ONDEMAND", Y),
                  ("CONFIG_CPU_FREQ_DEFAULT_GOV_USERSPACE", OFF), ("CONFIG_CPU_FREQ_DEFAULT_GOV_CONSERVATIVE", OFF)])
                .note("Cannot be the default on a kernel built with the Intel P-state driver (X86_INTEL_PSTATE); kbuild falls back to schedutil there."))
            .opt(opt("powersave", "powersave", "Stay low. Long battery life, sluggish under load.",
                &[("CONFIG_CPU_FREQ_DEFAULT_GOV_POWERSAVE", Y), ("CONFIG_CPU_FREQ_DEFAULT_GOV_SCHEDUTIL", OFF),
                  ("CONFIG_CPU_FREQ_DEFAULT_GOV_PERFORMANCE", OFF), ("CONFIG_CPU_FREQ_DEFAULT_GOV_ONDEMAND", OFF),
                  ("CONFIG_CPU_FREQ_GOV_POWERSAVE", Y),
                  ("CONFIG_CPU_FREQ_DEFAULT_GOV_USERSPACE", OFF), ("CONFIG_CPU_FREQ_DEFAULT_GOV_CONSERVATIVE", OFF)])),

        Tweak::new("x86_cpu_family", "cpu", "Processor family (x86-64)",
            "Lets the compiler assume your CPU's instruction set. 'Generic' runs anywhere; the specific ones only boot on matching hardware.")
            .risk(Risk::Caution)
            .opt(opt("generic", "Generic x86-64 (portable)", "Runs on any 64-bit x86 machine.",
                &[("CONFIG_GENERIC_CPU", Y), ("CONFIG_MK8", OFF), ("CONFIG_MPSC", OFF),
                  ("CONFIG_MCORE2", OFF), ("CONFIG_MATOM", OFF)]))
            .opt(opt("core2", "Intel Core 2 and newer", "Tunes for Intel. Only boot this on an Intel box.",
                &[("CONFIG_MCORE2", Y), ("CONFIG_GENERIC_CPU", OFF), ("CONFIG_MK8", OFF),
                  ("CONFIG_MPSC", OFF), ("CONFIG_MATOM", OFF)]).risk(Risk::Caution))
            .opt(opt("k8", "AMD Opteron / Athlon64 and newer", "Tunes for AMD. Only boot this on an AMD box.",
                &[("CONFIG_MK8", Y), ("CONFIG_GENERIC_CPU", OFF), ("CONFIG_MCORE2", OFF),
                  ("CONFIG_MPSC", OFF), ("CONFIG_MATOM", OFF)]).risk(Risk::Caution))
            .opt(opt("atom", "Intel Atom", "In-order Atom tuning.",
                &[("CONFIG_MATOM", Y), ("CONFIG_GENERIC_CPU", OFF), ("CONFIG_MK8", OFF),
                  ("CONFIG_MCORE2", OFF), ("CONFIG_MPSC", OFF)]).risk(Risk::Caution)),

        Tweak::new("nr_cpus", "cpu", "Maximum CPUs",
            "Upper bound on CPUs the kernel supports. Lowering it to just above your thread count shaves a little memory off per-CPU data.")
            .number("CONFIG_NR_CPUS", "e.g. 16"),

        Tweak::new("power_mgmt", "cpu", "Suspend & hibernate",
            "Suspend-to-RAM and suspend-to-disk. Dropping hibernate saves a little size on machines that never use it.")
            .opt(opt("both", "Suspend + hibernate", "Everything a laptop wants.",
                &[("CONFIG_SUSPEND", Y), ("CONFIG_HIBERNATION", Y), ("CONFIG_PM", Y)]))
            .opt(opt("suspend_only", "Suspend only", "Keeps sleep, drops suspend-to-disk.",
                &[("CONFIG_SUSPEND", Y), ("CONFIG_HIBERNATION", OFF), ("CONFIG_PM", Y)]))
            .opt(opt("none", "Neither (always-on machine)", "For servers that never sleep.",
                &[("CONFIG_SUSPEND", OFF), ("CONFIG_HIBERNATION", OFF)]).risk(Risk::Caution)),

        // ------------------------------------------------------------ memory
        Tweak::new("thp", "memory", "Transparent huge pages",
            "Backs memory with 2 MiB pages. A big win for databases and VMs; occasional latency spikes on desktops when set to 'always'.")
            .opt(opt("always", "Always", "Maximum throughput, some allocation stalls.",
                &[("CONFIG_TRANSPARENT_HUGEPAGE", Y), ("CONFIG_TRANSPARENT_HUGEPAGE_ALWAYS", Y),
                  ("CONFIG_TRANSPARENT_HUGEPAGE_MADVISE", OFF)]))
            .opt(opt("madvise", "Only when asked (madvise)", "Apps opt in. The safer default.",
                &[("CONFIG_TRANSPARENT_HUGEPAGE", Y), ("CONFIG_TRANSPARENT_HUGEPAGE_ALWAYS", OFF),
                  ("CONFIG_TRANSPARENT_HUGEPAGE_MADVISE", Y)]))
            .opt(opt("off", "Disabled", "4 KiB pages only.", &[("CONFIG_TRANSPARENT_HUGEPAGE", OFF)])),

        Tweak::new("mglru", "memory", "Multi-Gen LRU reclaim",
            "A newer page-reclaim design that holds up much better under memory pressure. Needs kernel 6.1 or later.")
            .toggle()
            .opt(opt("on", "Enable and turn on by default", "Noticeably less thrashing on low-RAM machines.",
                &[("CONFIG_LRU_GEN", Y), ("CONFIG_LRU_GEN_ENABLED", Y)]))
            .opt(opt("off", "Disable", "Classic active/inactive LRU.", &[("CONFIG_LRU_GEN", OFF)])),

        Tweak::new("zswap", "memory", "Compressed swap (zswap)",
            "Compresses swapped-out pages in RAM before touching the disk. A cheap way to make a low-RAM machine usable.")
            .opt(opt("on_zstd", "Enable, default on, zstd", "Best compression ratio for the CPU cost.",
                &[("CONFIG_ZSWAP", Y), ("CONFIG_ZSWAP_DEFAULT_ON", Y),
                  ("CONFIG_ZSWAP_COMPRESSOR_DEFAULT_ZSTD", Y), ("CONFIG_ZSWAP_ZPOOL_DEFAULT_ZSMALLOC", Y),
                  ("CONFIG_ZSMALLOC", Y), ("CONFIG_CRYPTO_ZSTD", Y)]))
            .opt(opt("on_lzo", "Enable, default on, lzo", "Faster, compresses less.",
                &[("CONFIG_ZSWAP", Y), ("CONFIG_ZSWAP_DEFAULT_ON", Y),
                  ("CONFIG_ZSWAP_COMPRESSOR_DEFAULT_LZO", Y), ("CONFIG_ZSMALLOC", Y), ("CONFIG_CRYPTO_LZO", Y)]))
            .opt(opt("available", "Build it, leave it off", "Enable later with zswap.enabled=1.",
                &[("CONFIG_ZSWAP", Y), ("CONFIG_ZSWAP_DEFAULT_ON", OFF), ("CONFIG_ZSMALLOC", Y)]))
            .opt(opt("off", "Disabled", "Plain swap.", &[("CONFIG_ZSWAP", OFF)])),

        Tweak::new("zram", "memory", "zram block device",
            "A compressed RAM disk, usually used as swap by modern distros.")
            .toggle()
            .opt(opt("on", "Enable (module)", "What zram-generator expects.",
                &[("CONFIG_ZRAM", M), ("CONFIG_ZRAM_DEF_COMP_ZSTD", Y), ("CONFIG_CRYPTO_ZSTD", Y)]))
            .opt(opt("off", "Disable", "", &[("CONFIG_ZRAM", OFF)])),

        Tweak::new("ksm", "memory", "Kernel same-page merging",
            "De-duplicates identical memory pages. Saves real RAM when you run many similar VMs or containers.")
            .toggle()
            .opt(opt("on", "Enable", "", &[("CONFIG_KSM", Y)]))
            .opt(opt("off", "Disable", "", &[("CONFIG_KSM", OFF)])),

        // ----------------------------------------------------------- storage
        Tweak::new("io_sched", "storage", "I/O schedulers",
            "Which block schedulers are available. BFQ is kind to desktop interactivity; none or mq-deadline suit fast NVMe.")
            .opt(opt("desktop", "Include BFQ (desktop)", "BFQ plus deadline and kyber.",
                &[("CONFIG_IOSCHED_BFQ", Y), ("CONFIG_BFQ_GROUP_IOSCHED", Y),
                  ("CONFIG_MQ_IOSCHED_DEADLINE", Y), ("CONFIG_MQ_IOSCHED_KYBER", Y)]))
            .opt(opt("server", "Deadline + kyber only", "A lean set for NVMe-backed servers.",
                &[("CONFIG_IOSCHED_BFQ", OFF), ("CONFIG_MQ_IOSCHED_DEADLINE", Y),
                  ("CONFIG_MQ_IOSCHED_KYBER", Y)])),

        Tweak::new("filesystems", "storage", "Filesystem support",
            "Which filesystems to build. Keep the one your root partition uses built in (=y), not as a module.")
            .opt(opt("common", "Common set (ext4, btrfs, xfs, vfat, exfat, ntfs3)",
                "Covers almost every desktop install.",
                &[("CONFIG_EXT4_FS", Y), ("CONFIG_BTRFS_FS", M), ("CONFIG_XFS_FS", M),
                  ("CONFIG_FAT_FS", Y), ("CONFIG_VFAT_FS", Y), ("CONFIG_EXFAT_FS", M),
                  ("CONFIG_NTFS3_FS", M), ("CONFIG_FUSE_FS", M), ("CONFIG_OVERLAY_FS", M),
                  ("CONFIG_ISO9660_FS", M), ("CONFIG_UDF_FS", M), ("CONFIG_F2FS_FS", M),
                  ("CONFIG_NLS_UTF8", Y), ("CONFIG_NLS_CODEPAGE_437", Y)]))
            .opt(opt("ext4_only", "Minimal (ext4 + vfat for /boot/efi)",
                "The smallest sensible set for a single-purpose machine.",
                &[("CONFIG_EXT4_FS", Y), ("CONFIG_FAT_FS", Y), ("CONFIG_VFAT_FS", Y),
                  ("CONFIG_BTRFS_FS", OFF), ("CONFIG_XFS_FS", OFF), ("CONFIG_NTFS3_FS", OFF),
                  ("CONFIG_F2FS_FS", OFF), ("CONFIG_NLS_CODEPAGE_437", Y), ("CONFIG_NLS_UTF8", Y)])
                .risk(Risk::Caution)
                .note("Only pick this if you know every filesystem you mount at boot is in the list."))
            .opt(opt("btrfs_root", "Btrfs root", "Btrfs built in rather than modular.",
                &[("CONFIG_BTRFS_FS", Y), ("CONFIG_BTRFS_FS_POSIX_ACL", Y), ("CONFIG_EXT4_FS", M),
                  ("CONFIG_FAT_FS", Y), ("CONFIG_VFAT_FS", Y)])),

        Tweak::new("nfs_smb", "storage", "Network filesystems", "NFS and SMB/CIFS clients.")
            .toggle()
            .opt(opt("on", "Enable NFS + SMB clients", "",
                &[("CONFIG_NFS_FS", M), ("CONFIG_NFS_V4", M), ("CONFIG_CIFS", M)]))
            .opt(opt("off", "Disable", "", &[("CONFIG_NFS_FS", OFF), ("CONFIG_CIFS", OFF)])),

        // ----------------------------------------------------------- network
        Tweak::new("tcp_cc", "network", "Default TCP congestion control",
            "BBR usually gives much better throughput on lossy or long-haul links than the traditional CUBIC.")
            .opt(opt("bbr", "BBR", "Model-based control. Great over the internet.",
                &[("CONFIG_TCP_CONG_BBR", Y), ("CONFIG_DEFAULT_BBR", Y), ("CONFIG_DEFAULT_CUBIC", OFF),
                  ("CONFIG_NET_SCH_FQ", Y)]))
            .opt(opt("cubic", "CUBIC", "The long-standing Linux default.",
                &[("CONFIG_TCP_CONG_CUBIC", Y), ("CONFIG_DEFAULT_CUBIC", Y), ("CONFIG_DEFAULT_BBR", OFF)])),

        Tweak::new("qdisc", "network", "Queueing and bufferbloat",
            "fq_codel and CAKE keep latency low when the link is saturated.")
            .toggle()
            .opt(opt("on", "Build fq_codel + CAKE + fq", "Recommended for any machine that routes or uploads.",
                &[("CONFIG_NET_SCH_FQ_CODEL", Y), ("CONFIG_NET_SCH_CAKE", M), ("CONFIG_NET_SCH_FQ", Y),
                  ("CONFIG_NET_SCH_DEFAULT", Y), ("CONFIG_DEFAULT_FQ_CODEL", Y)]))
            .opt(opt("off", "Skip them", "", &[("CONFIG_NET_SCH_CAKE", OFF)])),

        Tweak::new("wireguard", "network", "WireGuard VPN",
            "In-kernel WireGuard. Much faster than a userspace implementation.")
            .toggle()
            .opt(opt("on", "Enable (module)", "", &[("CONFIG_WIREGUARD", M), ("CONFIG_NET_UDP_TUNNEL", M)]))
            .opt(opt("off", "Disable", "", &[("CONFIG_WIREGUARD", OFF)])),

        Tweak::new("ipv6", "network", "IPv6", "Leave this on. Plenty of userspace assumes it exists.")
            .toggle()
            .opt(opt("on", "Enable", "", &[("CONFIG_IPV6", Y)]))
            .opt(opt("off", "Disable", "Breaks a surprising amount of software.", &[("CONFIG_IPV6", OFF)])
                .risk(Risk::Risky)),

        // ---------------------------------------------------------- graphics
        Tweak::new("gpu", "graphics", "GPU drivers",
            "Each DRM driver is a big chunk of build time. Build only the one your machine has, as a module, so an initramfs can load it.")
            .opt(opt("amd", "AMD only (amdgpu)", "",
                &[("CONFIG_DRM", Y), ("CONFIG_DRM_AMDGPU", M), ("CONFIG_DRM_AMD_DC", Y),
                  ("CONFIG_DRM_I915", OFF), ("CONFIG_DRM_NOUVEAU", OFF), ("CONFIG_DRM_SIMPLEDRM", Y),
                  ("CONFIG_FB", Y), ("CONFIG_DRM_FBDEV_EMULATION", Y),
                  ("CONFIG_FRAMEBUFFER_CONSOLE", Y)]))
            .opt(opt("intel", "Intel only (i915 + xe)", "",
                &[("CONFIG_DRM", Y), ("CONFIG_DRM_I915", M), ("CONFIG_DRM_XE", M),
                  ("CONFIG_DRM_AMDGPU", OFF), ("CONFIG_DRM_NOUVEAU", OFF), ("CONFIG_DRM_SIMPLEDRM", Y),
                  ("CONFIG_FB", Y), ("CONFIG_DRM_FBDEV_EMULATION", Y),
                  ("CONFIG_FRAMEBUFFER_CONSOLE", Y)]))
            .opt(opt("nvidia_nouveau", "NVIDIA open driver (nouveau)",
                "The in-tree driver. The proprietary NVIDIA module is built separately through DKMS.",
                &[("CONFIG_DRM", Y), ("CONFIG_DRM_NOUVEAU", M), ("CONFIG_DRM_AMDGPU", OFF),
                  ("CONFIG_DRM_I915", OFF), ("CONFIG_DRM_SIMPLEDRM", Y), ("CONFIG_FB", Y), ("CONFIG_DRM_FBDEV_EMULATION", Y),
                  ("CONFIG_FRAMEBUFFER_CONSOLE", Y)]))
            .opt(opt("all", "All of them", "Slowest to build, works on any machine.",
                &[("CONFIG_DRM", Y), ("CONFIG_DRM_AMDGPU", M), ("CONFIG_DRM_I915", M),
                  ("CONFIG_DRM_NOUVEAU", M), ("CONFIG_DRM_SIMPLEDRM", Y), ("CONFIG_FB", Y), ("CONFIG_DRM_FBDEV_EMULATION", Y),
                  ("CONFIG_FRAMEBUFFER_CONSOLE", Y)]))
            .opt(opt("virtual", "Virtual machine display (virtio-gpu, QXL, bochs, VMware)",
                "For a VM guest with a graphical console. virtio-gpu also needs 'Run as a VM guest' on.",
                &[("CONFIG_DRM", Y), ("CONFIG_DRM_VIRTIO_GPU", M), ("CONFIG_DRM_QXL", M),
                  ("CONFIG_DRM_BOCHS", M), ("CONFIG_DRM_VMWGFX", M), ("CONFIG_DRM_AMDGPU", OFF),
                  ("CONFIG_DRM_I915", OFF), ("CONFIG_DRM_NOUVEAU", OFF), ("CONFIG_DRM_SIMPLEDRM", Y),
                  ("CONFIG_FB", Y), ("CONFIG_DRM_FBDEV_EMULATION", Y),
                  ("CONFIG_FRAMEBUFFER_CONSOLE", Y)]))
            .opt(opt("headless", "None (headless server)",
                "No DRM at all. Do not pick this for a machine with a screen.",
                &[("CONFIG_DRM", OFF), ("CONFIG_DRM_AMDGPU", OFF), ("CONFIG_DRM_I915", OFF),
                  ("CONFIG_DRM_NOUVEAU", OFF)])
                .risk(Risk::Risky)
                .note("You lose the graphical console. Serial or SSH only.")),

        Tweak::new("sound", "graphics", "Sound",
            "ALSA plus the HD-audio and USB-audio drivers almost every machine needs.")
            .toggle()
            .opt(opt("on", "Enable ALSA + HDA + USB audio", "",
                &[("CONFIG_SOUND", Y), ("CONFIG_SND", M), ("CONFIG_SND_HDA_INTEL", M),
                  ("CONFIG_SND_HDA_CODEC_HDMI", M), ("CONFIG_USB_SUPPORT", Y), ("CONFIG_USB", Y),
                  ("CONFIG_SND_USB", Y), ("CONFIG_SND_USB_AUDIO", M)]))
            .opt(opt("off", "No sound", "Server builds only.", &[("CONFIG_SOUND", OFF)]).risk(Risk::Caution)),

        // -------------------------------------------------------------- virt
        Tweak::new("kvm", "virt", "KVM host support",
            "Lets this machine run virtual machines through QEMU, libvirt, GNOME Boxes and friends.")
            .opt(opt("intel", "KVM for Intel", "",
                &[("CONFIG_VIRTUALIZATION", Y), ("CONFIG_KVM", M), ("CONFIG_KVM_INTEL", M), ("CONFIG_KVM_AMD", OFF)]))
            .opt(opt("amd", "KVM for AMD", "",
                &[("CONFIG_VIRTUALIZATION", Y), ("CONFIG_KVM", M), ("CONFIG_KVM_AMD", M), ("CONFIG_KVM_INTEL", OFF)]))
            .opt(opt("both", "Both", "Handy if you move the config between machines.",
                &[("CONFIG_VIRTUALIZATION", Y), ("CONFIG_KVM", M), ("CONFIG_KVM_INTEL", M), ("CONFIG_KVM_AMD", M)]))
            .opt(opt("off", "No KVM", "Smaller build, no local VMs.",
                &[("CONFIG_KVM", OFF), ("CONFIG_KVM_INTEL", OFF), ("CONFIG_KVM_AMD", OFF)])),

        Tweak::new("virtio_guest", "virt", "Run as a VM guest",
            "virtio disk, network and console drivers, so this kernel boots well inside QEMU/KVM, Proxmox and cloud instances. The virtual display lives under GPU drivers.")
            .toggle()
            .opt(opt("on", "Enable virtio guest drivers", "",
                &[("CONFIG_HYPERVISOR_GUEST", Y), ("CONFIG_PARAVIRT", Y), ("CONFIG_VIRTIO_MENU", Y),
                  ("CONFIG_VIRTIO_PCI", Y), ("CONFIG_VIRTIO_BLK", Y), ("CONFIG_VIRTIO_NET", Y),
                  ("CONFIG_VIRTIO_CONSOLE", Y), ("CONFIG_VIRTIO_BALLOON", Y), ("CONFIG_VIRTIO_MMIO", Y),
                  ("CONFIG_SCSI", Y), ("CONFIG_BLK_DEV_SD", Y), ("CONFIG_SCSI_VIRTIO", Y)]))
            .opt(opt("off", "Bare metal only", "Drops the virtio drivers. This kernel will not boot as a VM guest.",
                &[("CONFIG_VIRTIO_MENU", OFF), ("CONFIG_VIRTIO_PCI", OFF), ("CONFIG_VIRTIO_MMIO", OFF),
                  ("CONFIG_VIRTIO_BLK", OFF), ("CONFIG_VIRTIO_NET", OFF), ("CONFIG_VIRTIO_CONSOLE", OFF),
                  ("CONFIG_VIRTIO_BALLOON", OFF), ("CONFIG_SCSI_VIRTIO", OFF)])
                .risk(Risk::Caution)),

        Tweak::new("containers", "virt", "Container support",
            "Everything Docker, Podman and Kubernetes check for: namespaces, cgroup v2, overlayfs, veth and the netfilter bits.")
            .toggle()
            .opt(opt("on", "Enable container support", "",
                &[("CONFIG_NAMESPACES", Y), ("CONFIG_USER_NS", Y), ("CONFIG_PID_NS", Y), ("CONFIG_NET_NS", Y),
                  ("CONFIG_NETFILTER", Y), ("CONFIG_NETFILTER_ADVANCED", Y),
                  ("CONFIG_CGROUPS", Y), ("CONFIG_MEMCG", Y), ("CONFIG_CGROUP_PIDS", Y), ("CONFIG_CPUSETS", Y),
                  ("CONFIG_CGROUP_SCHED", Y), ("CONFIG_BLK_CGROUP", Y), ("CONFIG_OVERLAY_FS", M),
                  ("CONFIG_VETH", M), ("CONFIG_BRIDGE", M), ("CONFIG_BRIDGE_NETFILTER", M),
                  ("CONFIG_NF_NAT", M), ("CONFIG_IP_NF_IPTABLES", M), ("CONFIG_NF_TABLES", M),
                  ("CONFIG_SECCOMP", Y), ("CONFIG_SECCOMP_FILTER", Y)])),

        Tweak::new("vfio", "virt", "GPU / PCI passthrough (VFIO)",
            "Hand a whole PCI device to a virtual machine. The single-GPU-passthrough gaming VM setup needs this.")
            .toggle()
            .opt(opt("on", "Enable VFIO", "",
                &[("CONFIG_VFIO", M), ("CONFIG_VFIO_PCI", M), ("CONFIG_VFIO_IOMMU_TYPE1", M),
                  ("CONFIG_IOMMU_SUPPORT", Y), ("CONFIG_INTEL_IOMMU", Y), ("CONFIG_AMD_IOMMU", Y),
                  ("CONFIG_PCI_STUB", Y)]))
            .opt(opt("off", "Disable", "", &[("CONFIG_VFIO", OFF)])),

        // ---------------------------------------------------------- security
        Tweak::new("hardening", "security", "Memory hardening level",
            "Stacked defences against memory-corruption bugs. Each step costs a little performance.")
            .tags(&["security"])
            .opt(opt("standard", "Standard (distro-like)",
                "Stack protector, FORTIFY_SOURCE, KASLR and strict RWX.",
                &[("CONFIG_STACKPROTECTOR", Y), ("CONFIG_STACKPROTECTOR_STRONG", Y),
                  ("CONFIG_FORTIFY_SOURCE", Y), ("CONFIG_RANDOMIZE_BASE", Y), ("CONFIG_RANDOMIZE_MEMORY", Y),
                  ("CONFIG_STRICT_KERNEL_RWX", Y), ("CONFIG_STRICT_MODULE_RWX", Y),
                  ("CONFIG_HARDENED_USERCOPY", Y), ("CONFIG_SLAB_FREELIST_RANDOM", Y),
                  ("CONFIG_SLAB_FREELIST_HARDENED", Y), ("CONFIG_VMAP_STACK", Y)]))
            .opt(opt("strong", "Strong",
                "Adds zero-on-alloc and stack-leak clearing, and drops legacy vsyscalls. Expect a few percent overhead.",
                &[("CONFIG_STACKPROTECTOR", Y), ("CONFIG_STACKPROTECTOR_STRONG", Y),
                  ("CONFIG_FORTIFY_SOURCE", Y), ("CONFIG_RANDOMIZE_BASE", Y), ("CONFIG_RANDOMIZE_MEMORY", Y),
                  ("CONFIG_STRICT_KERNEL_RWX", Y), ("CONFIG_STRICT_MODULE_RWX", Y),
                  ("CONFIG_HARDENED_USERCOPY", Y), ("CONFIG_SLAB_FREELIST_RANDOM", Y),
                  ("CONFIG_SLAB_FREELIST_HARDENED", Y), ("CONFIG_VMAP_STACK", Y),
                  ("CONFIG_INIT_ON_ALLOC_DEFAULT_ON", Y), ("CONFIG_GCC_PLUGIN_STACKLEAK", Y), ("CONFIG_KSTACK_ERASE", Y),
                  ("CONFIG_LEGACY_VSYSCALL_NONE", Y), ("CONFIG_SCHED_STACK_END_CHECK", Y),
                  ("CONFIG_BUG_ON_DATA_CORRUPTION", Y)])
                .risk(Risk::Caution)
                .note("Stack-leak clearing is a GCC plugin on kernels before 6.17: without the gcc plugin headers (gcc-plugin-dev) kbuild drops that one option."))
            .opt(opt("paranoid", "Paranoid",
                "Also zeroes on free, randomises struct layout and locks the kernel down. Real performance cost.",
                &[("CONFIG_STACKPROTECTOR_STRONG", Y), ("CONFIG_FORTIFY_SOURCE", Y),
                  ("CONFIG_RANDOMIZE_BASE", Y), ("CONFIG_RANDOMIZE_MEMORY", Y),
                  ("CONFIG_STRICT_KERNEL_RWX", Y), ("CONFIG_STRICT_MODULE_RWX", Y),
                  ("CONFIG_HARDENED_USERCOPY", Y), ("CONFIG_SLAB_FREELIST_RANDOM", Y),
                  ("CONFIG_SLAB_FREELIST_HARDENED", Y), ("CONFIG_VMAP_STACK", Y),
                  ("CONFIG_INIT_ON_ALLOC_DEFAULT_ON", Y), ("CONFIG_INIT_ON_FREE_DEFAULT_ON", Y),
                  ("CONFIG_GCC_PLUGIN_STACKLEAK", Y), ("CONFIG_KSTACK_ERASE", Y), ("CONFIG_RANDSTRUCT_FULL", Y),
                  ("CONFIG_ZERO_CALL_USED_REGS", Y), ("CONFIG_SECURITY_LOCKDOWN_LSM", Y),
                  ("CONFIG_SECURITY_LOCKDOWN_LSM_EARLY", Y), ("CONFIG_LEGACY_VSYSCALL_NONE", Y),
                  ("CONFIG_BUG_ON_DATA_CORRUPTION", Y)])
                .risk(Risk::Caution)
                .note("Lockdown blocks unsigned module loading, which breaks DKMS modules such as the proprietary NVIDIA driver. Stack-leak clearing and struct randomisation need the gcc plugin headers (gcc-plugin-dev) on kernels before 6.17, and struct randomisation cannot be combined with BTF debug info.")),

        Tweak::new("attack_surface", "security", "Legacy kernel interfaces",
            "/dev/mem, /proc/kcore and friends are mostly useful to attackers. Restrict them, or remove them outright.")
            .opt(opt("restrict", "Restrict", "Keeps /dev/mem but limits it to non-RAM, non-claimed regions, and hides dmesg from unprivileged users.",
                &[("CONFIG_STRICT_DEVMEM", Y), ("CONFIG_IO_STRICT_DEVMEM", Y),
                  ("CONFIG_SECURITY_DMESG_RESTRICT", Y)]))
            .opt(opt("on", "Remove", "No /dev/mem, /dev/port or /proc/kcore at all, restricted dmesg, no legacy PTYs or binfmt_misc.",
                &[("CONFIG_DEVMEM", OFF), ("CONFIG_DEVPORT", OFF), ("CONFIG_PROC_KCORE", OFF),
                  ("CONFIG_SECURITY_DMESG_RESTRICT", Y), ("CONFIG_LEGACY_PTYS", OFF),
                  ("CONFIG_BINFMT_MISC", OFF)])
                .risk(Risk::Caution)
                .note("Some debuggers, firmware flashing tools and Wine's binfmt setup rely on these.")),

        Tweak::new("lsm", "security", "Security module (LSM)",
            "Which access-control framework your distro's policy uses. Getting this wrong leaves your policy unenforced.")
            .risk(Risk::Caution)
            .opt(opt("apparmor", "AppArmor (Ubuntu, Debian, openSUSE)", "",
                &[("CONFIG_SECURITY", Y), ("CONFIG_SECURITY_APPARMOR", Y),
                  ("CONFIG_DEFAULT_SECURITY_APPARMOR", Y),
                  ("CONFIG_LSM", Some("\"landlock,lockdown,yama,integrity,apparmor,bpf\""))]))
            .opt(opt("selinux", "SELinux (Fedora, RHEL)", "",
                &[("CONFIG_SECURITY", Y), ("CONFIG_SECURITY_SELINUX", Y),
                  ("CONFIG_SECURITY_SELINUX_BOOTPARAM", Y),
                  ("CONFIG_LSM", Some("\"landlock,lockdown,yama,integrity,selinux,bpf\""))]))
            .opt(opt("both", "Build both, decide at boot", "Choose with lsm= on the command line.",
                &[("CONFIG_SECURITY", Y), ("CONFIG_SECURITY_APPARMOR", Y), ("CONFIG_SECURITY_SELINUX", Y),
                  ("CONFIG_LSM", Some("\"landlock,lockdown,yama,integrity,apparmor,selinux,bpf\""))])),

        Tweak::new("module_sig", "security", "Module signing",
            "Sign modules at build time. Required if you want Secure Boot or lockdown to mean anything.")
            .opt(opt("on", "Sign modules (SHA-512)", "",
                &[("CONFIG_MODULE_SIG", Y), ("CONFIG_MODULE_SIG_ALL", Y), ("CONFIG_MODULE_SIG_SHA512", Y)]))
            .opt(opt("force", "Sign and refuse unsigned modules", "",
                &[("CONFIG_MODULE_SIG", Y), ("CONFIG_MODULE_SIG_ALL", Y), ("CONFIG_MODULE_SIG_FORCE", Y),
                  ("CONFIG_MODULE_SIG_SHA512", Y)])
                .risk(Risk::Risky)
                .note("Any module not signed by your key, DKMS ones included, will refuse to load."))
            .opt(opt("off", "Do not sign", "Fastest, and fine on a machine with Secure Boot off.",
                &[("CONFIG_MODULE_SIG", OFF)])),

        Tweak::new("mitigations", "security", "CPU vulnerability mitigations",
            "Spectre, Meltdown, Retbleed and friends. Turning these off buys real performance and real risk.")
            .risk(Risk::Risky)
            .opt(opt("on", "Keep mitigations compiled in", "",
                &[("CONFIG_CPU_MITIGATIONS", Y), ("CONFIG_SPECULATION_MITIGATIONS", Y),
                  ("CONFIG_MITIGATION_RETPOLINE", Y), ("CONFIG_RETPOLINE", Y)]))
            .opt(opt("off", "Compile them out",
                "Like booting with mitigations=off, but unchangeable at runtime.",
                &[("CONFIG_CPU_MITIGATIONS", OFF), ("CONFIG_SPECULATION_MITIGATIONS", OFF)])
                .risk(Risk::Risky)
                .note("Leaves you exposed to speculative-execution attacks. Never do this on a machine that runs other people's code, browses the web, or hosts VMs.")),

        // -------------------------------------------------------------- size
        Tweak::new("debug_info", "size", "Debug info",
            "The single biggest lever on build time and disk use. Turning it off can take a build from 90 minutes to 20 and save around 20 GiB.")
            .tags(&["fast-build"])
            .opt(opt("none", "None (fastest build)",
                "No DWARF, no BTF. No perf probes, no bpftrace CO-RE, no crash dumps.",
                &[("CONFIG_DEBUG_INFO_NONE", Y), ("CONFIG_DEBUG_INFO_DWARF_TOOLCHAIN_DEFAULT", OFF),
                  ("CONFIG_DEBUG_INFO_BTF", OFF), ("CONFIG_DEBUG_INFO_REDUCED", OFF),
                  ("CONFIG_DEBUG_INFO_SPLIT", OFF), ("CONFIG_GDB_SCRIPTS", OFF)])
                .note("eBPF tools that need BTF (bpftrace CO-RE, newer Cilium) stop working."))
            .opt(opt("btf", "BTF only (keeps eBPF tooling working)",
                "What modern distros ship. Needs pahole installed.",
                &[("CONFIG_DEBUG_INFO_NONE", OFF), ("CONFIG_DEBUG_INFO_DWARF_TOOLCHAIN_DEFAULT", Y),
                  ("CONFIG_BPF_SYSCALL", Y), ("CONFIG_DEBUG_INFO_BTF", Y), ("CONFIG_DEBUG_INFO_REDUCED", OFF)])
                .needs(&["pahole"]))
            .opt(opt("full", "Full DWARF + BTF", "Everything. Slowest build, biggest artifacts.",
                &[("CONFIG_DEBUG_INFO_NONE", OFF), ("CONFIG_DEBUG_INFO_DWARF_TOOLCHAIN_DEFAULT", Y),
                  ("CONFIG_BPF_SYSCALL", Y), ("CONFIG_DEBUG_INFO_BTF", Y), ("CONFIG_DEBUG_INFO_REDUCED", OFF),
                  ("CONFIG_GDB_SCRIPTS", Y)])
                .needs(&["pahole"])),

        Tweak::new("localmodconfig_hint", "size", "Only build modules you are using",
            "Scans your loaded modules and switches off everything else. Cuts a distro config of ~8000 modules down to a few hundred.")
            .toggle()
            .keep_text("Build whatever the base config builds.")
            .opt(opt("on", "Trim to loaded modules",
                "Runs 'make localmodconfig'. Plug in any USB or dock hardware you care about first, so its driver is loaded and kept.",
                &[])
                .risk(Risk::Caution)
                .note("Hardware you were not using when the scan ran may have no driver in the resulting kernel.")),

        Tweak::new("compiler_opt", "size", "Compiler optimisation",
            "Optimise the kernel for speed or for size.")
            .opt(opt("performance", "For performance (-O2)", "The normal choice.",
                &[("CONFIG_CC_OPTIMIZE_FOR_PERFORMANCE", Y), ("CONFIG_CC_OPTIMIZE_FOR_SIZE", OFF)]))
            .opt(opt("size", "For size (-Os)", "Smaller image, a bit slower. Nice on tiny machines.",
                &[("CONFIG_CC_OPTIMIZE_FOR_SIZE", Y), ("CONFIG_CC_OPTIMIZE_FOR_PERFORMANCE", OFF)])),

        Tweak::new("lto", "size", "Link-time optimisation",
            "Whole-program optimisation. A few percent faster kernel, much slower and more memory-hungry link step. Clang only.")
            .risk(Risk::Caution)
            .opt(opt("none", "Off", "",
                &[("CONFIG_LTO_NONE", Y), ("CONFIG_LTO_CLANG_THIN", OFF), ("CONFIG_LTO_CLANG_FULL", OFF)]))
            .opt(opt("thin", "ThinLTO (clang)", "Most of the benefit, far less RAM than full LTO.",
                &[("CONFIG_LTO_CLANG_THIN", Y), ("CONFIG_LTO_NONE", OFF)])
                .risk(Risk::Caution)
                .needs(&["clang", "ld.lld"])
                .note("Only clang can do this, so the whole kernel is built with LLVM=1 (clang + lld)."))
            .opt(opt("full", "Full LTO (clang)", "Needs a lot of RAM to link.",
                &[("CONFIG_LTO_CLANG_FULL", Y), ("CONFIG_LTO_NONE", OFF)])
                .risk(Risk::Risky)
                .needs(&["clang", "ld.lld"])
                .note("Can need 16 GiB or more to link, and is slow. The whole kernel is built with LLVM=1.")),

        Tweak::new("modules", "size", "Loadable module support",
            "Whether drivers can be loaded on demand. Almost every desktop and server wants this on; tiny VMs and appliances sometimes build everything in.")
            .toggle()
            .opt(opt("on", "Enable", "Drivers load when needed, and DKMS modules can be built.",
                &[("CONFIG_MODULES", Y), ("CONFIG_MODULE_UNLOAD", Y)]))
            .opt(opt("off", "Disable (monolithic kernel)",
                "Everything you enable is built into the image. Smaller attack surface, bigger kernel.",
                &[("CONFIG_MODULES", OFF)])
                .risk(Risk::Caution)
                .note("No DKMS, no out-of-tree drivers, and anything this tool would build as a module is built in instead.")),

        Tweak::new("module_compress", "size", "Module compression",
            "Compress modules on install. zstd is the modern pick: small and fast to load.")
            .opt(opt("zstd", "zstd", "",
                &[("CONFIG_MODULE_COMPRESS_ZSTD", Y), ("CONFIG_MODULE_COMPRESS_NONE", OFF),
                  ("CONFIG_MODULE_COMPRESS_XZ", OFF), ("CONFIG_MODULE_COMPRESS_GZIP", OFF)])
                .needs(&["zstd"]))
            .opt(opt("xz", "xz", "Smallest, slowest to load.",
                &[("CONFIG_MODULE_COMPRESS_XZ", Y), ("CONFIG_MODULE_COMPRESS_NONE", OFF),
                  ("CONFIG_MODULE_COMPRESS_ZSTD", OFF)]))
            .opt(opt("none", "Uncompressed", "Fastest boot, most disk.",
                &[("CONFIG_MODULE_COMPRESS_NONE", Y), ("CONFIG_MODULE_COMPRESS_ZSTD", OFF),
                  ("CONFIG_MODULE_COMPRESS_XZ", OFF)])),

        Tweak::new("kernel_compress", "size", "Kernel image compression",
            "Trade a little boot time against image size.")
            .opt(opt("zstd", "zstd", "Good ratio, fast to decompress.",
                &[("CONFIG_KERNEL_ZSTD", Y), ("CONFIG_KERNEL_GZIP", OFF), ("CONFIG_KERNEL_XZ", OFF),
                  ("CONFIG_KERNEL_LZ4", OFF)])
                .needs(&["zstd"]))
            .opt(opt("lz4", "lz4", "Fastest boot, biggest image.",
                &[("CONFIG_KERNEL_LZ4", Y), ("CONFIG_KERNEL_ZSTD", OFF), ("CONFIG_KERNEL_GZIP", OFF),
                  ("CONFIG_KERNEL_XZ", OFF)])
                .needs(&["lz4"]))
            .opt(opt("gzip", "gzip", "The safe old default.",
                &[("CONFIG_KERNEL_GZIP", Y), ("CONFIG_KERNEL_ZSTD", OFF), ("CONFIG_KERNEL_XZ", OFF),
                  ("CONFIG_KERNEL_LZ4", OFF)])),

        Tweak::new("trim_ksyms", "size", "Trim unused exported symbols",
            "Drops exported symbols nothing in-tree uses. A smaller kernel, but out-of-tree modules may fail to build against it.")
            .toggle()
            .opt(opt("on", "Trim them", "", &[("CONFIG_EXPERT", Y), ("CONFIG_TRIM_UNUSED_KSYMS", Y)])
                .risk(Risk::Caution)
                .note("Breaks DKMS modules such as the NVIDIA or VirtualBox drivers."))
            .opt(opt("off", "Keep all symbols", "", &[("CONFIG_TRIM_UNUSED_KSYMS", OFF)])),

        Tweak::new("kernel_debugging", "size", "Kernel debugging and tracing",
            "ftrace, kprobes and eBPF. Keep them unless you are chasing the last bit of performance: plenty of tooling needs them.")
            .opt(opt("full", "Tracing + eBPF (recommended)", "",
                &[("CONFIG_FTRACE", Y), ("CONFIG_FUNCTION_TRACER", Y), ("CONFIG_DYNAMIC_FTRACE", Y),
                  ("CONFIG_KPROBES", Y), ("CONFIG_BPF_SYSCALL", Y), ("CONFIG_BPF_JIT", Y),
                  ("CONFIG_BPF_EVENTS", Y), ("CONFIG_PERF_EVENTS", Y), ("CONFIG_KALLSYMS_ALL", Y)]))
            .opt(opt("minimal", "Strip tracing",
                "Smaller and marginally faster. perf and bpftrace lose capability.",
                &[("CONFIG_FTRACE", OFF), ("CONFIG_FUNCTION_TRACER", OFF), ("CONFIG_KPROBES", OFF),
                  ("CONFIG_DEBUG_KERNEL", OFF)])
                .risk(Risk::Caution)
                .note("systemd uses BPF for a few unit options, so BPF_SYSCALL is deliberately left alone here.")),

        // ---------------------------------------------------------- identity
        Tweak::new("localversion", "identity", "Version suffix",
            "Appended to the version string, so uname and /lib/modules tell your kernel apart from the distro's. Start it with a dash.")
            .text("CONFIG_LOCALVERSION", "-kertweaker"),

        Tweak::new("auto_localversion", "identity", "Append git revision automatically",
            "Adds '+' or the git SHA to the version. Usually you want this off so module paths stay predictable.")
            .toggle()
            .opt(opt("off", "Do not append", "", &[("CONFIG_LOCALVERSION_AUTO", OFF)]))
            .opt(opt("on", "Append", "", &[("CONFIG_LOCALVERSION_AUTO", Y)])),
    ]
}

static CATALOG: OnceLock<Vec<Tweak>> = OnceLock::new();

pub fn tweaks() -> &'static [Tweak] {
    CATALOG.get_or_init(build_catalog)
}

pub fn tweak(id: &str) -> Option<&'static Tweak> {
    tweaks().iter().find(|t| t.id == id)
}

// ------------------------------------------------------------------- profiles

pub struct Profile {
    pub id: &'static str,
    pub title: &'static str,
    pub desc: &'static str,
    pub recommended_for: &'static str,
    pub selections: &'static [(&'static str, &'static str)],
}

pub const PROFILES: &[Profile] = &[
    Profile {
        id: "desktop",
        title: "Responsive desktop",
        desc: "Full preemption at 1000 Hz, BFQ available, autogrouping on, BBR networking. Aims at a machine that stays responsive while busy.",
        recommended_for: "Daily-driver desktops and workstations",
        selections: &[
            ("preempt", "full"), ("hz", "1000"), ("tickless", "idle"), ("autogroup", "on"),
            ("cpufreq_gov", "schedutil"), ("thp", "madvise"), ("mglru", "on"), ("zswap", "on_zstd"),
            ("io_sched", "desktop"), ("filesystems", "common"), ("tcp_cc", "bbr"), ("qdisc", "on"),
            ("wireguard", "on"), ("sound", "on"), ("containers", "on"), ("hardening", "standard"),
            ("debug_info", "btf"), ("module_compress", "zstd"), ("kernel_compress", "zstd"),
            ("kernel_debugging", "full"), ("auto_localversion", "off"),
        ],
    },
    Profile {
        id: "gaming",
        title: "Gaming / low latency",
        desc: "Everything pushed at frame-time consistency: full preemption, 1000 Hz, performance governor, no debug info.",
        recommended_for: "Gaming rigs, audio production, anything latency-sensitive",
        selections: &[
            ("preempt", "full"), ("hz", "1000"), ("tickless", "idle"), ("autogroup", "on"),
            ("cpufreq_gov", "performance"), ("thp", "madvise"), ("mglru", "on"), ("zswap", "on_zstd"),
            ("io_sched", "desktop"), ("filesystems", "common"), ("tcp_cc", "bbr"), ("qdisc", "on"),
            ("sound", "on"), ("kvm", "both"), ("vfio", "on"), ("containers", "on"),
            ("hardening", "standard"), ("debug_info", "none"), ("module_compress", "zstd"),
            ("kernel_compress", "zstd"), ("compiler_opt", "performance"), ("auto_localversion", "off"),
        ],
    },
    Profile {
        id: "laptop",
        title: "Laptop / battery life",
        desc: "Tickless idle at 250 Hz with schedutil, hibernate kept, compressed swap on.",
        recommended_for: "Laptops and small form-factor machines",
        selections: &[
            ("preempt", "voluntary"), ("hz", "250"), ("tickless", "idle"), ("autogroup", "on"),
            ("cpufreq_gov", "schedutil"), ("power_mgmt", "both"), ("thp", "madvise"), ("mglru", "on"),
            ("zswap", "on_zstd"), ("zram", "on"), ("io_sched", "desktop"), ("filesystems", "common"),
            ("tcp_cc", "bbr"), ("qdisc", "on"), ("wireguard", "on"), ("sound", "on"),
            ("containers", "on"), ("hardening", "standard"), ("debug_info", "btf"),
            ("module_compress", "zstd"), ("kernel_compress", "zstd"), ("auto_localversion", "off"),
        ],
    },
    Profile {
        id: "server",
        title: "Server / throughput",
        desc: "Throughput over latency: no forced preemption, 100 Hz, no GPU drivers, no sound, containers and KVM on.",
        recommended_for: "Headless servers and hypervisors",
        selections: &[
            ("preempt", "none"), ("hz", "100"), ("tickless", "idle"), ("autogroup", "off"),
            ("cpufreq_gov", "performance"), ("power_mgmt", "none"), ("thp", "always"), ("mglru", "on"),
            ("ksm", "on"), ("io_sched", "server"), ("filesystems", "common"), ("nfs_smb", "on"),
            ("tcp_cc", "bbr"), ("qdisc", "on"), ("gpu", "headless"), ("sound", "off"),
            ("kvm", "both"), ("virtio_guest", "on"), ("containers", "on"), ("vfio", "on"),
            ("hardening", "strong"), ("module_sig", "on"), ("debug_info", "btf"),
            ("kernel_debugging", "full"), ("module_compress", "zstd"), ("kernel_compress", "zstd"),
            ("auto_localversion", "off"),
        ],
    },
    Profile {
        id: "vm_guest",
        title: "Virtual machine guest",
        desc: "virtio disk, network and display; no physical GPU or sound drivers. Boots fast and builds fast.",
        recommended_for: "QEMU/KVM, Proxmox and cloud instances",
        selections: &[
            ("preempt", "voluntary"), ("hz", "250"), ("tickless", "idle"), ("cpufreq_gov", "schedutil"),
            ("thp", "madvise"), ("mglru", "on"), ("io_sched", "server"), ("filesystems", "common"),
            ("tcp_cc", "bbr"), ("gpu", "virtual"), ("sound", "off"), ("virtio_guest", "on"),
            ("kvm", "off"), ("containers", "on"), ("hardening", "standard"), ("debug_info", "none"),
            ("module_compress", "zstd"), ("kernel_compress", "zstd"), ("localmodconfig_hint", "on"),
            ("auto_localversion", "off"),
        ],
    },
    Profile {
        id: "fast_build",
        title: "Fastest possible build",
        desc: "For your first attempt: trims to the modules you are actually using and drops debug info. Often 15-25 minutes instead of hours.",
        recommended_for: "A first build, or iterating on config changes",
        selections: &[
            ("localmodconfig_hint", "on"), ("debug_info", "none"), ("module_sig", "off"),
            ("compiler_opt", "performance"), ("module_compress", "zstd"), ("kernel_compress", "zstd"),
            ("auto_localversion", "off"),
        ],
    },
    Profile {
        id: "hardened",
        title: "Hardened",
        desc: "Paranoid memory hardening, lockdown, module signing and legacy interfaces removed. Read the warnings before you boot it.",
        recommended_for: "Machines where you accept some breakage for security",
        selections: &[
            ("preempt", "voluntary"), ("hz", "250"), ("tickless", "idle"), ("cpufreq_gov", "schedutil"),
            ("thp", "madvise"), ("mglru", "on"), ("io_sched", "desktop"), ("filesystems", "common"),
            ("tcp_cc", "bbr"), ("containers", "on"), ("hardening", "paranoid"),
            ("attack_surface", "on"), ("module_sig", "on"), ("mitigations", "on"),
            ("debug_info", "none"), ("module_compress", "zstd"), ("auto_localversion", "off"),
        ],
    },
];

pub fn profile(id: &str) -> Option<&'static Profile> {
    PROFILES.iter().find(|p| p.id == id)
}

// -------------------------------------------------------------------- resolve

/// Pseudo-tweak id used when the user's own pasted fragment wins.
pub const EXTRA: &str = "extra_options";

/// Symbols that were renamed between kernel versions. Options request every
/// spelling; verification treats a group as satisfied when one of them took.
pub const ALIASES: &[&[&str]] = &[
    // 6.9 (and 6.8 stable) folded the x86 name into the generic one.
    &["CONFIG_CPU_MITIGATIONS", "CONFIG_SPECULATION_MITIGATIONS"],
    &["CONFIG_MITIGATION_RETPOLINE", "CONFIG_RETPOLINE"],
    // 6.17 turned the GCC-plugin stackleak into compiler-agnostic KSTACK_ERASE.
    &["CONFIG_KSTACK_ERASE", "CONFIG_GCC_PLUGIN_STACKLEAK"],
];

/// A host tool some selected option depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Need {
    pub tweak: String,
    pub label: String,
    pub tool: &'static str,
}

#[derive(Debug, Clone)]
pub struct Conflict {
    pub symbol: String,
    pub previous_tweak: String,
    pub previous_value: String,
    pub winning_tweak: String,
    pub winning_value: String,
}

#[derive(Debug, Clone)]
pub struct Note {
    pub tweak: String,
    pub level: String,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct Resolved {
    /// Ordered `(symbol, value)` fragment, catalog order then symbol order.
    pub fragment: Vec<(String, Value)>,
    pub conflicts: Vec<Conflict>,
    pub notes: Vec<Note>,
    pub unknown: Vec<String>,
    pub localmodconfig: bool,
    pub risk: Risk,
    /// Which tweak (or [`EXTRA`]) is responsible for each symbol.
    pub origin: BTreeMap<String, String>,
    /// Host tools the selected options need.
    pub needs: Vec<Need>,
}

impl Resolved {
    /// LTO is clang-only, so selecting it means the build must use LLVM=1.
    pub fn needs_clang(&self) -> bool {
        self.needs.iter().any(|n| n.tool == "clang")
    }

    /// Notes for needed tools that `has_tool` says are missing.
    pub fn missing_tool_notes(&self, has_tool: impl Fn(&str) -> bool) -> Vec<Note> {
        self.needs
            .iter()
            .filter(|n| !has_tool(n.tool))
            .map(|n| Note {
                tweak: n.tweak.clone(),
                level: "caution".into(),
                text: format!(
                    "{} needs '{}', which is not installed. Without it kbuild will drop the option.",
                    n.label, n.tool
                ),
            })
            .collect()
    }
}

impl Resolved {
    pub fn to_json(&self) -> Json {
        Json::obj(vec![
            ("symbol_count", Json::u(self.fragment.len())),
            ("risk", Json::s(self.risk.as_str())),
            ("localmodconfig", Json::b(self.localmodconfig)),
            ("needs_clang", Json::b(self.needs_clang())),
            ("fragment_text", Json::s(fragment_text(&self.fragment))),
            (
                "fragment",
                Json::Obj(
                    self.fragment
                        .iter()
                        .map(|(k, v)| {
                            (
                                k.clone(),
                                match v {
                                    Some(x) => Json::s(x.clone()),
                                    None => Json::Null,
                                },
                            )
                        })
                        .collect(),
                ),
            ),
            (
                "conflicts",
                Json::arr(
                    self.conflicts
                        .iter()
                        .map(|c| {
                            Json::obj(vec![
                                ("symbol", Json::s(c.symbol.clone())),
                                ("previous_tweak", Json::s(c.previous_tweak.clone())),
                                ("previous_value", Json::s(c.previous_value.clone())),
                                ("winning_tweak", Json::s(c.winning_tweak.clone())),
                                ("winning_value", Json::s(c.winning_value.clone())),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "notes",
                Json::arr(
                    self.notes
                        .iter()
                        .map(|n| {
                            Json::obj(vec![
                                ("tweak", Json::s(n.tweak.clone())),
                                ("level", Json::s(n.level.clone())),
                                ("text", Json::s(n.text.clone())),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("unknown", Json::strs(self.unknown.clone())),
        ])
    }
}

/// Turn `{tweak_id: option_id}` into a config fragment plus metadata.
///
/// Catalog order decides precedence, and every overlap is reported so the GUI
/// can say "X overrode Y on CONFIG_Z" instead of silently dropping one.
pub fn resolve(selections: &BTreeMap<String, String>) -> Resolved {
    resolve_with_extra(selections, "")
}

/// As [`resolve`], plus a hand-written fragment from the "extra options" box.
///
/// The user's own lines are the most specific thing they can say, so they win -
/// but every value they override is reported as a conflict, because silently
/// discarding a tweak the user also clicked on is exactly the surprise this
/// tool exists to avoid.
pub fn resolve_with_extra(selections: &BTreeMap<String, String>, extra: &str) -> Resolved {
    let mut out = Resolved::default();
    let mut origin: BTreeMap<String, &str> = BTreeMap::new();

    for t in tweaks() {
        let Some(chosen) = selections.get(t.id) else {
            continue;
        };
        if chosen.is_empty() || chosen == "keep" {
            continue;
        }

        // Free-form kinds write straight to their symbol.
        if matches!(t.kind, Kind::Text | Kind::Number) {
            let value = match t.kind {
                Kind::Text => {
                    let trimmed = chosen.trim();
                    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
                        trimmed.to_string()
                    } else {
                        format!("\"{}\"", trimmed.replace('\\', "\\\\").replace('"', "\\\""))
                    }
                }
                _ => {
                    let trimmed = chosen.trim();
                    if trimmed.is_empty()
                        || !trimmed
                            .strip_prefix('-')
                            .unwrap_or(trimmed)
                            .bytes()
                            .all(|b| b.is_ascii_digit())
                    {
                        out.notes.push(Note {
                            tweak: t.id.to_string(),
                            level: "error".into(),
                            text: format!("{}: {:?} is not a number.", t.title, chosen),
                        });
                        continue;
                    }
                    trimmed.to_string()
                }
            };
            upsert(&mut out.fragment, t.symbol, Some(value));
            origin.insert(t.symbol.to_string(), t.id);
            continue;
        }

        let Some(o) = t.option(chosen) else {
            out.unknown.push(format!("{}={}", t.id, chosen));
            continue;
        };

        // localmodconfig is a kbuild step, not a symbol.
        if t.id == "localmodconfig_hint" && o.id == "on" {
            out.localmodconfig = true;
        }
        out.risk = out.risk.max(o.risk);
        for tool in &o.needs {
            out.needs.push(Need {
                tweak: t.id.to_string(),
                label: format!("{} / {}", t.title, o.label),
                tool,
            });
        }
        if !o.note.is_empty() {
            out.notes.push(Note {
                tweak: t.id.to_string(),
                level: o.risk.as_str().to_string(),
                text: format!("{} / {}: {}", t.title, o.label, o.note),
            });
        }

        for (symbol, value) in &o.sets {
            let value = value.map(|s| s.to_string());
            if let Some(existing) = out.fragment.iter().find(|(s, _)| s == symbol) {
                if existing.1 != value {
                    out.conflicts.push(Conflict {
                        symbol: symbol.to_string(),
                        previous_tweak: origin.get(*symbol).copied().unwrap_or("?").to_string(),
                        previous_value: show(&existing.1),
                        winning_tweak: t.id.to_string(),
                        winning_value: show(&value),
                    });
                }
            }
            upsert(&mut out.fragment, symbol, value);
            origin.insert(symbol.to_string(), t.id);
        }
    }

    for (symbol, value) in crate::kconfig::parse_fragment(extra) {
        if let Some(existing) = out.fragment.iter().find(|(s, _)| *s == symbol) {
            if existing.1 != value {
                out.conflicts.push(Conflict {
                    symbol: symbol.clone(),
                    previous_tweak: origin.get(&symbol).copied().unwrap_or("?").to_string(),
                    previous_value: show(&existing.1),
                    winning_tweak: EXTRA.to_string(),
                    winning_value: show(&value),
                });
            }
        }
        upsert(&mut out.fragment, &symbol, value);
        origin.insert(symbol, EXTRA);
    }

    for id in selections.keys() {
        if tweak(id).is_none() {
            out.unknown.push(id.clone());
        }
    }
    out.unknown.sort();
    out.unknown.dedup();
    out.origin = origin
        .into_iter()
        .map(|(k, v)| (k, v.to_string()))
        .collect();
    out
}

fn upsert(fragment: &mut Vec<(String, Value)>, symbol: &str, value: Value) {
    match fragment.iter_mut().find(|(s, _)| s == symbol) {
        Some(slot) => slot.1 = value,
        None => fragment.push((symbol.to_string(), value)),
    }
}

fn show(v: &Value) -> String {
    match v {
        Some(x) => x.clone(),
        None => "n".to_string(),
    }
}

/// Render a fragment as `.config` syntax, sorted for readability.
pub fn fragment_text(fragment: &[(String, Value)]) -> String {
    let mut sorted: Vec<&(String, Value)> = fragment.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = String::new();
    for (sym, val) in sorted {
        out.push_str(&crate::kconfig::format_line(sym, val));
        out.push('\n');
    }
    out
}

/// Which profile best fits the machine we are running on.
pub fn profile_hint(
    hypervisor: bool,
    memory_gib: f64,
    threads: usize,
    has_gui: bool,
) -> &'static str {
    if hypervisor {
        "vm_guest"
    } else if memory_gib > 0.0 && memory_gib < 8.0 && threads <= 4 {
        "fast_build"
    } else if !has_gui {
        "server"
    } else {
        "desktop"
    }
}

pub fn to_json() -> Json {
    Json::obj(vec![
        (
            "groups",
            Json::arr(
                GROUPS
                    .iter()
                    .map(|g| {
                        Json::obj(vec![
                            ("id", Json::s(g.id)),
                            ("title", Json::s(g.title)),
                            ("blurb", Json::s(g.blurb)),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "tweaks",
            Json::arr(tweaks().iter().map(Tweak::to_json).collect()),
        ),
        (
            "profiles",
            Json::arr(
                PROFILES
                    .iter()
                    .map(|p| {
                        Json::obj(vec![
                            ("id", Json::s(p.id)),
                            ("title", Json::s(p.title)),
                            ("description", Json::s(p.desc)),
                            ("recommended_for", Json::s(p.recommended_for)),
                            (
                                "selections",
                                Json::Obj(
                                    p.selections
                                        .iter()
                                        .map(|(k, v)| (k.to_string(), Json::s(*v)))
                                        .collect(),
                                ),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

/// Selections as a map, for a named profile.
pub fn profile_selections(id: &str) -> Option<BTreeMap<String, String>> {
    profile(id).map(|p| {
        p.selections
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn every_tweak_is_wellformed() {
        let mut ids = Vec::new();
        for t in tweaks() {
            assert!(!t.title.is_empty() && !t.desc.is_empty(), "{}", t.id);
            assert!(
                GROUPS.iter().any(|g| g.id == t.group),
                "{} has unknown group {}",
                t.id,
                t.group
            );
            assert!(!ids.contains(&t.id), "duplicate tweak id {}", t.id);
            ids.push(t.id);
            match t.kind {
                Kind::Text | Kind::Number => {
                    assert!(t.symbol.starts_with("CONFIG_"), "{} needs a symbol", t.id);
                    assert_eq!(t.opts.len(), 1, "{} should only have 'keep'", t.id);
                }
                _ => {
                    assert!(t.opts.len() >= 2, "{} needs real options", t.id);
                    assert_eq!(t.opts[0].id, "keep", "{} must start with keep", t.id);
                    let mut seen = Vec::new();
                    for o in &t.opts {
                        assert!(!seen.contains(&o.id), "duplicate option {}/{}", t.id, o.id);
                        seen.push(o.id);
                        for (sym, _) in &o.sets {
                            assert!(sym.starts_with("CONFIG_"), "{}/{}: {}", t.id, o.id, sym);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn every_profile_reference_resolves() {
        for p in PROFILES {
            assert!(!p.selections.is_empty(), "{} is empty", p.id);
            for (tid, oid) in p.selections {
                let t = tweak(tid).unwrap_or_else(|| panic!("{}: no tweak {}", p.id, tid));
                if matches!(t.kind, Kind::Text | Kind::Number) {
                    continue;
                }
                assert!(
                    t.option(oid).is_some(),
                    "profile {}: {} has no option {}",
                    p.id,
                    tid,
                    oid
                );
            }
            let r = resolve(&profile_selections(p.id).unwrap());
            assert!(r.unknown.is_empty(), "{}: {:?}", p.id, r.unknown);
            assert!(r.conflicts.is_empty(), "{}: {:?}", p.id, r.conflicts);
            assert!(!r.fragment.is_empty(), "{} resolved to nothing", p.id);
        }
    }

    #[test]
    fn resolve_builds_expected_fragment() {
        let r = resolve(&sel(&[("preempt", "full"), ("hz", "1000")]));
        let text = fragment_text(&r.fragment);
        assert!(text.contains("CONFIG_PREEMPT=y\n"));
        assert!(text.contains("# CONFIG_PREEMPT_NONE is not set\n"));
        assert!(text.contains("CONFIG_HZ=1000\n"));
        assert_eq!(r.risk, Risk::Safe);
    }

    #[test]
    fn keep_and_empty_are_noops() {
        let r = resolve(&sel(&[("preempt", "keep"), ("hz", "")]));
        assert!(r.fragment.is_empty());
    }

    #[test]
    fn catalog_options_never_disagree_with_each_other() {
        // Two tweaks setting one symbol to different values would make the
        // result depend on catalog order, which no user can see. Keep the
        // catalog free of that.
        let ts = tweaks();
        let mut clashes = Vec::new();
        for (i, a) in ts.iter().enumerate() {
            for b in ts.iter().skip(i + 1) {
                for oa in &a.opts {
                    for ob in &b.opts {
                        for (sa, va) in &oa.sets {
                            for (sb, vb) in &ob.sets {
                                if sa == sb && va != vb {
                                    clashes.push(format!(
                                        "{}/{} vs {}/{} on {}",
                                        a.id, oa.id, b.id, ob.id, sa
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(clashes.is_empty(), "overlapping options: {clashes:#?}");
    }

    #[test]
    fn extra_fragment_wins_and_the_override_is_reported() {
        let selections = sel(&[("preempt", "full"), ("debug_info", "none")]);
        let r = resolve_with_extra(
            &selections,
            "CONFIG_PREEMPT=n\nCONFIG_DEBUG_INFO_BTF=y\nCONFIG_MY_OWN=y\n",
        );
        let frag: BTreeMap<_, _> = r.fragment.iter().cloned().collect();
        // The user's lines land in the fragment...
        assert_eq!(frag["CONFIG_PREEMPT"], None);
        assert_eq!(frag["CONFIG_DEBUG_INFO_BTF"], Some("y".to_string()));
        assert_eq!(frag["CONFIG_MY_OWN"], Some("y".to_string()));
        // ...and each overridden tweak value is surfaced, not swallowed.
        assert_eq!(r.conflicts.len(), 2, "{:?}", r.conflicts);
        let preempt = r
            .conflicts
            .iter()
            .find(|c| c.symbol == "CONFIG_PREEMPT")
            .unwrap();
        assert_eq!(preempt.previous_tweak, "preempt");
        assert_eq!(preempt.previous_value, "y");
        assert_eq!(preempt.winning_tweak, EXTRA);
        assert_eq!(preempt.winning_value, "n");
    }

    #[test]
    fn extra_fragment_agreeing_is_not_a_conflict() {
        let r = resolve_with_extra(&sel(&[("preempt", "full")]), "CONFIG_PREEMPT=y\n");
        assert!(r.conflicts.is_empty());
    }

    #[test]
    fn risk_escalates_to_the_worst_option() {
        let r = resolve(&sel(&[("preempt", "full"), ("mitigations", "off")]));
        assert_eq!(r.risk, Risk::Risky);
        assert!(r.notes.iter().any(|n| n.text.contains("speculative")));
    }

    #[test]
    fn text_values_are_quoted_once() {
        let r = resolve(&sel(&[("localversion", "-mine")]));
        assert_eq!(r.fragment[0].1, Some("\"-mine\"".to_string()));
        let r2 = resolve(&sel(&[("localversion", "\"-quoted\"")]));
        assert_eq!(r2.fragment[0].1, Some("\"-quoted\"".to_string()));
        let r3 = resolve(&sel(&[("localversion", "say \"hi\"")]));
        assert_eq!(r3.fragment[0].1, Some("\"say \\\"hi\\\"\"".to_string()));
    }

    #[test]
    fn numbers_are_validated() {
        let ok = resolve(&sel(&[("nr_cpus", "16")]));
        assert_eq!(
            ok.fragment[0],
            ("CONFIG_NR_CPUS".to_string(), Some("16".to_string()))
        );
        let bad = resolve(&sel(&[("nr_cpus", "lots")]));
        assert!(bad.fragment.is_empty());
        assert_eq!(bad.notes.len(), 1);
        assert_eq!(bad.notes[0].level, "error");
    }

    #[test]
    fn unknown_selections_are_surfaced() {
        let r = resolve(&sel(&[("nonsense", "x"), ("preempt", "sideways")]));
        assert_eq!(r.unknown, vec!["nonsense", "preempt=sideways"]);
    }

    #[test]
    fn tool_requirements_are_tracked_and_reported_when_missing() {
        let r = resolve(&sel(&[
            ("debug_info", "btf"),
            ("lto", "thin"),
            ("preempt", "full"),
        ]));
        let tools: Vec<&str> = r.needs.iter().map(|n| n.tool).collect();
        assert_eq!(tools, vec!["pahole", "clang", "ld.lld"]);
        assert!(r.needs_clang());

        // Only the tools the host lacks produce a note.
        let notes = r.missing_tool_notes(|tool| tool != "pahole");
        assert_eq!(notes.len(), 1);
        assert!(notes[0].text.contains("'pahole'"), "{}", notes[0].text);
        assert!(r.missing_tool_notes(|_| true).is_empty());

        assert!(!resolve(&sel(&[("preempt", "full")])).needs_clang());
    }

    #[test]
    fn alias_groups_only_name_symbols_the_catalog_uses() {
        let all: Vec<&str> = tweaks()
            .iter()
            .flat_map(|t| t.opts.iter())
            .flat_map(|o| o.sets.iter().map(|(s, _)| *s))
            .collect();
        for group in ALIASES {
            assert!(group.len() >= 2);
            for sym in *group {
                assert!(all.contains(sym), "alias {sym} is not set by any option");
            }
        }
    }

    #[test]
    fn localmodconfig_is_a_step_not_a_symbol() {
        let r = resolve(&sel(&[("localmodconfig_hint", "on")]));
        assert!(r.localmodconfig);
        assert!(r.fragment.is_empty());
    }

    #[test]
    fn profile_hint_picks_sensibly() {
        assert_eq!(profile_hint(true, 16.0, 8, true), "vm_guest");
        assert_eq!(profile_hint(false, 4.0, 4, true), "fast_build");
        assert_eq!(profile_hint(false, 32.0, 16, false), "server");
        assert_eq!(profile_hint(false, 32.0, 16, true), "desktop");
    }

    #[test]
    fn catalog_json_is_parseable_and_complete() {
        let j = to_json();
        let text = j.to_string();
        let back = crate::json::parse(&text).unwrap();
        assert_eq!(
            back.get("tweaks").unwrap().as_array().unwrap().len(),
            tweaks().len()
        );
        assert_eq!(
            back.get("profiles").unwrap().as_array().unwrap().len(),
            PROFILES.len()
        );
    }
}
