//! Command-line entry point: starts the GUI by default, with subcommands for
//! people who would rather script it.

use kertweaker::api::{self, App};
use kertweaker::build::{self, Base, Output, Request, Source, Status};
use kertweaker::http::{self, Reply};
use kertweaker::recipe::{self, Recipe};
use kertweaker::{catalog, kconfig, software, sources, system};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_PORT: u16 = 7878;

const HELP: &str = "\
kertweaker - tweak a Linux kernel from a GUI and build it for this machine

USAGE:
    kertweaker [gui] [FILE.kernel] [--port N] [--no-open] [--workdir DIR]
    kertweaker doctor
    kertweaker profiles | tweaks [--group ID] | apps
    kertweaker fragment [SELECTION]
    kertweaker build (--file FILE.kernel | --tree PATH | --kernel VERSION) [options]
    kertweaker export -o FILE.kernel [SELECTION] [--kernel VERSION] [--name NAME]
    kertweaker show FILE.kernel

COMMANDS:
    gui         Start the local web GUI (the default). Give it a .kernel file
                to open it with that kernel loaded.
    doctor      Check this machine for the tools a kernel build needs
    profiles    List the built-in profiles
    tweaks      List every tweak and its options
    apps        List software kertweaker can prepare a kernel for
    fragment    Print the config fragment a selection produces
    build       Configure and build without the GUI
    export      Write a selection to a .kernel file
    show        Describe a .kernel file without building anything

SELECTION (fragment, build, export):
    --file FILE.kernel    Start from a .kernel file
    --profile ID          Start from a built-in profile
    --set TWEAK=OPTION    Set one tweak; repeatable, applied last
    --app ID              Add software and the kernel features it needs; repeatable
    --package NAME        Add a package to install alongside the kernel; repeatable

BUILD OPTIONS:
    --tree PATH           Build a kernel source tree already on disk
    --kernel VERSION      Download this version from kernel.org (e.g. 6.12.1)
    --base WHAT           running (default) | defconfig | tree | /path/to/config
    --output FORMAT       image (default) | deb | rpm
    -j, --jobs N          Parallel jobs (default: sized to this machine)
    --config-only         Write and verify the .config, build nothing
    --clang               Build with LLVM=1
    --clean               Run 'make clean' first
    --install             Install the result (image output, needs root)
    --yes                 Accept risky options without being asked
    --skip-tool-check     Build even if a required tool looks missing

EXPORT OPTIONS:
    -o, --out FILE        Where to write the .kernel file
    --name NAME           Name stored in the file
    --embed-config        Store the full starting config in the file, so it
                          rebuilds the same kernel on any machine

GENERAL:
    --workdir DIR         Where sources, configs and logs live
                          (default: ~/.local/share/kertweaker)
    -h, --help            Show this help
    -V, --version         Show the version
";

#[derive(Default)]
struct Args {
    command: String,
    /// A second positional argument: the file for `show`, or for the GUI.
    path: Option<PathBuf>,
    port: Option<u16>,
    no_open: bool,
    workdir: Option<PathBuf>,
    group: Option<String>,
    file: Option<PathBuf>,
    profile: Option<String>,
    sets: Vec<(String, String)>,
    apps: Vec<String>,
    packages: Vec<String>,
    tree: Option<PathBuf>,
    kernel: Option<String>,
    base: Option<String>,
    output: Option<String>,
    jobs: Option<usize>,
    out: Option<PathBuf>,
    name: Option<String>,
    embed_config: bool,
    config_only: bool,
    clang: bool,
    clean: bool,
    install: bool,
    yes: bool,
    skip_tool_check: bool,
}

impl Args {
    fn workdir(&self) -> PathBuf {
        self.workdir.clone().unwrap_or_else(system::default_workdir)
    }
}

