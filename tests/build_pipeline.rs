//! End-to-end coverage of the build pipeline against a stub kbuild.

mod support;

use kertweaker::build::{self, Base, Output, Request, Source, StageState, Status};
use kertweaker::kconfig::Config;
use std::collections::BTreeMap;
use std::time::Duration;
use support::{fixture, selections, Fixture};

fn request(f: &Fixture, sel: BTreeMap<String, String>) -> Request {
    Request {
        source: Source::Local(f.tree.clone()),
        base: Base::Defconfig,
        selections: sel,
        extra: String::new(),
        jobs: 2,
        output: Output::Image,
        llvm: false,
        clean: false,
        install: false,
        config_only: false,
        // The host running these tests need not have a kernel toolchain.
        skip_tool_check: true,
        workdir: f.workdir.clone(),
        make: f.make.to_string_lossy().into_owned(),
    }
}

fn run(req: Request) -> std::sync::Arc<build::Job> {
    let job = build::new_job("test-job".into(), req);
    build::run(job.clone());
    job
}

fn stage(job: &build::Job, id: &str) -> build::Stage {
    job.state
        .lock()
        .unwrap()
        .stages
        .iter()
        .find(|s| s.id == id)
        .cloned()
        .unwrap_or_else(|| panic!("no stage {id}"))
}

fn stage_of(st: &build::JobState, id: &str) -> build::Stage {
    st.stages
        .iter()
        .find(|s| s.id == id)
        .cloned()
        .unwrap_or_else(|| panic!("no stage {id}"))
}

fn log(job: &build::Job) -> String {
    job.state.lock().unwrap().log.join("\n")
}

#[test]
fn full_build_applies_tweaks_and_produces_artifacts() {
    let f = fixture("6.12.3");
    let job = run(request(
        &f,
        selections(&[("preempt", "full"), ("hz", "1000"), ("localversion", "-kertweaker")]),
    ));

    let st = job.state.lock().unwrap();
    assert_eq!(st.status, Status::Done, "error: {:?}\n{}", st.error, st.log.join("\n"));
    drop(st);

    // Every stage either ran or was deliberately skipped.
    for id in ["source", "deps", "base", "tweaks", "resolve", "verify", "build", "collect"] {
        assert_eq!(stage(&job, id).state, StageState::Done, "stage {id}");
    }
    assert_eq!(stage(&job, "trim").state, StageState::Skipped);
    assert_eq!(stage(&job, "install").state, StageState::Skipped);
    assert!(stage(&job, "source").detail.contains("6.12.3"));

    // The tweaks really landed in the tree's .config.
    let cfg = Config::load(&f.tree.join(".config")).unwrap();
    assert_eq!(cfg.get("CONFIG_PREEMPT"), Some("y"));
    assert!(!cfg.enabled("CONFIG_PREEMPT_NONE"));
    assert_eq!(cfg.get("CONFIG_HZ"), Some("1000"));
    assert_eq!(cfg.string("CONFIG_LOCALVERSION").as_deref(), Some("-kertweaker"));

    // Progress accounting and artifacts.
    let st = job.state.lock().unwrap();
    assert_eq!(st.objects, 13, "12 CC lines plus one OBJCOPY");
    assert!(st.diff_count > 0);
    let names: Vec<&str> = st.artifacts.iter().map(|(p, _)| p.as_str()).collect();
    assert!(names.iter().any(|p| p.ends_with("bzImage")), "{names:?}");
    assert!(names.iter().any(|p| p.ends_with("System.map")), "{names:?}");
    assert!(names.iter().any(|p| p.ends_with(".config")), "{names:?}");
    assert!(st.next_steps.iter().any(|s| s.contains("modules_install")));

    // A copy of the final config is kept outside the tree.
    let saved = st.config_path.clone().unwrap();
    assert!(saved.exists());
    assert!(Config::load(&saved).unwrap().enabled("CONFIG_PREEMPT"));

    // The full log is on disk as well as in memory.
    let disk = std::fs::read_to_string(&job.log_path).unwrap();
    assert!(disk.contains("CC      kernel/sched/obj1.o"));
}

