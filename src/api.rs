//! The HTTP API behind the GUI, plus authentication.
//!
//! Security model: the server binds to loopback only, checks the `Host`
//! header (which defeats DNS rebinding), and requires a random per-run token.
//! The token arrives once in the URL the CLI prints, is swapped for an
//! `HttpOnly; SameSite=Strict` cookie, and is never embedded in a page - so
//! neither a hostile web page nor another local user can start a build.

use crate::build::{self, Base, Output, Registry, Source};
use crate::catalog;
use crate::http::{self, Reply, Request, Response};
use crate::json::{self, Json};
use crate::kconfig::{self, Config};
use crate::recipe::{self, Recipe};
use crate::{software, sources, system};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const INDEX_HTML: &str = include_str!("web/index.html");
const APP_JS: &str = include_str!("web/app.js");
const STYLE_CSS: &str = include_str!("web/style.css");
const LOGIN_HTML: &str = include_str!("web/login.html");

/// Cap on diff rows sent to the browser; the count is always exact.
const MAX_DIFF_ROWS: usize = 500;

pub struct App {
    pub workdir: PathBuf,
    pub token: String,
    pub port: u16,
    pub registry: Registry,
    /// A `.kernel` file given on the command line, loaded into the page at start.
    pub preload: Option<(Recipe, Vec<String>)>,
}

impl App {
    pub fn new(workdir: PathBuf, port: u16) -> App {
        App {
            workdir,
            token: new_token(),
            port,
            registry: Registry::new(),
            preload: None,
        }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/?token={}", self.port, self.token)
    }
}