fn is_kernel_file(s: &str) -> bool {
    s.ends_with(&format!(".{}", recipe::EXTENSION))
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        command: "gui".into(),
        ..Args::default()
    };
    let mut it = std::env::args().skip(1);
    let mut seen_command = false;
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "-h" | "--help" | "help" => args.command = "help".into(),
            "-V" | "--version" => args.command = "version".into(),
            "--port" => {
                args.port = Some(
                    value("--port")?
                        .parse()
                        .map_err(|_| "--port needs a number")?,
                )
            }
            "--no-open" => args.no_open = true,
            "--workdir" => args.workdir = Some(PathBuf::from(value("--workdir")?)),
            "--group" => args.group = Some(value("--group")?),
            "--file" => args.file = Some(PathBuf::from(value("--file")?)),
            "--profile" => args.profile = Some(value("--profile")?),
            "--set" => {
                let v = value("--set")?;
                let (k, o) = v.split_once('=').ok_or("--set expects TWEAK=OPTION")?;
                args.sets.push((k.to_string(), o.to_string()));
            }
            "--app" => args.apps.push(value("--app")?),
            "--package" => args.packages.push(value("--package")?),
            "--tree" => args.tree = Some(PathBuf::from(value("--tree")?)),
            "--kernel" => args.kernel = Some(value("--kernel")?),
            "--base" => args.base = Some(value("--base")?),
            "--output" => args.output = Some(value("--output")?),
            "-o" | "--out" => args.out = Some(PathBuf::from(value("--out")?)),
            "--name" => args.name = Some(value("--name")?),
            "--embed-config" => args.embed_config = true,
            "-j" | "--jobs" => {
                args.jobs = Some(
                    value("--jobs")?
                        .parse()
                        .map_err(|_| "--jobs needs a number")?,
                )
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
            // `kertweaker my.kernel` opens the GUI with that file loaded.
            s if !s.starts_with('-') && !seen_command && is_kernel_file(s) => {
                args.path = Some(PathBuf::from(s));
                seen_command = true;
            }
            s if !s.starts_with('-') && !seen_command => {
                args.command = s.to_string();
                seen_command = true;
            }
            s if !s.starts_with('-') && args.path.is_none() => {
                args.path = Some(PathBuf::from(s));
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
        "apps" => {
            cmd_apps();
            Ok(())
        }
        "fragment" => cmd_fragment(&args),
        "build" => cmd_build(&args),
        "export" => cmd_export(&args),
        "show" => cmd_show(&args),
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

/// Read and validate a `.kernel` file, printing any warnings to stderr.
fn load_recipe(path: &std::path::Path) -> Result<(Recipe, Vec<String>), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let (r, warnings) = recipe::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    Ok((r, warnings))
}

// ------------------------------------------------------------------------ gui

fn cmd_gui(args: &Args) -> Result<(), String> {
    let workdir = args.workdir();
    std::fs::create_dir_all(&workdir)
        .map_err(|e| format!("cannot create {}: {e}", workdir.display()))?;

    let preload = match args.path.as_ref().or(args.file.as_ref()) {
        Some(path) => Some(load_recipe(path)?),
        None => None,
    };

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

    let mut app = App::new(workdir.clone(), port);
    if let Some((r, _)) = &preload {
        println!("Loaded {} from the .kernel file.", r.name);
    }
    app.preload = preload;
    let app = Arc::new(app);
    let url = app.url();
    println!("kertweaker {} is running.", kertweaker::VERSION);
    println!();
    println!("  Open:     {url}");
    println!("  Workdir:  {}", workdir.display());
    println!();
    println!("The link carries this run's access token and only works from this machine.");
    println!("Press Ctrl+C to stop.");

    if !args.no_open && open_browser(&url) {
        println!("(opened in your browser)");
    }

    // List the distribution's packages in the background, so the first
    // "is this installable?" check on the Review page does not wait for it.
    std::thread::spawn(software::warm_cache);

    let handler_app = app.clone();
    http::serve(listener, move |req| -> Reply {
        api::handle(&handler_app, req)
    })
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
    let workdir = args.workdir();
    let cpu = system::cpu();
    let os = system::os_release();
    println!("Machine");
    println!(
        "  distro      {}",
        os.get("PRETTY_NAME")
            .cloned()
            .unwrap_or_else(|| "unknown".into())
    );
    println!("  kernel      {}", system::uname("-r"));
    println!("  cpu         {} ({} threads)", cpu.model, cpu.threads);
    println!("  memory      {} GiB", system::memory_gib());
    println!(
        "  free disk   {} GiB at {}",
        system::free_disk_gib(workdir.parent().unwrap_or(&workdir)),
        workdir.display()
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
            "  {mark} {:<17} {}{}",
            t.spec.tool,
            t.spec.why,
            match (t.spec.required, t.spec.output) {
                (true, _) => String::new(),
                (false, Some(out)) => format!(" (only for .{out} output)"),
                (false, None) => " (optional)".to_string(),
            }
        );
    }
    let pkgs = system::missing_packages(&report);
    println!();
    let native = system::output_tools(pkgs.family, &report);
    if let (false, Some(cmd)) = (native.missing.is_empty(), native.install_command) {
        println!("For .{} package output, also run:\n  {cmd}\n", pkgs.family);
    }
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
        println!(
            "             {} tweaks; for: {}",
            p.selections.len(),
            p.recommended_for
        );
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

fn cmd_apps() {
    let family = system::package_family();
    println!("Software kertweaker can prepare a kernel for. Apps cannot be compiled into a");
    println!("kernel; --app turns on the kernel features an app needs and adds it to the");
    println!("list of things to install alongside.");
    println!();
    for a in software::APPS {
        println!("--app {:<16} {}", a.id, a.title);
        println!("      {}", a.desc);
        println!(
            "      kernel options: {}",
            if a.kernel.is_empty() {
                "none needed".to_string()
            } else {
                a.kernel.len().to_string()
            }
        );
        let pkgs = a.packages(family);
        let how = match (pkgs.is_empty(), a.flatpak, a.script) {
            (false, _, _) => pkgs.join(" "),
            (true, Some(id), _) => format!("flatpak {id}"),
            (true, None, Some(url)) => format!("its own script, {url} (read it before running)"),
            (true, None, None) => "no package known for this distribution".to_string(),
        };
        println!("      installs:       {how}");
        println!();
    }
    println!("Anything else: --package NAME adds it to the install list as-is.");
}

// ------------------------------------------------------------------ selection

/// Everything a selection adds up to: a .kernel file, then a profile, then flags.
fn selection(args: &Args) -> Result<Recipe, String> {
    let mut r = match &args.file {
        Some(path) => load_recipe(path)?.0,
        None => Recipe::default(),
    };

    if let Some(id) = &args.profile {
        let profile = catalog::profile_selections(id).ok_or_else(|| {
            format!(
                "no profile {id:?}; available: {}",
                catalog::PROFILES
                    .iter()
                    .map(|p| p.id)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        r.selections.extend(profile);
    }
    for (k, v) in &args.sets {
        r.selections.insert(k.clone(), v.clone());
    }
    for id in &args.apps {
        if software::app(id).is_none() {
            return Err(format!(
                "{id:?} is not software kertweaker knows (see 'kertweaker apps'); \
                 use --package {id} to add it as a plain package"
            ));
        }
        if !r.software.contains(id) {
            r.software.push(id.clone());
        }
    }
    for name in &args.packages {
        if !software::valid_package(name) {
            return Err(format!("{name:?} is not a valid package name"));
        }
        if !r.packages.contains(name) {
            r.packages.push(name.clone());
        }
    }

    let resolved = software::resolve(&r.selections, &r.software, &r.extra);
    if !resolved.unknown.is_empty() {
        return Err(format!(
            "unknown tweak or option: {} (see 'kertweaker tweaks')",
            resolved.unknown.join(", ")
        ));
    }
    if let Some(v) = &args.kernel {
        if !sources::valid_version(v) {
            return Err(format!("{v:?} is not a valid kernel version"));
        }
        r.kernel_version = Some(v.clone());
    }
    if let Some(o) = &args.output {
        match o.as_str() {
            "image" | "deb" | "rpm" => r.output = Some(o.clone()),
            other => return Err(format!("unknown output format {other:?}")),
        }
    }
    if args.clang {
        r.llvm = true;
    }
    if let Some(n) = &args.name {
        r.name = recipe::clean_name(n)
            .ok_or("--name: use letters, digits, spaces, dashes or underscores")?;
    }
    Ok(r)
}

fn cmd_fragment(args: &Args) -> Result<(), String> {
    let r = selection(args)?;
    let resolved = software::resolve(&r.selections, &r.software, &r.extra);
    for n in &resolved.notes {
        eprintln!("# {}: {}", n.level, n.text);
    }
    print!("{}", catalog::fragment_text(&resolved.fragment));
    Ok(())
}

// ---------------------------------------------------------------- .kernel files

fn cmd_show(args: &Args) -> Result<(), String> {
    let path = args
        .path
        .as_ref()
        .or(args.file.as_ref())
        .ok_or("which file? usage: kertweaker show FILE.kernel")?;
    let (r, _) = load_recipe(path)?;
    for line in r.summary() {
        println!("{line}");
    }
    let resolved = software::resolve(&r.selections, &r.software, &r.extra);
    println!(
        "  config     {} options set, risk: {}",
        resolved.fragment.len(),
        resolved.risk.as_str()
    );
    let family = system::package_family();
    let plan = software::plan(&r.software, &r.packages, family, &|p| {
        software::available(family, p)
    });
    if !plan.commands.is_empty() {
        println!();
        println!("To install its software on this machine:");
        for c in &plan.commands {
            println!("  {c}");
        }
    }
    for n in &plan.notes {
        println!("  note: {n}");
    }
    println!();
    println!(
        "Build it with: kertweaker build --file {}{}",
        path.display(),
        if r.kernel_version.is_some() {
            ""
        } else {
            " --kernel VERSION   (or --tree PATH)"
        }
    );
    Ok(())
}

fn cmd_export(args: &Args) -> Result<(), String> {
    let out = args
        .out
        .as_ref()
        .ok_or("where to? usage: kertweaker export -o FILE.kernel ...")?;
    let mut r = selection(args)?;

    // --base decides what the recipe starts from.
    match args.base.as_deref() {
        None => {}
        Some("running") => r.base = recipe::Base::Running,
        Some("defconfig") => r.base = recipe::Base::Defconfig,
        Some("tree") => return Err("--base tree cannot be exported; pass the config's path".into()),
        Some(path) => {
            let cfg = kconfig::Config::load(std::path::Path::new(path))
                .map_err(|e| format!("reading {path}: {e}"))?;
            if cfg.is_empty() {
                return Err(format!("{path} has no CONFIG_ options in it"));
            }
            r.base = recipe::Base::Embedded(cfg.render());
        }
    }
    if args.embed_config && r.base == recipe::Base::Running {
        let path = system::running_config_path()
            .ok_or("this machine does not expose its running kernel's config to embed")?;
        let cfg = kconfig::Config::load(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        r.base = recipe::Base::Embedded(cfg.render());
    }
    if args.name.is_none() {
        // Name the recipe after its file unless told otherwise.
        if let Some(stem) = out.file_stem().and_then(|s| s.to_str()) {
            if let Some(n) = recipe::clean_name(stem) {
                r.name = n;
            }
        }
    }

    std::fs::write(out, r.render()).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    println!("Wrote {}", out.display());
    for line in r.summary() {
        println!("{line}");
    }
    Ok(())
}

// ---------------------------------------------------------------------- build

fn cmd_build(args: &Args) -> Result<(), String> {
    let r = selection(args)?;

    let source = match (&args.tree, &r.kernel_version) {
        (Some(p), _) => Source::Local(p.clone()),
        (None, Some(v)) => Source::Download(v.clone()),
        (None, None) => {
            return Err(if args.file.is_some() {
                "this .kernel file does not pin a kernel version; pass --tree PATH or --kernel VERSION"
                    .into()
            } else {
                "pass --file FILE.kernel, --tree PATH or --kernel VERSION".into()
            })
        }
    };
    // An explicit --base wins; otherwise the recipe's own starting point.
    let base = match args.base.as_deref() {
        Some("running") => Base::Running,
        Some("defconfig") => Base::Defconfig,
        Some("tree") => Base::Tree,
        Some(path) => Base::File(PathBuf::from(path)),
        None => match &r.base {
            recipe::Base::Running => Base::Running,
            recipe::Base::Defconfig => Base::Defconfig,
            recipe::Base::Embedded(text) => Base::Text(text.clone()),
        },
    };

    let resolved = software::resolve(&r.selections, &r.software, &r.extra);
    for n in &resolved.notes {
        eprintln!("{}: {}", n.level, n.text);
    }
    if resolved.risk == catalog::Risk::Risky && !args.yes {
        return Err(
            "this selection includes risky options; re-run with --yes to accept them".into(),
        );
    }

    let workdir = args.workdir();
    let req = Request {
        source,
        base,
        selections: r.selections.clone(),
        extra: r.extra.clone(),
        software: r.software.clone(),
        packages: r.packages.clone(),
        jobs: args
            .jobs
            .unwrap_or_else(system::suggested_jobs)
            .clamp(1, 256),
        output: Output::parse(r.output.as_deref().unwrap_or("image")),
        llvm: r.llvm,
        clean: args.clean,
        install: args.install,
        config_only: args.config_only,
        skip_tool_check: args.skip_tool_check,
        workdir,
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
        println!(
            "  {:<8} {:<38} {}",
            format!("{:?}", s.state).to_lowercase(),
            s.title,
            s.detail
        );
    }
    if !st.unapplied.is_empty() {
        println!();
        println!(
            "{} requested option(s) did not stick - see the warnings above.",
            st.unapplied.len()
        );
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
    if st.status == Status::Done && !st.software.commands.is_empty() {
        println!();
        println!("Then install the software that goes with it:");
        for c in &st.software.commands {
            println!("  {c}");
        }
    }
    match st.status {
        Status::Done => Ok(()),
        Status::Cancelled => Err("build cancelled".into()),
        _ => Err(st.error.clone().unwrap_or_else(|| "build failed".into())),
    }
}