#[test]
fn verify_reports_options_that_did_not_stick() {
    let f = fixture("6.12.3");
    let mut req = request(&f, selections(&[("preempt", "full")]));
    // One symbol this kernel does not have, one the dependency solver forces off.
    req.extra = "CONFIG_NOT_IN_THIS_KERNEL=y\nCONFIG_FORCED_OFF=y\n".into();
    let job = run(req);

    let st = job.state.lock().unwrap();
    assert_eq!(st.status, Status::Done);
    let symbols: Vec<&str> = st.unapplied.iter().map(|u| u.symbol.as_str()).collect();
    assert_eq!(symbols, vec!["CONFIG_FORCED_OFF", "CONFIG_NOT_IN_THIS_KERNEL"]);

    let forced = &st.unapplied[0];
    assert!(forced.present, "kbuild kept the symbol but changed its value");
    assert_eq!(forced.actual, None);
    let missing = &st.unapplied[1];
    assert!(!missing.present, "symbol does not exist in this kernel at all");

    // The user is told, in the log and in the stage detail.
    drop(st);
    assert!(log(&job).contains("warning: wanted CONFIG_FORCED_OFF=y"));
    assert!(stage(&job, "verify").detail.contains("did not stick"));
}

#[test]
fn localmodconfig_runs_when_the_profile_asks_for_it() {
    let f = fixture("6.12.3");
    let job = run(request(&f, selections(&[("localmodconfig_hint", "on")])));
    assert_eq!(job.state.lock().unwrap().status, Status::Done);
    assert_eq!(stage(&job, "trim").state, StageState::Done);
    assert!(log(&job).contains("using config"));
    // The stub turns i915 off during the trim, and nothing turns it back on.
    let cfg = Config::load(&f.tree.join(".config")).unwrap();
    assert!(!cfg.enabled("CONFIG_DRM_I915"));
}

#[test]
fn config_only_stops_before_compiling() {
    let f = fixture("6.12.3");
    let mut req = request(&f, selections(&[("hz", "300")]));
    req.config_only = true;
    let job = run(req);

    let st = job.state.lock().unwrap();
    assert_eq!(st.status, Status::Done);
    assert!(!st.stages.iter().any(|s| s.id == "build"));
    assert_eq!(st.objects, 0);
    drop(st);
    assert_eq!(stage(&job, "resolve").state, StageState::Done);
    assert!(!f.tree.join("arch/x86_64/boot/bzImage").exists());
}

#[test]
fn a_failing_build_reports_the_failure_and_stops() {
    let f = fixture("6.12.3");
    let mut req = request(&f, selections(&[]));
    req.output = Output::Tar; // the stub has no tarxz-pkg rule, so make fails
    let job = run(req);

    let st = job.state.lock().unwrap();
    assert_eq!(st.status, Status::Failed);
    let err = st.error.clone().unwrap();
    assert!(err.contains("exit 2"), "{err}");
    drop(st);
    assert_eq!(stage(&job, "build").state, StageState::Failed);
    // Stages after the failure are not left looking pending.
    assert_eq!(stage(&job, "collect").state, StageState::Skipped);
    assert!(log(&job).contains("no rule to make target"));
}

#[test]
fn missing_source_tree_fails_before_anything_else() {
    let f = fixture("6.12.3");
    let mut req = request(&f, selections(&[]));
    req.source = Source::Local(f.root.join("not-a-kernel"));
    let job = run(req);
    let st = job.state.lock().unwrap();
    assert_eq!(st.status, Status::Failed);
    assert!(st.error.clone().unwrap().contains("not a Linux kernel source tree"));
    drop(st);
    assert_eq!(stage(&job, "base").state, StageState::Skipped);
}

