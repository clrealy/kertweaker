//! Command-line entry point: starts the GUI by default, with subcommands for
//! people who would rather script it.

use kertweaker::api::{self, App};
use kertweaker::build::{self, Base, Output, Request, Source, Status};
use kertweaker::http::{self, Reply};
use kertweaker::{catalog, sources, system};
use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_PORT: u16 = 7878;

const HELP: &str = "\
kertweaker - tweak a Linux kernel from a GUI and build it for this machine

USAGE:
    kertweaker [gui] [--port N] [--no-open] [--workdir DIR]
    kertweaker doctor
    kertweaker profiles
    kertweaker tweaks [--group ID]
    kertweaker fragment [--profile ID] [--set TWEAK=OPTION]...
    kertweaker build (--tree PATH | --kernel VERSION) [options]

COMMANDS:
    gui         Start the local web GUI (the default)
    doctor      Check this machine for the tools a kernel build needs
    profiles    List the built-in profiles
    tweaks      List every tweak and its options
    fragment    Print the config fragment a selection produces
    build       Configure and build without the GUI

BUILD OPTIONS:
    --tree PATH           Build a kernel source tree already on disk
    --kernel VERSION      Download this version from kernel.org (e.g. 6.12.1)
    --profile ID          Start from a built-in profile
    --set TWEAK=OPTION    Set one tweak; repeatable, applied after --profile
    --base WHAT           running (default) | defconfig | tree | /path/to/config
    --output FORMAT       image (default) | deb | rpm
    -j, --jobs N          Parallel jobs (default: sized to this machine)
    --config-only         Write and verify the .config, build nothing
    --clang               Build with LLVM=1
    --clean               Run 'make clean' first
    --install             Install the result (image output, needs root)
    --yes                 Accept risky options without being asked
    --skip-tool-check     Build even if a required tool looks missing

GENERAL:
    --workdir DIR         Where sources, configs and logs live
                          (default: ~/.local/share/kertweaker)
    -h, --help            Show this help
    -V, --version         Show the version
";

