//! Running the build: stages, streamed logs, cancellation and verification.
//!
//! One [`Job`] owns a sequence of stages. Everything a caller needs to render
//! progress lives behind a single mutex, and the SSE endpoint just reads new
//! log lines from a cursor, so a slow client can never stall the build.

use crate::catalog;
use crate::json::Json;
use crate::kconfig::{self, Config};
use crate::sources;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Keep this many log lines in memory; the full log always goes to disk.
const LOG_MEMORY_LINES: usize = 4000;

pub fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// ------------------------------------------------------------------- request

#[derive(Debug, Clone)]
pub enum Source {
    /// A kernel tree already on disk.
    Local(PathBuf),
    /// Download and extract this kernel.org version.
    Download(String),
}

#[derive(Debug, Clone)]
pub enum Base {
    /// The config of the running kernel (`/proc/config.gz` or `/boot/config-*`).
    Running,
    /// A `.config` file on disk.
    File(PathBuf),
    /// `make defconfig`.
    Defconfig,
    /// Whatever `.config` the tree already has.
    Tree,
    /// A pasted `.config`.
    Text(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// bzImage + modules, installed with `make modules_install install`.
    Image,
    /// `make bindeb-pkg` - .deb files you install with dpkg.
    Deb,
    /// `make binrpm-pkg`.
    Rpm,
    /// `make tarbz2-pkg`-style tarball.
    Tar,
}

impl Output {
    pub fn parse(s: &str) -> Output {
        match s {
            "deb" => Output::Deb,
            "rpm" => Output::Rpm,
            "tar" => Output::Tar,
            _ => Output::Image,
        }
    }
    pub fn make_target(self) -> &'static str {
        match self {
            Output::Image => "all",
            Output::Deb => "bindeb-pkg",
            Output::Rpm => "binrpm-pkg",
            Output::Tar => "tarxz-pkg",
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Output::Image => "image",
            Output::Deb => "deb",
            Output::Rpm => "rpm",
            Output::Tar => "tar",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Request {
    pub source: Source,
    pub base: Base,
    pub selections: BTreeMap<String, String>,
    pub extra: String,
    pub jobs: usize,
    pub output: Output,
    pub llvm: bool,
    pub clean: bool,
    /// Install the built kernel. Only honoured when running as root.
    pub install: bool,
    /// Stop after the config is resolved and verified; build nothing.
    pub config_only: bool,
    /// Build even though a required tool looks missing. For the rare case
    /// where a toolchain is installed under a name `which` does not find.
    pub skip_tool_check: bool,
    pub workdir: PathBuf,
    /// The `make` to invoke. Overridable so tests can drive a stub kbuild.
    pub make: String,
}

impl Request {
    pub fn make_cmd() -> String {
        std::env::var("KERTWEAKER_MAKE").unwrap_or_else(|_| "make".to_string())
    }
}

// --------------------------------------------------------------------- stages

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageState {
    Pending,
    Running,
    Done,
    Failed,
    Skipped,
}

impl StageState {
    fn as_str(self) -> &'static str {
        match self {
            StageState::Pending => "pending",
            StageState::Running => "running",
            StageState::Done => "done",
            StageState::Failed => "failed",
            StageState::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Stage {
    pub id: &'static str,
    pub title: &'static str,
    pub state: StageState,
    pub detail: String,
    pub started: Option<f64>,
    pub ended: Option<f64>,
}

impl Stage {
    fn new(id: &'static str, title: &'static str) -> Stage {
        Stage {
            id,
            title,
            state: StageState::Pending,
            detail: String::new(),
            started: None,
            ended: None,
        }
    }
    fn to_json(&self) -> Json {
        Json::obj(vec![
            ("id", Json::s(self.id)),
            ("title", Json::s(self.title)),
            ("state", Json::s(self.state.as_str())),
            ("detail", Json::s(self.detail.clone())),
            (
                "seconds",
                match (self.started, self.ended) {
                    (Some(s), Some(e)) => Json::n(e - s),
                    (Some(s), None) => Json::n(now_secs() - s),
                    _ => Json::Null,
                },
            ),
        ])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    Done,
    Failed,
    Cancelled,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Cancelled => "cancelled",
        }
    }
    pub fn finished(self) -> bool {
        !matches!(self, Status::Running)
    }
}

/// Everything about a job that changes while it runs.
pub struct JobState {
    pub status: Status,
    pub stages: Vec<Stage>,
    pub log: Vec<String>,
    /// Lines dropped from the front of `log`, so cursors stay meaningful.
    pub dropped: usize,
    pub started: f64,
    pub ended: Option<f64>,
    pub error: Option<String>,
    pub artifacts: Vec<(String, u64)>,
    pub next_steps: Vec<String>,
    pub unapplied: Vec<kconfig::Unapplied>,
    /// Requested as modules, built in instead.
    pub builtin_instead: Vec<String>,
    /// Plain-language observations about the resolved config.
    pub advice: Vec<String>,
    pub diff_count: usize,
    pub config_path: Option<PathBuf>,
    pub tree: Option<PathBuf>,
    pub objects: u64,
    pub objects_expected: Option<u64>,
    pub phase: String,
}

impl JobState {
    fn push_log(&mut self, line: &str) {
        self.log.push(line.to_string());
        if self.log.len() > LOG_MEMORY_LINES {
            let overflow = self.log.len() - LOG_MEMORY_LINES;
            self.log.drain(..overflow);
            self.dropped += overflow;
        }
    }
    fn stage_mut(&mut self, id: &str) -> Option<&mut Stage> {
        self.stages.iter_mut().find(|s| s.id == id)
    }
}

pub struct Job {
    pub id: String,
    pub request: Request,
    pub state: Mutex<JobState>,
    pub cancel: Arc<AtomicBool>,
    /// Whether make runs with LLVM=1: requested, or forced by an LTO option.
    pub llvm: AtomicBool,
    pub child: Mutex<Option<Child>>,
    pub log_path: PathBuf,
    /// Monotonically bumped on every change, so clients can poll cheaply.
    pub revision: AtomicU64,
}

impl Job {
    fn log(&self, line: &str) {
        if let Ok(mut st) = self.state.lock() {
            st.push_log(line);
        }
        let _ = append_line(&self.log_path, line);
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    fn begin(&self, id: &str) {
        if let Ok(mut st) = self.state.lock() {
            if let Some(s) = st.stage_mut(id) {
                s.state = StageState::Running;
                s.started = Some(now_secs());
            }
        }
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    fn finish(&self, id: &str, state: StageState, detail: impl Into<String>) {
        if let Ok(mut st) = self.state.lock() {
            if let Some(s) = st.stage_mut(id) {
                s.state = state;
                s.detail = detail.into();
                s.ended = Some(now_secs());
                if s.started.is_none() {
                    s.started = s.ended;
                }
            }
        }
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Ask the job to stop, killing the running `make` and everything it spawned.
    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        let pid = self
            .child
            .lock()
            .ok()
            .and_then(|c| c.as_ref().map(|c| c.id()));
        if let Some(pid) = pid {
            // The child leads its own process group, so a negative pid reaches
            // every compiler it started. Ask nicely, then insist.
            kill_group(pid, "TERM");
            let id = self.id.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_secs(3));
                kill_group(pid, "KILL");
                let _ = &id;
            });
        }
        if let Ok(mut child) = self.child.lock() {
            if let Some(c) = child.as_mut() {
                let _ = c.kill();
            }
        }
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    pub fn to_json(&self, log_from: Option<usize>) -> Json {
        let st = self.state.lock().expect("job state poisoned");
        let mut pairs = vec![
            ("id", Json::s(self.id.clone())),
            ("status", Json::s(st.status.as_str())),
            ("phase", Json::s(st.phase.clone())),
            ("started", Json::n(st.started)),
            (
                "elapsed",
                Json::n(st.ended.unwrap_or_else(now_secs) - st.started),
            ),
            ("error", Json::opt_s(st.error.clone())),
            ("output", Json::s(self.request.output.as_str())),
            ("jobs", Json::u(self.request.jobs)),
            ("llvm", Json::b(self.llvm.load(Ordering::Relaxed))),
            ("objects", Json::n(st.objects as f64)),
            (
                "objects_expected",
                match st.objects_expected {
                    Some(n) => Json::n(n as f64),
                    None => Json::Null,
                },
            ),
            ("diff_count", Json::u(st.diff_count)),
            (
                "tree",
                Json::opt_s(st.tree.as_ref().map(|p| p.to_string_lossy().into_owned())),
            ),
            (
                "config_path",
                Json::opt_s(
                    st.config_path
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned()),
                ),
            ),
            ("log_path", Json::s(self.log_path.to_string_lossy())),
            ("stages", Json::arr(st.stages.iter().map(Stage::to_json).collect())),
            (
                "artifacts",
                Json::arr(
                    st.artifacts
                        .iter()
                        .map(|(p, size)| {
                            Json::obj(vec![
                                ("path", Json::s(p.clone())),
                                ("bytes", Json::n(*size as f64)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("next_steps", Json::strs(st.next_steps.clone())),
            (
                "unapplied",
                Json::arr(
                    st.unapplied
                        .iter()
                        .map(|u| {
                            Json::obj(vec![
                                ("symbol", Json::s(u.symbol.clone())),
                                ("wanted", Json::s(kconfig::format_line(&u.symbol, &u.wanted))),
                                (
                                    "actual",
                                    Json::s(if u.present {
                                        kconfig::format_line(&u.symbol, &u.actual)
                                    } else {
                                        "unavailable: not in this kernel version, or a dependency is off"
                                            .to_string()
                                    }),
                                ),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("builtin_instead", Json::strs(st.builtin_instead.clone())),
            ("advice", Json::strs(st.advice.clone())),
            ("log_dropped", Json::u(st.dropped)),
            ("log_total", Json::u(st.dropped + st.log.len())),
        ];
        if let Some(from) = log_from {
            let start = from.saturating_sub(st.dropped).min(st.log.len());
            pairs.push(("log", Json::strs(st.log[start..].to_vec())));
        }
        Json::Obj(
            pairs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    }
}

/// Signal a whole process group. Shelling out to `kill` avoids a libc
/// dependency for the one syscall this program needs.
fn kill_group(pid: u32, signal: &str) {
    let _ = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg("--")
        .arg(format!("-{pid}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")
}

fn plan(req: &Request) -> Vec<Stage> {
    let mut stages = vec![
        Stage::new("source", "Kernel source"),
        Stage::new("deps", "Build tools"),
        Stage::new("base", "Starting config"),
    ];
    stages.push(Stage::new("trim", "Trim to loaded modules"));
    stages.push(Stage::new("tweaks", "Apply your tweaks"));
    stages.push(Stage::new("resolve", "Resolve dependencies (olddefconfig)"));
    stages.push(Stage::new("verify", "Verify the result"));
    if !req.config_only {
        if req.clean {
            stages.push(Stage::new("clean", "Clean tree"));
        }
        stages.push(Stage::new("build", "Compile"));
        stages.push(Stage::new("collect", "Collect artifacts"));
        stages.push(Stage::new("install", "Install"));
    }
    stages
}

pub fn new_job(id: String, req: Request) -> Arc<Job> {
    let log_path = req.workdir.join("logs").join(format!("{id}.log"));
    let _ = fs::create_dir_all(log_path.parent().unwrap_or(&req.workdir));
    Arc::new(Job {
        log_path,
        state: Mutex::new(JobState {
            status: Status::Running,
            stages: plan(&req),
            log: Vec::new(),
            dropped: 0,
            started: now_secs(),
            ended: None,
            error: None,
            artifacts: Vec::new(),
            next_steps: Vec::new(),
            unapplied: Vec::new(),
            builtin_instead: Vec::new(),
            advice: Vec::new(),
            diff_count: 0,
            config_path: None,
            tree: None,
            objects: 0,
            objects_expected: load_expected_objects(&req.workdir),
            phase: "starting".into(),
        }),
        cancel: Arc::new(AtomicBool::new(false)),
        llvm: AtomicBool::new(req.llvm),
        child: Mutex::new(None),
        revision: AtomicU64::new(0),
        id,
        request: req,
    })
}

/// Run a job to completion. Intended to be called on its own thread.
pub fn run(job: Arc<Job>) {
    let result = run_inner(&job);
    let mut st = job.state.lock().expect("job state poisoned");
    st.ended = Some(now_secs());
    match result {
        Ok(()) => {
            st.status = if job.cancel.load(Ordering::Relaxed) {
                Status::Cancelled
            } else {
                Status::Done
            };
            st.phase = "finished".into();
        }
        Err(e) => {
            if job.cancel.load(Ordering::Relaxed) {
                st.status = Status::Cancelled;
                st.phase = "cancelled".into();
            } else {
                st.status = Status::Failed;
                st.error = Some(e);
                st.phase = "failed".into();
            }
        }
    }
    // The stage that was still running is the one that failed; everything
    // after it never got its turn. Leaving either as "running" would spin the
    // UI forever.
    let failed = st.status == Status::Failed;
    let detail = st.error.clone().unwrap_or_default();
    let mut blamed = false;
    for s in st.stages.iter_mut() {
        match s.state {
            StageState::Running if failed && !blamed => {
                s.state = StageState::Failed;
                if s.detail.is_empty() {
                    s.detail = detail.clone();
                }
                s.ended = Some(now_secs());
                blamed = true;
            }
            StageState::Pending | StageState::Running => {
                s.state = StageState::Skipped;
                if s.ended.is_none() && s.started.is_some() {
                    s.ended = Some(now_secs());
                }
            }
            _ => {}
        }
    }
    drop(st);
    job.revision.fetch_add(1, Ordering::Relaxed);
}

fn run_inner(job: &Arc<Job>) -> Result<(), String> {
    let req = job.request.clone();
    fs::create_dir_all(&req.workdir).map_err(|e| format!("cannot create workdir: {e}"))?;

    // ---------------------------------------------------------- 1. source
    job.begin("source");
    let tree = match &req.source {
        Source::Local(p) => {
            if !sources::is_kernel_tree(p) {
                job.finish("source", StageState::Failed, "not a kernel tree");
                return Err(format!("{} is not a Linux kernel source tree", p.display()));
            }
            p.clone()
        }
        Source::Download(version) => {
            if !sources::valid_version(version) {
                job.finish("source", StageState::Failed, "bad version");
                return Err(format!("{version:?} is not a valid kernel version"));
            }
            let url = sources::tarball_url(version);
            let tarball = {
                let j = job.clone();
                sources::download(&url, &req.workdir.join("downloads"), &mut |l| j.log(l))
                    .map_err(|e| {
                        job.finish("source", StageState::Failed, "download failed");
                        e.to_string()
                    })?
            };
            let j = job.clone();
            sources::extract(&tarball, &req.workdir.join("sources"), &mut |l| j.log(l)).map_err(
                |e| {
                    job.finish("source", StageState::Failed, "extract failed");
                    e.to_string()
                },
            )?
        }
    };
    let version = sources::tree_version(&tree).unwrap_or_else(|| "unknown".into());
    {
        let mut st = job.state.lock().map_err(|_| "state poisoned")?;
        st.tree = Some(tree.clone());
    }
    job.log(&format!("Kernel {} at {}", version, tree.display()));
    job.finish("source", StageState::Done, format!("Linux {version}"));
    bail_if_cancelled(job)?;

    // ------------------------------------------------------------ 2. deps
    job.begin("deps");
    let report = crate::system::tool_report();
    let missing: Vec<&str> = report
        .iter()
        .filter(|t| t.spec.required && !t.found)
        .map(|t| t.spec.tool)
        .collect();
    if !missing.is_empty() && !req.config_only && !req.skip_tool_check {
        let pkgs = crate::system::missing_packages(&report);
        if let Some(cmd) = &pkgs.install_command {
            job.log(&format!("Install them with: {cmd}"));
        }
        job.finish("deps", StageState::Failed, missing.join(", "));
        return Err(format!("missing build tools: {}", missing.join(", ")));
    }
    let resolved = catalog::resolve_with_extra(&req.selections, &req.extra);
    if resolved.needs_clang() && !req.llvm {
        job.log("Link-time optimisation is clang-only, so this build uses LLVM=1.");
        job.llvm.store(true, Ordering::Relaxed);
    }
    if job.llvm.load(Ordering::Relaxed) && crate::system::which("clang").is_none() {
        job.finish("deps", StageState::Failed, "clang not installed");
        return Err("this build needs clang (LLVM=1), but clang is not installed".into());
    }
    for note in resolved.missing_tool_notes(|tool| crate::system::which(tool).is_some()) {
        job.log(&format!("warning: {}", note.text));
    }
    job.finish(
        "deps",
        StageState::Done,
        if missing.is_empty() {
            "all present".to_string()
        } else {
            format!("missing {} (continuing anyway)", missing.join(", "))
        },
    );

    // ------------------------------------------------------------ 3. base
    job.begin("base");
    let dot_config = tree.join(".config");
    let base_desc = match &req.base {
        Base::Running => {
            let path = crate::system::running_config_path()
                .ok_or("cannot find the running kernel's config on this machine")?;
            let cfg = Config::load(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
            if cfg.is_empty() {
                return Err(format!("{} contained no config symbols", path.display()));
            }
            cfg.save(&dot_config).map_err(|e| e.to_string())?;
            format!("{} ({} symbols)", path.display(), cfg.len())
        }
        Base::File(p) => {
            let cfg = Config::load(p).map_err(|e| format!("reading {}: {e}", p.display()))?;
            cfg.save(&dot_config).map_err(|e| e.to_string())?;
            format!("{} ({} symbols)", p.display(), cfg.len())
        }
        Base::Text(text) => {
            let cfg = Config::parse(text);
            if cfg.is_empty() {
                return Err("the pasted config contained no CONFIG_ symbols".into());
            }
            cfg.save(&dot_config).map_err(|e| e.to_string())?;
            format!("pasted config ({} symbols)", cfg.len())
        }
        Base::Tree => {
            if !dot_config.is_file() {
                return Err(format!(
                    "{} has no .config to start from - pick another starting point",
                    tree.display()
                ));
            }
            format!("existing {}", dot_config.display())
        }
        Base::Defconfig => {
            make(job, &tree, &["defconfig"], 1)?;
            "make defconfig".to_string()
        }
    };
    let base_config = Config::load(&dot_config).map_err(|e| e.to_string())?;
    job.log(&format!("Starting config: {base_desc}"));
    job.finish("base", StageState::Done, base_desc);
    bail_if_cancelled(job)?;

    // ------------------------------------------------------------ 4. trim
    if resolved.localmodconfig {
        job.begin("trim");
        // localmodconfig runs an interactive oldconfig for any symbol the base
        // does not mention. Bring the config up to date for this tree first so
        // there is nothing left to ask, and answer "default" to anything that
        // slips through.
        make(job, &tree, &["olddefconfig"], 1)?;
        job.log("Scanning loaded modules (make localmodconfig)...");
        make_with_stdin(job, &tree, &["localmodconfig"], 1, &"\n".repeat(4096))?;
        let after = Config::load(&dot_config).map_err(|e| e.to_string())?;
        job.finish(
            "trim",
            StageState::Done,
            format!("{} modules kept", after.module_count()),
        );
    } else {
        job.finish("trim", StageState::Skipped, "not requested");
    }
    bail_if_cancelled(job)?;

    // ---------------------------------------------------------- 5. tweaks
    job.begin("tweaks");
    let mut cfg = Config::load(&dot_config).map_err(|e| e.to_string())?;
    let changed = cfg.merge(&resolved.fragment);
    cfg.save(&dot_config).map_err(|e| e.to_string())?;
    let fragment_path = req.workdir.join("configs").join(format!("{}.fragment", job.id));
    let _ = fs::create_dir_all(fragment_path.parent().unwrap());
    let _ = fs::write(&fragment_path, catalog::fragment_text(&resolved.fragment));
    for c in &resolved.conflicts {
        job.log(&format!(
            "note: {} set {}={} after {} set it to {}",
            c.winning_tweak, c.symbol, c.winning_value, c.previous_tweak, c.previous_value
        ));
    }
    job.finish(
        "tweaks",
        StageState::Done,
        format!(
            "{} symbols requested, {} differed from the base",
            resolved.fragment.len(),
            changed.len()
        ),
    );
    bail_if_cancelled(job)?;

    // --------------------------------------------------------- 6. resolve
    job.begin("resolve");
    make(job, &tree, &["olddefconfig"], 1)?;
    let final_config = Config::load(&dot_config).map_err(|e| e.to_string())?;
    let changes = kconfig::diff(&base_config, &final_config);
    let saved = req.workdir.join("configs").join(format!("{}.config", job.id));
    let _ = final_config.save(&saved);
    {
        let mut st = job.state.lock().map_err(|_| "state poisoned")?;
        st.diff_count = changes.len();
        st.config_path = Some(saved.clone());
    }
    job.finish(
        "resolve",
        StageState::Done,
        format!(
            "{} symbols changed vs the base config, {} enabled in total",
            changes.len(),
            final_config.enabled_count()
        ),
    );

    // ---------------------------------------------------------- 7. verify
    job.begin("verify");
    let verification = kconfig::check(&resolved.fragment, &final_config, catalog::ALIASES);
    let unapplied = verification.unapplied.clone();
    for u in &unapplied {
        job.log(&format!(
            "warning: wanted {}, got {}",
            kconfig::format_line(&u.symbol, &u.wanted),
            if u.present {
                kconfig::format_line(&u.symbol, &u.actual)
            } else {
                format!(
                    "{} unavailable (not in this kernel version, or a dependency is off)",
                    u.symbol
                )
            }
        ));
    }
    let advice = advise(&final_config, &verification);
    for line in &advice {
        job.log(&format!("note: {line}"));
    }
    {
        let mut st = job.state.lock().map_err(|_| "state poisoned")?;
        st.unapplied = unapplied.clone();
        st.builtin_instead = verification.builtin_instead.clone();
        st.advice = advice;
    }
    job.finish(
        "verify",
        StageState::Done,
        if unapplied.is_empty() {
            "every requested option took effect".to_string()
        } else {
            format!(
                "{} of {} requested options did not stick",
                unapplied.len(),
                resolved.fragment.len()
            )
        },
    );
    if req.config_only {
        let mut st = job.state.lock().map_err(|_| "state poisoned")?;
        st.next_steps = vec![format!("Config written to {}", saved.display())];
        return Ok(());
    }
    bail_if_cancelled(job)?;

    // ----------------------------------------------------------- 8. clean
    if req.clean {
        job.begin("clean");
        make(job, &tree, &["clean"], 1)?;
        job.finish("clean", StageState::Done, "tree cleaned");
    }
    bail_if_cancelled(job)?;

    // ----------------------------------------------------------- 9. build
    job.begin("build");
    {
        let mut st = job.state.lock().map_err(|_| "state poisoned")?;
        st.phase = "compiling".into();
    }
    let target = req.output.make_target();
    job.log(&format!(
        "Building with {} parallel jobs (target: {target})",
        req.jobs
    ));
    make(job, &tree, &[target], req.jobs)?;
    let objects = job.state.lock().map(|s| s.objects).unwrap_or(0);
    save_expected_objects(&req.workdir, objects);
    job.finish("build", StageState::Done, format!("{objects} objects compiled"));
    bail_if_cancelled(job)?;

    // --------------------------------------------------------- 10. collect
    job.begin("collect");
    let artifacts = collect_artifacts(&tree, req.output, &crate::system::kernel_arch());
    let total: u64 = artifacts.iter().map(|(_, s)| s).sum();
    {
        let mut st = job.state.lock().map_err(|_| "state poisoned")?;
        st.artifacts = artifacts.clone();
    }
    job.finish(
        "collect",
        StageState::Done,
        format!("{} files, {:.1} MiB", artifacts.len(), total as f64 / 1048576.0),
    );

    // --------------------------------------------------------- 11. install
    let steps = install_steps(&tree, req.output, &artifacts);
    if req.install {
        if crate::system::is_root() {
            job.begin("install");
            {
                let mut st = job.state.lock().map_err(|_| "state poisoned")?;
                st.phase = "installing".into();
            }
            if req.output == Output::Image {
                make(job, &tree, &["modules_install"], req.jobs)?;
                make(job, &tree, &["install"], 1)?;
                job.finish("install", StageState::Done, "installed to /boot");
            } else {
                // Packages are deliberately not force-installed: handing them
                // to dpkg/rpm is the user's call, and their package manager
                // should own the files.
                job.finish(
                    "install",
                    StageState::Skipped,
                    "install the package yourself with the command below",
                );
            }
        } else {
            job.log("Not running as root, so nothing was installed.");
            job.finish(
                "install",
                StageState::Skipped,
                "needs root - run the commands below yourself",
            );
        }
    } else {
        job.finish("install", StageState::Skipped, "not requested");
    }

    {
        let mut st = job.state.lock().map_err(|_| "state poisoned")?;
        st.next_steps = steps;
    }
    Ok(())
}

/// Explain the resolved config in plain language where the raw lists would
/// leave someone guessing at the cause.
fn advise(config: &Config, v: &kconfig::Verification) -> Vec<String> {
    let mut out = Vec::new();
    let modules_off = !config.enabled("CONFIG_MODULES");
    if !v.builtin_instead.is_empty() {
        out.push(if modules_off {
            format!(
                "This config has loadable modules switched off (CONFIG_MODULES), so {} option(s) \
                 you asked for as modules are built into the kernel image instead.",
                v.builtin_instead.len()
            )
        } else {
            format!(
                "{} option(s) you asked for as modules are built in instead, because something \
                 built in depends on them.",
                v.builtin_instead.len()
            )
        });
    }
    if modules_off && !v.unapplied.is_empty() {
        out.push(
            "Loadable modules are off, and some options (kprobes, the BPF JIT, module signing \
             and compression) depend on module support. Turn on 'Loadable module support' under \
             Build size & speed if you want them."
                .to_string(),
        );
    }
    out
}

fn bail_if_cancelled(job: &Arc<Job>) -> Result<(), String> {
    if job.cancelled() {
        Err("cancelled".into())
    } else {
        Ok(())
    }
}

/// Run `make` in the tree, streaming every line into the job log.
fn make(job: &Arc<Job>, tree: &Path, args: &[&str], jobs: usize) -> Result<(), String> {
    make_with_stdin(job, tree, args, jobs, "")
}

fn make_with_stdin(
    job: &Arc<Job>,
    tree: &Path,
    args: &[&str],
    jobs: usize,
    stdin_text: &str,
) -> Result<(), String> {
    let req = &job.request;
    let mut cmd = Command::new(&req.make);
    // Its own process group: a parallel build spawns dozens of compilers, and
    // killing only the top-level make would leave them running (and holding
    // the output pipe open, so cancelling would appear to hang).
    cmd.process_group(0)
        .current_dir(tree)
        .arg(format!("-j{}", jobs.max(1)))
        .args(args)
        .env("LC_ALL", "C")
        // Keep kbuild from opening an interactive dialog if a new symbol shows up.
        .env("KCONFIG_NOSILENTUPDATE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let llvm = job.llvm.load(Ordering::Relaxed);
    if llvm {
        cmd.arg("LLVM=1");
    }

    job.log(&format!(
        "$ {} -j{} {}{}",
        req.make,
        jobs.max(1),
        args.join(" "),
        if llvm { " LLVM=1" } else { "" }
    ));

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", req.make))?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(stdin_text.as_bytes());
    }

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    *job.child.lock().map_err(|_| "child lock poisoned")? = Some(child);

    let err_thread = stderr.map(|s| {
        let j = job.clone();
        thread::spawn(move || {
            for line in BufReader::new(s).lines().map_while(Result::ok) {
                j.log(&line);
            }
        })
    });

    if let Some(out) = stdout {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            note_progress(job, &line);
            job.log(&line);
        }
    }
    if let Some(t) = err_thread {
        let _ = t.join();
    }

    let status = {
        let mut slot = job.child.lock().map_err(|_| "child lock poisoned")?;
        match slot.as_mut() {
            Some(c) => c.wait().map_err(|e| e.to_string())?,
            None => return Err("build process vanished".into()),
        }
    };
    *job.child.lock().map_err(|_| "child lock poisoned")? = None;

    if job.cancelled() {
        return Err("cancelled".into());
    }
    if !status.success() {
        return Err(format!(
            "make {} failed ({}). See the log for the first error.",
            args.join(" "),
            status
                .code()
                .map(|c| format!("exit {c}"))
                .unwrap_or_else(|| "killed".into())
        ));
    }
    Ok(())
}

/// Count compiled objects and name the current phase, for the progress bar.
fn note_progress(job: &Arc<Job>, line: &str) {
    let t = line.trim_start();
    let verb = t.split_whitespace().next().unwrap_or("");
    let counts = matches!(
        verb,
        "CC" | "AS" | "CC_FPIC" | "CC_NOSTDINC" | "AR" | "OBJCOPY" | "RUSTC"
    );
    let phase = match verb {
        "LD" | "LINK" | "LZ4" | "ZSTD" | "GZIP" | "XZ" | "BZIP2" => Some("linking"),
        "MODPOST" => Some("modules"),
        "BTF" | "BTFIDS" => Some("btf"),
        "INSTALL" | "DPKG-DEB" | "RPM" => Some("packaging"),
        "SIGN" => Some("signing"),
        _ => None,
    };
    if !counts && phase.is_none() {
        return;
    }
    if let Ok(mut st) = job.state.lock() {
        if counts {
            st.objects += 1;
        }
        if let Some(p) = phase {
            if st.phase != p {
                st.phase = p.to_string();
            }
        }
    }
}

fn collect_artifacts(tree: &Path, output: Output, arch: &str) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut add = |p: PathBuf| {
        if let Ok(meta) = fs::metadata(&p) {
            if meta.is_file() {
                out.push((p.to_string_lossy().into_owned(), meta.len()));
            }
        }
    };
    match output {
        Output::Image => {
            for rel in [
                format!("arch/{arch}/boot/bzImage"),
                format!("arch/{arch}/boot/Image"),
                format!("arch/{arch}/boot/Image.gz"),
                format!("arch/{arch}/boot/zImage"),
                "vmlinux".to_string(),
                "System.map".to_string(),
                ".config".to_string(),
            ] {
                add(tree.join(rel));
            }
        }
        Output::Deb | Output::Rpm | Output::Tar => {
            // Package targets drop their output in the parent directory.
            let wanted: &[&str] = match output {
                Output::Deb => &[".deb"],
                Output::Rpm => &[".rpm"],
                _ => &[".tar", ".tar.xz", ".tar.gz"],
            };
            if let Some(parent) = tree.parent() {
                if let Ok(entries) = fs::read_dir(parent) {
                    let mut paths: Vec<PathBuf> = entries
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| {
                            let name = p.file_name().unwrap_or_default().to_string_lossy();
                            wanted.iter().any(|ext| name.ends_with(ext))
                        })
                        .collect();
                    paths.sort();
                    for p in paths {
                        add(p);
                    }
                }
            }
            if output == Output::Rpm {
                for sub in ["rpmbuild/RPMS/x86_64", "rpmbuild/RPMS/noarch"] {
                    if let Some(home) = std::env::var_os("HOME") {
                        let dir = PathBuf::from(&home).join(sub);
                        if let Ok(entries) = fs::read_dir(&dir) {
                            let mut paths: Vec<PathBuf> =
                                entries.flatten().map(|e| e.path()).collect();
                            paths.sort();
                            for p in paths {
                                add(p);
                            }
                        }
                    }
                }
            }
            add(tree.join(".config"));
        }
    }
    out
}

/// The exact commands a user should run to install what was just built.
fn install_steps(tree: &Path, output: Output, artifacts: &[(String, u64)]) -> Vec<String> {
    let dir = tree.display();
    match output {
        Output::Image => vec![
            format!("sudo make -C {dir} modules_install"),
            format!("sudo make -C {dir} install"),
            "sudo update-initramfs -c -k <version>   # Debian/Ubuntu".to_string(),
            "sudo dracut --force --kver <version>    # Fedora/RHEL".to_string(),
            "sudo update-grub                        # or: sudo grub2-mkconfig -o /boot/grub2/grub.cfg".to_string(),
        ],
        Output::Deb => {
            let debs: Vec<String> = artifacts
                .iter()
                .map(|(p, _)| p.clone())
                .filter(|p| p.ends_with(".deb") && !p.contains("-dbg"))
                .collect();
            if debs.is_empty() {
                vec!["sudo dpkg -i ../linux-image-*.deb ../linux-headers-*.deb".to_string()]
            } else {
                vec![format!("sudo dpkg -i {}", debs.join(" "))]
            }
        }
        Output::Rpm => vec!["sudo rpm -ivh <the .rpm files listed above>".to_string()],
        Output::Tar => vec![
            "sudo tar -xf <the tarball above> -C /".to_string(),
            "sudo depmod -a <version> && sudo update-grub".to_string(),
        ],
    }
}

/// Remember how many objects the last successful build compiled, so the next
/// build can show a real percentage instead of a spinner.
fn stats_path(workdir: &Path) -> PathBuf {
    workdir.join("build-stats.json")
}

fn load_expected_objects(workdir: &Path) -> Option<u64> {
    let text = fs::read_to_string(stats_path(workdir)).ok()?;
    crate::json::parse(&text)
        .ok()?
        .get("objects")?
        .as_f64()
        .map(|n| n as u64)
}

fn save_expected_objects(workdir: &Path, objects: u64) {
    if objects < 100 {
        return; // a stub or a no-op rebuild; not a useful estimate
    }
    let j = Json::obj(vec![
        ("objects", Json::n(objects as f64)),
        ("saved_at", Json::n(now_secs())),
    ]);
    let _ = fs::write(stats_path(workdir), j.to_string());
}

// -------------------------------------------------------------------- registry

/// All jobs this process has run, newest last.
#[derive(Default)]
pub struct Registry {
    jobs: Mutex<Vec<Arc<Job>>>,
    counter: AtomicU64,
}

impl Registry {
    pub fn new() -> Registry {
        Registry::default()
    }

    pub fn next_id(&self) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        format!("build-{:03}-{}", n, (now_secs() as u64) % 100000)
    }

    pub fn start(&self, req: Request) -> Arc<Job> {
        let job = new_job(self.next_id(), req);
        self.jobs.lock().expect("registry poisoned").push(job.clone());
        let runner = job.clone();
        thread::spawn(move || run(runner));
        job
    }

    pub fn get(&self, id: &str) -> Option<Arc<Job>> {
        self.jobs
            .lock()
            .ok()?
            .iter()
            .find(|j| j.id == id)
            .cloned()
    }

    pub fn all(&self) -> Vec<Arc<Job>> {
        self.jobs.lock().map(|j| j.clone()).unwrap_or_default()
    }

    /// True while any job is still running.
    pub fn busy(&self) -> bool {
        self.all().iter().any(|j| {
            j.state
                .lock()
                .map(|s| !s.status.finished())
                .unwrap_or(false)
        })
    }
}

/// Block until the job finishes (used by the CLI and by tests).
pub fn wait(job: &Arc<Job>, timeout: Duration) -> Status {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Ok(st) = job.state.lock() {
            if st.status.finished() {
                return st.status;
            }
        }
        if std::time::Instant::now() > deadline {
            job.request_cancel();
            return Status::Cancelled;
        }
        thread::sleep(Duration::from_millis(50));
    }
}