#[test]
fn an_invalid_version_never_reaches_the_network() {
    let f = fixture("6.12.3");
    let mut req = request(&f, selections(&[]));
    req.source = Source::Download("6.12; rm -rf /".into());
    let job = run(req);
    let st = job.state.lock().unwrap();
    assert_eq!(st.status, Status::Failed);
    assert!(st.error.clone().unwrap().contains("not a valid kernel version"));
}

#[test]
fn cancelling_stops_the_running_make() {
    let f = fixture("6.12.3");
    let req = request(&f, selections(&[]));
    let job = build::new_job("cancel-job".into(), req);

    f.make_compile_slow();
    let runner = job.clone();
    let handle = std::thread::spawn(move || build::run(runner));

    // Wait until the compile is actually under way, then cancel it.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let started = job
            .state
            .lock()
            .unwrap()
            .stages
            .iter()
            .any(|s| s.id == "build" && s.state == StageState::Running);
        if started && log(&job).contains("slow.o") {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "build never started");
        std::thread::sleep(Duration::from_millis(20));
    }

    let t0 = std::time::Instant::now();
    job.request_cancel();
    handle.join().unwrap();

    // Cancelling must be prompt: if only the top process were killed, the
    // sleeping child would hold the pipe open for its full 120 seconds.
    assert!(t0.elapsed() < Duration::from_secs(15), "cancel took {:?}", t0.elapsed());
    let st = job.state.lock().unwrap();
    assert_eq!(st.status, Status::Cancelled);
    assert!(st.error.is_none(), "cancelling is not an error: {:?}", st.error);
    assert_eq!(stage_of(&st, "build").state, StageState::Skipped);
}

#[test]
fn package_output_collects_the_debs_and_tells_you_how_to_install() {
    let f = fixture("6.12.3");
    let mut req = request(&f, selections(&[]));
    req.output = Output::Deb;
    let job = run(req);

    let st = job.state.lock().unwrap();
    assert_eq!(st.status, Status::Done, "{:?}", st.error);
    let debs: Vec<&str> = st
        .artifacts
        .iter()
        .map(|(p, _)| p.as_str())
        .filter(|p| p.ends_with(".deb"))
        .collect();
    assert_eq!(debs.len(), 2, "{:?}", st.artifacts);
    // One dpkg line that installs the image and the matching headers, which
    // is what DKMS modules need to rebuild against the new kernel.
    let step = st.next_steps.join(" ");
    assert!(step.starts_with("sudo dpkg -i"), "{step}");
    assert!(step.contains("linux-image-"), "{step}");
    assert!(step.contains("linux-headers-"), "{step}");
}

#[test]
fn json_is_renderable_at_every_point_of_a_run() {
    let f = fixture("6.12.3");
    let job = run(request(&f, selections(&[("preempt", "full")])));
    let text = job.to_json(Some(0)).to_string();
    let parsed = kertweaker::json::parse(&text).expect("job JSON must parse");
    assert_eq!(parsed.str_or("status", ""), "done");
    assert!(parsed.get("stages").unwrap().as_array().unwrap().len() >= 8);
    assert!(!parsed.get("log").unwrap().as_array().unwrap().is_empty());
    // Cursor semantics: asking from the end returns nothing new.
    let total = parsed.get("log_total").unwrap().as_usize().unwrap();
    let tail = job.to_json(Some(total)).to_string();
    let tail = kertweaker::json::parse(&tail).unwrap();
    assert!(tail.get("log").unwrap().as_array().unwrap().is_empty());
}

#[test]
fn registry_tracks_jobs_and_hands_them_back_by_id() {
    let f = fixture("6.12.3");
    let reg = build::Registry::new();
    let mut req = request(&f, selections(&[]));
    req.config_only = true;
    let job = reg.start(req);
    assert_eq!(build::wait(&job, Duration::from_secs(30)), Status::Done);
    assert!(!reg.busy());
    assert_eq!(reg.all().len(), 1);
    assert!(reg.get(&job.id).is_some());
    assert!(reg.get("nope").is_none());
    assert_ne!(reg.next_id(), reg.next_id());
}