/// 128 bits from the kernel's CSPRNG, hex encoded.
pub fn new_token() -> String {
    use std::io::Read;
    let mut buf = [0u8; 16];
    let ok = fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok();
    if !ok {
        // Should be unreachable on Linux; still never fall back to a constant.
        let seed = build::now_secs().to_bits() ^ ((std::process::id() as u64) << 32);
        let mut x = seed | 1;
        for b in buf.iter_mut() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = (x & 0xff) as u8;
        }
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn host_allowed(req: &Request, port: u16) -> bool {
    let Some(host) = req.header("host") else {
        return false;
    };
    [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ]
    .iter()
    .any(|h| h == host)
}

fn origin_allowed(req: &Request, port: u16) -> bool {
    match req.header("origin") {
        None => true, // non-browser client; the token still gates it
        Some(origin) => [
            format!("http://127.0.0.1:{port}"),
            format!("http://localhost:{port}"),
            format!("http://[::1]:{port}"),
        ]
        .iter()
        .any(|o| o == origin),
    }
}

fn authed(req: &Request, token: &str) -> bool {
    req.cookie("kt").is_some_and(|c| constant_time_eq(c, token))
        || req
            .header("x-kertweaker-token")
            .is_some_and(|h| constant_time_eq(h, token))
}

fn ok(value: Json) -> Reply {
    Reply::Full(Response::json(200, &value))
}

fn fail(status: u16, message: impl AsRef<str>) -> Reply {
    Reply::Full(Response::error(status, message.as_ref()))
}

/// The single entry point: every request is routed here.
pub fn handle(app: &Arc<App>, req: &Request) -> Reply {
    if !host_allowed(req, app.port) {
        return fail(403, "unexpected Host header");
    }
    let method = req.method.as_str();
    let path = req.path.as_str();

    // ---------------------------------------------------- static + sign-in
    match (method, path) {
        ("GET", "/") => {
            if let Some(t) = req.query.get("token") {
                if constant_time_eq(t, &app.token) {
                    // Swap the URL token for a cookie and drop it from the address bar.
                    return Reply::Full(Response::redirect("/").with_header(
                        "Set-Cookie",
                        &format!("kt={}; HttpOnly; SameSite=Strict; Path=/", app.token),
                    ));
                }
            }
            let page = if authed(req, &app.token) {
                INDEX_HTML
            } else {
                LOGIN_HTML
            };
            return Reply::Full(Response::new(200, "text/html; charset=utf-8", page));
        }
        ("GET", "/app.js") => {
            return Reply::Full(Response::new(200, "text/javascript; charset=utf-8", APP_JS))
        }
        ("GET", "/style.css") => {
            return Reply::Full(Response::new(200, "text/css; charset=utf-8", STYLE_CSS))
        }
        ("GET", "/favicon.ico") => return Reply::Full(Response::new(404, "text/plain", "")),
        _ => {}
    }

    if !path.starts_with("/api/") {
        return fail(404, "not found");
    }
    if !authed(req, &app.token) {
        return fail(
            401,
            "missing or wrong token - open the URL kertweaker printed",
        );
    }
    if method == "POST" && !origin_allowed(req, app.port) {
        return fail(403, "cross-origin request refused");
    }

    let segments: Vec<&str> = path.trim_start_matches("/api/").split('/').collect();
    match (method, segments.as_slice()) {
        ("GET", ["system"]) => ok(system::describe(&app.workdir)),
        ("GET", ["catalog"]) => {
            let mut doc = catalog::to_json();
            if let Json::Obj(pairs) = &mut doc {
                pairs.push((
                    "software".to_string(),
                    software::to_json(system::package_family()),
                ));
            }
            ok(doc)
        }
        ("GET", ["preload"]) => ok(match &app.preload {
            Some((r, warnings)) => recipe::to_state_json(r, warnings),
            None => Json::Null,
        }),
        ("POST", ["kernelfile", "import"]) => with_body(req, import_kernel_file),
        ("POST", ["kernelfile", "export"]) => with_body(req, export_kernel_file),
        ("GET", ["releases"]) => ok(sources::releases(&app.workdir)),
        ("GET", ["trees"]) => ok(Json::arr(sources::local_trees(
            &sources::default_search_dirs(&app.workdir),
        ))),
        ("POST", ["preview"]) => with_body(req, |body| preview(app, body)),
        ("POST", ["build"]) => with_body(req, |body| start_build(app, body)),
        ("GET", ["jobs"]) => ok(Json::arr(
            app.registry
                .all()
                .iter()
                .map(|j| {
                    let st = j.state.lock().expect("job state poisoned");
                    Json::obj(vec![
                        ("id", Json::s(j.id.clone())),
                        (
                            "status",
                            Json::s(if st.status.finished() {
                                "finished"
                            } else {
                                "running"
                            }),
                        ),
                        ("started", Json::n(st.started)),
                    ])
                })
                .collect(),
        )),
        ("GET", ["jobs", id]) => match app.registry.get(id) {
            Some(job) => {
                let from = req.query.get("from").and_then(|v| v.parse().ok());
                ok(job.to_json(Some(from.unwrap_or(0))))
            }
            None => fail(404, "no such build"),
        },
        ("GET", ["jobs", id, "events"]) => match app.registry.get(id) {
            Some(job) => {
                let from = req
                    .query
                    .get("from")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                Reply::Stream(Box::new(move |stream| stream_job(job, from, stream)))
            }
            None => fail(404, "no such build"),
        },
        ("GET", ["jobs", id, "config"]) => match app.registry.get(id) {
            Some(job) => {
                let path = job.state.lock().ok().and_then(|s| s.config_path.clone());
                match path.and_then(|p| fs::read(p).ok()) {
                    Some(bytes) => Reply::Full(Response::text(200, bytes).with_header(
                        "Content-Disposition",
                        "attachment; filename=\"kertweaker.config\"",
                    )),
                    None => fail(404, "this build has not written a config yet"),
                }
            }
            None => fail(404, "no such build"),
        },
        ("POST", ["jobs", id, "cancel"]) => match app.registry.get(id) {
            Some(job) => {
                job.request_cancel();
                ok(Json::obj(vec![("cancelled", Json::b(true))]))
            }
            None => fail(404, "no such build"),
        },
        ("GET", ["saved"]) => ok(list_saved(&app.workdir)),
        ("POST", ["saved"]) => with_body(req, |body| save_profile(&app.workdir, body)),
        ("POST", ["saved", "delete"]) => with_body(req, |body| delete_profile(&app.workdir, body)),
        _ => fail(404, "no such endpoint"),
    }
}

fn with_body(req: &Request, f: impl FnOnce(&Json) -> Reply) -> Reply {
    match json::parse(&req.body_text()) {
        Ok(body) => f(&body),
        Err(e) => fail(400, format!("invalid JSON body: {e}")),
    }
}

// --------------------------------------------------------------------- preview

fn parse_base(body: &Json) -> Result<Base, String> {
    let base = body.get("base").cloned().unwrap_or(Json::Null);
    match base.str_or("mode", "running") {
        "running" => Ok(Base::Running),
        "defconfig" => Ok(Base::Defconfig),
        "tree" => Ok(Base::Tree),
        "file" => {
            let p = base.str_or("path", "").trim();
            if p.is_empty() {
                Err("choose a config file to start from".into())
            } else {
                Ok(Base::File(expand(p)))
            }
        }
        "text" => {
            let t = base.str_or("text", "");
            if t.trim().is_empty() {
                Err("paste a config to start from".into())
            } else {
                Ok(Base::Text(t.to_string()))
            }
        }
        other => Err(format!("unknown starting config {other:?}")),
    }
}

fn parse_source(body: &Json) -> Result<Source, String> {
    let source = body.get("source").cloned().unwrap_or(Json::Null);
    match source.str_or("mode", "") {
        "local" => {
            let p = source.str_or("path", "").trim();
            if p.is_empty() {
                return Err("enter the path of a kernel source tree".into());
            }
            Ok(Source::Local(expand(p)))
        }
        "download" => {
            let v = source.str_or("version", "").trim();
            if !sources::valid_version(v) {
                return Err(format!("{v:?} is not a valid kernel version"));
            }
            Ok(Source::Download(v.to_string()))
        }
        _ => Err("choose a kernel source first".into()),
    }
}

/// Expand a leading `~/`, since that is what people type into a path box.
fn expand(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(p)
}

/// Load the base config for a preview, without running any kbuild step.
fn load_base(base: &Base, tree: Option<&Path>) -> Result<(Config, String), String> {
    match base {
        Base::Running => {
            let p = system::running_config_path()
                .ok_or("this machine does not expose the running kernel's config")?;
            let c = Config::load(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            Ok((c, format!("running kernel ({})", p.display())))
        }
        Base::File(p) => {
            let c = Config::load(p).map_err(|e| format!("{}: {e}", p.display()))?;
            if c.is_empty() {
                return Err(format!("{} has no CONFIG_ symbols in it", p.display()));
            }
            Ok((c, p.display().to_string()))
        }
        Base::Text(t) => {
            let c = Config::parse(t);
            if c.is_empty() {
                return Err("the pasted text has no CONFIG_ symbols in it".into());
            }
            Ok((c, "pasted config".into()))
        }
        Base::Tree => {
            let tree = tree.ok_or("choose a local kernel tree to use its .config")?;
            let p = tree.join(".config");
            let c = Config::load(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            Ok((c, p.display().to_string()))
        }
        Base::Defconfig => Err(
            "defconfig is generated by kbuild when the build starts, so there is nothing to \
             compare against yet"
                .into(),
        ),
    }
}

fn preview(_app: &Arc<App>, body: &Json) -> Reply {
    let selections = body.string_map("selections");
    let extra = body.str_or("extra", "");
    let apps = body.str_list("software");
    let (packages, bad_packages) = software::parse_packages(&body.str_list("packages").join(" "));
    let mut resolved = software::resolve(&selections, &apps, extra);
    // Tell the user now, not after a long build, that a tool is missing.
    let missing = resolved.missing_tool_notes(|tool| system::which(tool).is_some());
    resolved.notes.extend(missing);
    for name in &bad_packages {
        resolved.notes.push(catalog::Note {
            tweak: "packages".into(),
            level: "error".into(),
            text: format!("{name:?} is not a valid package name, so it is ignored."),
        });
    }
    let family = system::package_family();
    let plan = software::plan(&apps, &packages, family, &|p| {
        software::available(family, p)
    });

    let tree = match body.get("source") {
        Some(s) if s.str_or("mode", "") == "local" && !s.str_or("path", "").trim().is_empty() => {
            Some(expand(s.str_or("path", "").trim()))
        }
        _ => None,
    };

    let loaded = parse_base(body).and_then(|b| load_base(&b, tree.as_deref()));
    if let Ok((base, _)) = &loaded {
        // A monolithic base turns every "=m" request into "=y". Say so up
        // front, rather than leaving it as a surprise in the build report.
        let asks_for_modules = resolved
            .fragment
            .iter()
            .any(|(_, v)| v.as_deref() == Some("m"));
        let turns_modules_on = resolved
            .fragment
            .iter()
            .any(|(s, v)| s == "CONFIG_MODULES" && v.as_deref() == Some("y"));
        if asks_for_modules && !base.enabled("CONFIG_MODULES") && !turns_modules_on {
            resolved.notes.push(catalog::Note {
                tweak: "modules".into(),
                level: "caution".into(),
                text: "The starting config has loadable modules switched off, so everything \
                       requested as a module will be built into the kernel image instead. Turn on \
                       'Loadable module support' under Build size & speed to change that."
                    .into(),
            });
        }
    }

    let mut pairs = vec![
        ("resolved", resolved.to_json()),
        ("software_plan", plan.to_json()),
    ];
    match loaded {
        Ok((base, description)) => {
            let mut next = base.clone();
            next.apply(&resolved.fragment);
            let changes = kconfig::diff(&base, &next);
            let unchanged = resolved.fragment.len().saturating_sub(changes.len());
            pairs.push((
                "base",
                Json::obj(vec![
                    ("available", Json::b(true)),
                    ("description", Json::s(description)),
                    ("symbols", Json::u(base.len())),
                    ("enabled", Json::u(base.enabled_count())),
                    ("modules", Json::u(base.module_count())),
                ]),
            ));
            pairs.push(("change_count", Json::u(changes.len())));
            pairs.push(("already_set", Json::u(unchanged)));
            pairs.push((
                "changes",
                Json::arr(
                    changes
                        .iter()
                        .take(MAX_DIFF_ROWS)
                        .map(|c| {
                            Json::obj(vec![
                                ("symbol", Json::s(c.symbol.clone())),
                                ("kind", Json::s(c.kind().as_str())),
                                ("before", Json::opt_s(c.before_line())),
                                ("after", Json::s(c.after_line())),
                                (
                                    "tweak",
                                    Json::s(
                                        resolved.origin.get(&c.symbol).cloned().unwrap_or_default(),
                                    ),
                                ),
                            ])
                        })
                        .collect(),
                ),
            ));
        }
        Err(e) => {
            pairs.push((
                "base",
                Json::obj(vec![("available", Json::b(false)), ("error", Json::s(e))]),
            ));
        }
    }
    ok(Json::obj(pairs))
}

// ----------------------------------------------------------------------- build

fn start_build(app: &Arc<App>, body: &Json) -> Reply {
    if app.registry.busy() {
        return fail(
            409,
            "a build is already running - cancel it or wait for it to finish",
        );
    }
    let source = match parse_source(body) {
        Ok(s) => s,
        Err(e) => return fail(400, e),
    };
    let base = match parse_base(body) {
        Ok(b) => b,
        Err(e) => return fail(400, e),
    };
    if let Source::Local(p) = &source {
        if !sources::is_kernel_tree(p) {
            return fail(
                400,
                format!("{} is not a Linux kernel source tree", p.display()),
            );
        }
    }
    if matches!(base, Base::Tree) && !matches!(source, Source::Local(_)) {
        return fail(
            400,
            "a freshly downloaded tree has no .config of its own to start from",
        );
    }

    let selections = body.string_map("selections");
    let extra = body.str_or("extra", "").to_string();
    let apps = body.str_list("software");
    let (packages, bad_packages) = software::parse_packages(&body.str_list("packages").join(" "));
    if !bad_packages.is_empty() {
        return fail(
            400,
            format!("not valid package names: {}", bad_packages.join(", ")),
        );
    }
    let resolved = software::resolve(&selections, &apps, &extra);
    if !resolved.unknown.is_empty() {
        return fail(
            400,
            format!(
                "unknown tweaks or software: {}",
                resolved.unknown.join(", ")
            ),
        );
    }
    // Options that can cost someone their bootable machine need an explicit yes.
    if resolved.risk == catalog::Risk::Risky && !body.bool_or("accept_risk", false) {
        return fail(
            400,
            "this selection includes risky options - confirm that you accept the risk",
        );
    }

    let jobs = body
        .get("jobs")
        .and_then(Json::as_usize)
        .unwrap_or_else(system::suggested_jobs)
        .clamp(1, 256);

    let req = build::Request {
        source,
        base,
        selections,
        extra,
        software: apps,
        packages,
        jobs,
        output: Output::parse(body.str_or("output", "image")),
        llvm: body.bool_or("llvm", false),
        clean: body.bool_or("clean", false),
        install: body.bool_or("install", false),
        config_only: body.bool_or("config_only", false),
        skip_tool_check: false,
        workdir: app.workdir.clone(),
        make: build::Request::make_cmd(),
    };
    let job = app.registry.start(req);
    ok(Json::obj(vec![("id", Json::s(job.id.clone()))]))
}

// ---------------------------------------------------------------- .kernel files

/// Read a `.kernel` file's text and hand back what the page should load.
/// Nothing is built or installed; the user still reviews and presses Build.
fn import_kernel_file(body: &Json) -> Reply {
    match recipe::parse(body.str_or("text", "")) {
        Ok((r, warnings)) => ok(recipe::to_state_json(&r, &warnings)),
        Err(e) => fail(400, e),
    }
}

/// Turn the page's current state into the text of a `.kernel` file.
fn export_kernel_file(body: &Json) -> Reply {
    let Some(name) = recipe::clean_name(body.str_or("name", "my-kernel")) else {
        return fail(
            400,
            "use a name of letters, digits, spaces, dashes or underscores",
        );
    };
    let (packages, bad) = software::parse_packages(&body.str_list("packages").join(" "));
    if !bad.is_empty() {
        return fail(400, format!("not valid package names: {}", bad.join(", ")));
    }
    let apps: Vec<String> = body
        .str_list("software")
        .into_iter()
        .filter(|id| software::app(id).is_some())
        .collect();

    // What the recipe starts from. A config that only exists on this machine
    // (a pasted one, a file, a tree's .config) has to travel inside the file,
    // or the recipe would mean nothing anywhere else.
    let embed = body.bool_or("embed", false);
    let tree = match body.get("source") {
        Some(s) if s.str_or("mode", "") == "local" && !s.str_or("path", "").trim().is_empty() => {
            Some(expand(s.str_or("path", "").trim()))
        }
        _ => None,
    };
    let base = match parse_base(body) {
        Ok(Base::Defconfig) => recipe::Base::Defconfig,
        Ok(Base::Running) if !embed => recipe::Base::Running,
        Ok(b) => match load_base(&b, tree.as_deref()) {
            Ok((config, _)) => recipe::Base::Embedded(config.render()),
            Err(e) => return fail(400, format!("cannot read the starting config: {e}")),
        },
        Err(e) => return fail(400, e),
    };
    let embedded = matches!(base, recipe::Base::Embedded(_));

    let version = body
        .get("source")
        .filter(|s| s.str_or("mode", "") == "download")
        .map(|s| s.str_or("version", "").to_string())
        .filter(|v| sources::valid_version(v));
    let selections: std::collections::BTreeMap<String, String> = body
        .string_map("selections")
        .into_iter()
        .filter(|(id, _)| catalog::tweak(id).is_some())
        .collect();
    let extra = catalog::fragment_text_ordered(&kconfig::parse_fragment(body.str_or("extra", "")));

    let r = Recipe {
        name,
        description: body.str_or("description", "").chars().take(300).collect(),
        kernel_version: version,
        base,
        selections,
        extra,
        software: apps,
        packages,
        output: match body.str_or("output", "") {
            o @ ("image" | "deb" | "rpm") => Some(o.to_string()),
            _ => None,
        },
        llvm: body.bool_or("llvm", false),
    };
    ok(Json::obj(vec![
        ("filename", Json::s(r.file_name())),
        ("text", Json::s(r.render())),
        ("embedded", Json::b(embedded)),
    ]))
}

/// Push job snapshots over SSE until the job finishes or the client leaves.
fn stream_job(job: Arc<build::Job>, from: usize, mut stream: std::net::TcpStream) {
    // Events can be minutes apart during a long link step.
    let _ = stream.set_write_timeout(Some(Duration::from_secs(20)));
    if http::write_sse_head(&mut stream).is_err() {
        return;
    }
    let mut cursor = from;
    let mut last_rev = u64::MAX;
    let mut last_write = Instant::now();
    loop {
        // Read the revision *before* the snapshot, so a change landing between
        // the two is picked up next time round instead of being lost.
        let rev = job.revision.load(Ordering::Relaxed);
        if rev != last_rev {
            let snapshot = job.to_json(Some(cursor));
            cursor = snapshot
                .get("log_total")
                .and_then(Json::as_usize)
                .unwrap_or(cursor);
            let finished = matches!(
                snapshot.str_or("status", ""),
                "done" | "failed" | "cancelled"
            );
            if http::write_sse_event(&mut stream, "update", &snapshot.to_string()).is_err() {
                return;
            }
            last_rev = rev;
            last_write = Instant::now();
            if finished {
                let _ = http::write_sse_event(&mut stream, "end", "{}");
                return;
            }
        } else if last_write.elapsed() > Duration::from_secs(10) {
            // Keep-alive, and how we notice a closed tab.
            if http::write_sse_event(&mut stream, "ping", "{}").is_err() {
                return;
            }
            last_write = Instant::now();
        }
        thread::sleep(Duration::from_millis(200));
    }
}

// --------------------------------------------------------------- saved profiles

fn saved_dir(workdir: &Path) -> PathBuf {
    workdir.join("profiles")
}

/// Names become filenames, so they are restricted to a safe alphabet.
fn safe_name(name: &str) -> Option<String> {
    let n = name.trim();
    let ok = !n.is_empty()
        && n.len() <= 48
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.'))
        && !n.starts_with('.');
    ok.then(|| n.to_string())
}

fn list_saved(workdir: &Path) -> Json {
    let mut items = Vec::new();
    if let Ok(entries) = fs::read_dir(saved_dir(workdir)) {
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();
        for p in paths {
            if let Some(j) = fs::read_to_string(&p)
                .ok()
                .and_then(|t| json::parse(&t).ok())
            {
                items.push(j);
            }
        }
    }
    Json::arr(items)
}

fn save_profile(workdir: &Path, body: &Json) -> Reply {
    let Some(name) = safe_name(body.str_or("name", "")) else {
        return fail(
            400,
            "use a name of letters, digits, spaces, dashes or underscores",
        );
    };
    let selections = body.string_map("selections");
    let doc = Json::obj(vec![
        ("name", Json::s(name.clone())),
        ("saved_at", Json::n(build::now_secs())),
        ("extra", Json::s(body.str_or("extra", ""))),
        ("software", Json::strs(body.str_list("software"))),
        ("packages", Json::strs(body.str_list("packages"))),
        (
            "selections",
            Json::Obj(
                selections
                    .into_iter()
                    .map(|(k, v)| (k, Json::s(v)))
                    .collect(),
            ),
        ),
    ]);
    let dir = saved_dir(workdir);
    if let Err(e) = fs::create_dir_all(&dir) {
        return fail(500, format!("cannot create {}: {e}", dir.display()));
    }
    match fs::write(dir.join(format!("{name}.json")), doc.to_string()) {
        Ok(()) => ok(doc),
        Err(e) => fail(500, format!("cannot save profile: {e}")),
    }
}

fn delete_profile(workdir: &Path, body: &Json) -> Reply {
    let Some(name) = safe_name(body.str_or("name", "")) else {
        return fail(400, "bad profile name");
    };
    let _ = fs::remove_file(saved_dir(workdir).join(format!("{name}.json")));
    ok(Json::obj(vec![("deleted", Json::s(name))]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_hex() {
        let a = new_token();
        let b = new_token();
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn constant_time_eq_behaves_like_eq() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn profile_names_cannot_escape_the_directory() {
        assert_eq!(safe_name("my desktop-2").as_deref(), Some("my desktop-2"));
        for bad in [
            "",
            "../etc/passwd",
            "a/b",
            ".hidden",
            "x\0y",
            &"n".repeat(60),
        ] {
            assert!(safe_name(bad).is_none(), "{bad:?} must be rejected");
        }
    }
}