struct Args {
    command: String,
    port: Option<u16>,
    open: bool,
    workdir: PathBuf,
    group: Option<String>,
    profile: Option<String>,
    sets: Vec<(String, String)>,
    tree: Option<PathBuf>,
    kernel: Option<String>,
    base: String,
    output: String,
    jobs: Option<usize>,
    config_only: bool,
    clang: bool,
    clean: bool,
    install: bool,
    yes: bool,
    skip_tool_check: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        command: "gui".into(),
        port: None,
        open: true,
        workdir: system::default_workdir(),
        group: None,
        profile: None,
        sets: Vec::new(),
        tree: None,
        kernel: None,
        base: "running".into(),
        output: "image".into(),
        jobs: None,
        config_only: false,
        clang: false,
        clean: false,
        install: false,
        yes: false,
        skip_tool_check: false,
    };
    let mut it = std::env::args().skip(1);
    let mut seen_command = false;
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "-h" | "--help" | "help" => args.command = "help".into(),
            "-V" | "--version" => args.command = "version".into(),
            "--port" => {
                args.port = Some(value("--port")?.parse().map_err(|_| "--port needs a number")?)
            }
            "--no-open" => args.open = false,
            "--workdir" => args.workdir = PathBuf::from(value("--workdir")?),
            "--group" => args.group = Some(value("--group")?),
            "--profile" => args.profile = Some(value("--profile")?),
            "--set" => {
                let v = value("--set")?;
                let (k, o) = v.split_once('=').ok_or("--set expects TWEAK=OPTION")?;
                args.sets.push((k.to_string(), o.to_string()));
            }
            "--tree" => args.tree = Some(PathBuf::from(value("--tree")?)),
            "--kernel" => args.kernel = Some(value("--kernel")?),
            "--base" => args.base = value("--base")?,
            "--output" => args.output = value("--output")?,
            "-j" | "--jobs" => {
                args.jobs = Some(value("--jobs")?.parse().map_err(|_| "--jobs needs a number")?)
            }
            "--config-only" => args.config_only = true,
            "--clang" => args.clang = true,
            "--clean" => args.clean = true,
            "--install" => args.install = true,
            "--yes" => args.yes = true,
            "--skip-tool-check" => args.skip_tool_check = true,
            s if s.starts_with("-j") && s[2..].parse::<usize>().is_ok() => {
                args.jobs = s[2..].parse().ok();
            }
            s if !s.starts_with('-') && !seen_command => {
                args.command = s.to_string();
                seen_command = true;
            }
            other => return Err(format!("unknown argument {other:?} (try --help)")),
        }
    }
    Ok(args)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let result = match args.command.as_str() {
        "help" => {
            print!("{HELP}");
            Ok(())
        }
        "version" => {
            println!("kertweaker {}", kertweaker::VERSION);
            Ok(())
        }
        "gui" => cmd_gui(&args),
        "doctor" => cmd_doctor(&args),
        "profiles" => {
            cmd_profiles();
            Ok(())
        }
        "tweaks" => {
            cmd_tweaks(&args);
            Ok(())
        }
        "fragment" => cmd_fragment(&args),
        "build" => cmd_build(&args),
        other => Err(format!("unknown command {other:?} (try --help)")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

// ------------------------------------------------------------------------ gui

fn cmd_gui(args: &Args) -> Result<(), String> {
    std::fs::create_dir_all(&args.workdir)
        .map_err(|e| format!("cannot create {}: {e}", args.workdir.display()))?;

    // An explicit --port must be honoured or fail; the default may slide up.
    let candidates: Vec<u16> = match args.port {
        Some(p) => vec![p],
        None => (DEFAULT_PORT..DEFAULT_PORT + 20).collect(),
    };
    let listener = candidates
        .iter()
        .find_map(|p| TcpListener::bind(("127.0.0.1", *p)).ok())
        .ok_or_else(|| match args.port {
            Some(p) => format!("port {p} is already in use"),
            None => "no free port found near 7878 (pass --port)".to_string(),
        })?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();

    let app = Arc::new(App::new(args.workdir.clone(), port));
    let url = app.url();
    println!("kertweaker {} is running.", kertweaker::VERSION);
    println!();
    println!("  Open:     {url}");
    println!("  Workdir:  {}", args.workdir.display());
    println!();
    println!("The link carries a one-time access token; it only works from this machine.");
    println!("Press Ctrl+C to stop.");

    if args.open && open_browser(&url) {
        println!("(opened in your browser)");
    }

    let handler_app = app.clone();
    http::serve(listener, move |req| -> Reply { api::handle(&handler_app, req) })
        .map_err(|e| e.to_string())
}

/// Best effort: only when there is a desktop session to open a browser in.
fn open_browser(url: &str) -> bool {
    let has_display =
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
    if !has_display || system::which("xdg-open").is_none() {
        return false;
    }
    Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

// --------------------------------------------------------------------- doctor

fn cmd_doctor(args: &Args) -> Result<(), String> {
    let cpu = system::cpu();
    let os = system::os_release();
    println!("Machine");
    println!(
        "  distro      {}",
        os.get("PRETTY_NAME").cloned().unwrap_or_else(|| "unknown".into())
    );
    println!("  kernel      {}", system::uname("-r"));
    println!("  cpu         {} ({} threads)", cpu.model, cpu.threads);
    println!("  memory      {} GiB", system::memory_gib());
    println!(
        "  free disk   {} GiB at {}",
        system::free_disk_gib(args.workdir.parent().unwrap_or(&args.workdir)),
        args.workdir.display()
    );
    println!(
        "  config      {}",
        system::running_config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "running kernel config not available".into())
    );
    println!(
        "  secure boot {}",
        match system::secure_boot() {
            Some(true) => "ON - an unsigned kernel will not boot",
            Some(false) => "off",
            None => "unknown",
        }
    );
    println!("  jobs        {} suggested", system::suggested_jobs());
    println!();
    println!("Build tools");
    let report = system::tool_report();
    for t in &report {
        let mark = if t.found {
            "ok     "
        } else if t.spec.required {
            "MISSING"
        } else {
            "absent "
        };
        println!(
            "  {mark} {:<9} {}{}",
            t.spec.tool,
            t.spec.why,
            if t.spec.required { "" } else { " (optional)" }
        );
    }
    let pkgs = system::missing_packages(&report);
    println!();
    if pkgs.missing_required.is_empty() {
        println!("Ready to build.");
        Ok(())
    } else {
        if let Some(cmd) = pkgs.install_command {
            println!("Install what is missing with:\n  {cmd}");
        }
        Err(format!(
            "missing required tools: {}",
            pkgs.missing_required.join(", ")
        ))
    }
}

// -------------------------------------------------------------------- listing

fn cmd_profiles() {
    for p in catalog::PROFILES {
        println!("{:<12} {}", p.id, p.title);
        println!("             {}", p.desc);
        println!("             {} tweaks; for: {}", p.selections.len(), p.recommended_for);
        println!();
    }
}

fn cmd_tweaks(args: &Args) {
    for g in catalog::GROUPS {
        if args.group.as_deref().is_some_and(|want| want != g.id) {
            continue;
        }
        println!("== {} ({}) ==", g.title, g.id);
        for t in catalog::tweaks().iter().filter(|t| t.group == g.id) {
            println!("  {}  - {}", t.id, t.title);
            match t.kind {
                catalog::Kind::Text | catalog::Kind::Number => {
                    println!("      --set {}=<value>   writes {}", t.id, t.symbol)
                }
                _ => {
                    for o in t.opts.iter().filter(|o| o.id != "keep") {
                        let risk = match o.risk {
                            catalog::Risk::Safe => "",
                            catalog::Risk::Caution => "  [caution]",
                            catalog::Risk::Risky => "  [risky]",
                        };
                        println!("      --set {}={:<16} {}{risk}", t.id, o.id, o.label);
                    }
                }
            }
        }
        println!();
    }
}

fn selections(args: &Args) -> Result<BTreeMap<String, String>, String> {
    let mut sel = match &args.profile {
        Some(id) => catalog::profile_selections(id).ok_or_else(|| {
            format!(
                "no profile {id:?}; available: {}",
                catalog::PROFILES
                    .iter()
                    .map(|p| p.id)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?,
        None => BTreeMap::new(),
    };
    for (k, v) in &args.sets {
        sel.insert(k.clone(), v.clone());
    }
    let resolved = catalog::resolve(&sel);
    if !resolved.unknown.is_empty() {
        return Err(format!(
            "unknown tweak or option: {} (see 'kertweaker tweaks')",
            resolved.unknown.join(", ")
        ));
    }
    Ok(sel)
}

fn cmd_fragment(args: &Args) -> Result<(), String> {
    let resolved = catalog::resolve(&selections(args)?);
    for n in &resolved.notes {
        eprintln!("# {}: {}", n.level, n.text);
    }
    print!("{}", catalog::fragment_text(&resolved.fragment));
    Ok(())
}

// ---------------------------------------------------------------------- build

fn cmd_build(args: &Args) -> Result<(), String> {
    let source = match (&args.tree, &args.kernel) {
        (Some(p), None) => Source::Local(p.clone()),
        (None, Some(v)) => {
            if !sources::valid_version(v) {
                return Err(format!("{v:?} is not a valid kernel version"));
            }
            Source::Download(v.clone())
        }
        (Some(_), Some(_)) => return Err("pass --tree or --kernel, not both".into()),
        (None, None) => return Err("pass --tree PATH or --kernel VERSION".into()),
    };
    let base = match args.base.as_str() {
        "running" => Base::Running,
        "defconfig" => Base::Defconfig,
        "tree" => Base::Tree,
        path => Base::File(PathBuf::from(path)),
    };
    let sel = selections(args)?;
    let resolved = catalog::resolve(&sel);
    for n in &resolved.notes {
        eprintln!("{}: {}", n.level, n.text);
    }
    if resolved.risk == catalog::Risk::Risky && !args.yes {
        return Err("this selection includes risky options; re-run with --yes to accept them".into());
    }

    let req = Request {
        source,
        base,
        selections: sel,
        extra: String::new(),
        jobs: args.jobs.unwrap_or_else(system::suggested_jobs).clamp(1, 256),
        output: Output::parse(&args.output),
        llvm: args.clang,
        clean: args.clean,
        install: args.install,
        config_only: args.config_only,
        skip_tool_check: args.skip_tool_check,
        workdir: args.workdir.clone(),
        make: Request::make_cmd(),
    };

    let job = build::new_job(format!("cli-{}", std::process::id()), req);
    let runner = job.clone();
    let handle = std::thread::spawn(move || build::run(runner));

    // Mirror the job log to the terminal as it grows.
    let mut cursor = 0usize;
    loop {
        let (lines, finished) = {
            let st = job.state.lock().map_err(|_| "job state poisoned")?;
            let start = cursor.saturating_sub(st.dropped).min(st.log.len());
            let lines = st.log[start..].to_vec();
            cursor = st.dropped + st.log.len();
            (lines, st.status.finished())
        };
        for l in lines {
            println!("{l}");
        }
        if finished {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = handle.join();

    let st = job.state.lock().map_err(|_| "job state poisoned")?;
    println!();
    for s in &st.stages {
        println!("  {:<8} {:<38} {}", format!("{:?}", s.state).to_lowercase(), s.title, s.detail);
    }
    if !st.unapplied.is_empty() {
        println!();
        println!("{} requested option(s) did not stick - see the warnings above.", st.unapplied.len());
    }
    if !st.artifacts.is_empty() {
        println!();
        println!("Built:");
        for (p, size) in &st.artifacts {
            println!("  {p}  ({:.1} MiB)", *size as f64 / 1048576.0);
        }
    }
    if st.status == Status::Done && !st.next_steps.is_empty() {
        println!();
        println!("Next:");
        for s in &st.next_steps {
            println!("  {s}");
        }
    }
    match st.status {
        Status::Done => Ok(()),
        Status::Cancelled => Err("build cancelled".into()),
        _ => Err(st.error.clone().unwrap_or_else(|| "build failed".into())),
    }
}
