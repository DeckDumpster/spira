//! `release acceptance` unit tests: the pure readers, the argument contract, and whole runs
//! (all four phases) against a fake host with a fake clock — a PASS, the waiver, and each
//! class of FAIL (DESIGN.md "acceptance").

use super::*;
use std::cell::{Cell, RefCell};
use std::os::unix::fs::PermissionsExt;
use testkit::TempDir;

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

// ---- pure readers ------------------------------------------------------------------------

#[test]
fn extract_bead_id_reads_past_a_warning_prefix() {
    let out = "warning: beads.role not configured (GH#2950)\nFix: git config beads.role maintainer\n✓ Created issue: sp-xxxx — acceptance test probe title\n  Priority: P2";
    assert_eq!(extract_bead_id(out).as_deref(), Some("sp-xxxx"));
    // Positive control and the empty pair either side.
    assert_eq!(extract_bead_id("warning: beads.role not configured (GH#2950)"), None);
    assert_eq!(extract_bead_id("error: cannot connect to database\nconnection refused"), None);
    assert_eq!(extract_bead_id("Created issue: sp-ab12.3"), Some("sp-ab12".into()));
}

#[test]
fn bead_finished_is_closed_or_submitted_and_unreadable_is_not() {
    assert!(bead_finished(r#"[{"id":"sp-1","status":"closed"}]"#));
    assert!(bead_finished(r#"{"id":"sp-1","status":"open","labels":["spira-submitted"]}"#));
    assert!(bead_finished("warning: skew\n[{\"status\":\"closed\"}]"));
    assert!(!bead_finished(r#"[{"id":"sp-1","status":"open","labels":["plan"]}]"#));
    assert!(!bead_finished("Error: no such bead"));
    assert!(!bead_finished("[]"));
}

#[test]
fn ready_has_finds_the_probe_and_unreadable_is_not_claimable() {
    assert!(ready_has(r#"[{"id":"sp-a"},{"id":"sp-b"}]"#, "sp-b"));
    assert!(!ready_has(r#"[{"id":"sp-a"}]"#, "sp-b"));
    assert!(!ready_has("not json", "sp-b"));
}

#[test]
fn unit_set_is_sorted_spira_units_without_transient_ones() {
    let t = "spira-z.timer enabled enabled\nspira-landing.service transient -\ndolt.service enabled\nspira-a.service static -\n";
    assert_eq!(unit_set(t), s(&["spira-a.service static", "spira-z.timer enabled"]));
}

#[test]
fn missing_release_bins_names_what_is_missing_and_nothing_when_complete() {
    let d = TempDir::new("acc-bins");
    fs::create_dir_all(d.join("bin")).unwrap();
    for b in ["loom", "panel", "broker"] {
        let p = d.join("bin").join(b);
        fs::write(&p, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert_eq!(missing_release_bins(&d), "bin/spira-supervise, bin/landing-pass");
    for b in ["spira-supervise", "landing-pass"] {
        let p = d.join("bin").join(b);
        fs::write(&p, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
    }
    assert_eq!(missing_release_bins(&d), "bin/spira-supervise, bin/landing-pass", "not executable is missing");
    for b in ["spira-supervise", "landing-pass"] {
        fs::set_permissions(d.join("bin").join(b), fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert_eq!(missing_release_bins(&d), "");
}

#[test]
fn conf_line_value_reads_the_first_assignment() {
    let t = "SPIRA_OPERATED = 0\n\nSPIRA_CHECK5_MAX_FILE = 42\nSPIRA_CHECK5_MAX_FILE = 7\n";
    assert_eq!(conf_line_value(t, "SPIRA_CHECK5_MAX_FILE").as_deref(), Some("42"));
    assert_eq!(conf_line_value(t, "SPIRA_NOPE"), None);
}

#[test]
fn gh_repo_prefers_env_then_a_github_origin() {
    assert_eq!(gh_repo_from([None, Some("a/b".into()), Some("c/d".into())], None).as_deref(), Some("a/b"));
    assert_eq!(gh_repo_from([None, None, None], Some("git@github.com:Deck/spira.git\n".into())).as_deref(), Some("Deck/spira"));
    assert_eq!(gh_repo_from([None, None, None], Some("https://github.com/Deck/spira".into())).as_deref(), Some("Deck/spira"));
    assert_eq!(gh_repo_from([None, None, None], Some("/srv/git/spira.git".into())), None);
}

#[test]
fn json_line_escapes_and_orders_fields() {
    assert_eq!(json_line("phase-A", "a \"q\"\nb", None, "T", 3), r#"{"phase":"phase-A","check":"a \"q\"\nb","verdict":"ok","ts":"T","elapsed":3}"#);
    assert_eq!(json_line("p", "c", Some("r"), "T", 0), r#"{"phase":"p","check":"c","verdict":"fail","reason":"r","ts":"T","elapsed":0}"#);
}

#[test]
fn note_carries_tarball_upgrade_path_and_waiver() {
    let n = Note { verdict: "PASS", date: "D", tag: "t", pass: 3, fail: 0, tarball: Some(("abc", "spira-1.tar.gz")), prev_tag: Some("p"), waived: false };
    assert_eq!(n.text(), "PASS\nD t  3 passed, 0 failed\nsha256:abc tarball:spira-1.tar.gz\naged-install from=p: PASS");
    let n = Note { verdict: "FAIL", date: "D", tag: "t", pass: 1, fail: 2, tarball: None, prev_tag: None, waived: true };
    assert_eq!(n.text(), "FAIL\nD t  1 passed, 2 failed\nupgrade phases waived by operator");
}

#[test]
fn the_override_key_is_one_conf_sh_honours() {
    // conf.sh's allowlist is generated from spira/conf.d/, one file per key (sp-g3uwp): a key
    // with no file there is refused, and the override would never be in force.
    let key = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../spira/conf.d")).join(OVERRIDE_KEY);
    assert!(key.is_file(), "{OVERRIDE_KEY} has no spira/conf.d entry: conf.sh would refuse it and the override would never be in force");
}

// ---- arguments ---------------------------------------------------------------------------

#[test]
fn usage_errors() {
    assert!(parse_args(&[]).is_err(), "no tag");
    assert_eq!(parse_args(&s(&["t"])).unwrap_err(), "--scratch-repo is required");
    assert!(parse_args(&s(&["t", "u", "--scratch-repo", "r"])).unwrap_err().contains("too many"));
    assert!(parse_args(&s(&["t", "--scratch-repo", "r", "--bogus"])).unwrap_err().contains("unknown option"));
    assert!(parse_args(&s(&["t", "--scratch-repo"])).unwrap_err().contains("needs a value"));
}

#[test]
fn flags_in_both_forms_and_the_waiver_clears_prev_tag() {
    let a = parse_args(&s(&["t", "--scratch-repo=r", "--prev-tag", "p", "--tarball=x.tar.gz", "--record", "--agent", "ag"])).unwrap();
    assert_eq!((a.tag.as_str(), a.scratch_repo.to_str(), a.prev_tag.as_deref(), a.record), ("t", Some("r"), Some("p"), true));
    assert_eq!(a.tarball, Some(PathBuf::from("x.tar.gz")));
    assert_eq!(a.agent.as_deref(), Some("ag"));
    let w = parse_args(&s(&["t", "--scratch-repo", "r", "--prev-tag", "p", "--waive-upgrade"])).unwrap();
    assert_eq!(w.prev_tag, None);
    assert!(w.waive_upgrade);
}

// ---- whole runs against a fake host ------------------------------------------------------

/// A scripted world: a scratch repo where every probe bead is summoned, committed, closed and
/// landed; `release install-tarball` unpacks a fake release; `deploy.sh` switches `current`
/// and writes the tag sidecar. Knobs break one thing at a time.
struct Fake {
    clock: Cell<u64>,
    log: RefCell<Vec<Cmd>>,
    releases: PathBuf,
    missing_tool: Option<&'static str>,
    probes: RefCell<Vec<String>>,
    next_id: Cell<u32>,
    bead_count_fails: bool,
    sidecar_wrong: bool,
    failed_unit: bool,
    rollback: (i32, &'static str),
    never_lands: bool,
}

impl Fake {
    fn new(releases: PathBuf) -> Fake {
        Fake {
            clock: Cell::new(1_000_000),
            log: RefCell::new(Vec::new()),
            releases,
            missing_tool: None,
            probes: RefCell::new(Vec::new()),
            next_id: Cell::new(0),
            bead_count_fails: false,
            sidecar_wrong: false,
            failed_unit: false,
            rollback: (0, ""),
            never_lands: false,
        }
    }

    /// Unpack a fake release named after the tarball and point `current` at it.
    fn activate(&self, name: &str) {
        let d = self.releases.join(name);
        fs::create_dir_all(d.join("bin")).unwrap();
        fs::create_dir_all(d.join("spira")).unwrap();
        for b in RELEASE_BINS {
            let p = d.join("bin").join(b);
            fs::write(&p, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::write(d.join("spira/conf.sh"), "").unwrap();
        let cur = self.releases.join("current");
        let _ = fs::remove_file(&cur);
        std::os::unix::fs::symlink(name, &cur).unwrap();
    }

    fn sha(&self) -> String {
        format!("sha{}", self.probes.borrow().len())
    }

    fn answer(&self, c: &Cmd) -> Out {
        let a: Vec<&str> = c.args.iter().map(String::as_str).collect();
        let ok = |t: &str| Out::stdout(0, t);
        let prog = c.prog.rsplit('/').next().unwrap_or("");
        match (prog, a.as_slice()) {
            ("systemctl", ["--user", "is-system-running"]) => ok("running\n"),
            ("systemctl", ["--user", "list-unit-files", "spira-sentinel*.service", ..]) => ok("spira-sentinel-t.service static\n"),
            ("systemctl", ["--user", "list-unit-files", "--no-legend", "--plain", "spira-*.service"]) => ok("spira-archive-t.service static\nspira-loom-t.service enabled\n"),
            ("systemctl", ["--user", "list-unit-files", "--no-legend", "--plain"]) => ok("dolt.service enabled\n"),
            ("systemctl", ["--user", "list-unit-files", "--no-legend"]) => ok("spira-a.service enabled\nspira-b.timer enabled\n"),
            ("systemctl", ["--user", "show", "-p", "Type", "--value", u]) => ok(if u.contains("archive") { "oneshot\n" } else { "simple\n" }),
            ("systemctl", ["--user", "list-units", "--state=active", ..]) => ok("spira-sentinel-t.timer loaded active waiting\n"),
            ("systemctl", ["--user", "list-units", "--state=failed", ..]) => ok(if self.failed_unit { "spira-ops-t.service loaded failed failed\n" } else { "" }),
            ("systemctl", _) => ok(""),
            (_, ["install-tarball", tb]) | (_, ["install-tarball", "--skip-restart", tb]) => {
                self.activate(Path::new(tb).file_name().unwrap().to_string_lossy().trim_end_matches(".tar.gz"));
                ok("")
            }
            ("bash", ["-c", script, "_", _]) => {
                let key = script.split("${").nth(1).and_then(|k| k.split(':').next()).unwrap_or("");
                match key {
                    "SPIRA_PLAN_LABEL" => ok("plan"),
                    "SPIRA_SCOPE_LABEL" => ok("spira"),
                    "SPIRA_RELEASES" => ok(&self.releases.display().to_string()),
                    "SPIRA_PROD" => ok(&format!("{}/current/spira", self.releases.display())),
                    _ => ok(""),
                }
            }
            ("bash", _) => ok(""), // ready.sh
            ("spira-install", _) => ok(""), // spira-install (sp-31dm0): invoked directly now, no bash wrapper
            ("deploy.sh", args) => {
                let tag = args.last().unwrap();
                if args.contains(&"--allow-draft") {
                    self.activate("spira-new");
                    fs::create_dir_all(self.releases.join(".tags")).unwrap();
                    let v = if self.sidecar_wrong { "spira-release-other" } else { tag };
                    fs::write(self.releases.join(".tags/spira-new"), format!("{v}\n")).unwrap();
                    ok("")
                } else {
                    Out { rc: self.rollback.0, text: self.rollback.1.into(), out: String::new() }
                }
            }
            ("world.sh", ["status"]) => ok("world: RUNNING\n"),
            ("uninstall.sh" | "world.sh" | "doctor", _) => ok(""),
            ("bd", ["-C", _, "create", "--title", _, "--description", _, "--label", l, ..]) if l.starts_with("acceptance,") => {
                self.next_id.set(self.next_id.get() + 1);
                let id = format!("sp-p{}", self.next_id.get());
                self.probes.borrow_mut().push(id.clone());
                ok(&format!("warning: beads.role not configured (GH#2950)\n✓ Created issue: {id} — probe\n"))
            }
            ("bd", ["-C", _, "create", ..]) => {
                self.next_id.set(self.next_id.get() + 1);
                ok(&format!("✓ Created issue: sp-s{}\n", self.next_id.get()))
            }
            // A warning on stderr (bd's schema-skew notice) never reaches what is parsed.
            ("bd", ["-C", _, "ready", ..]) => {
                let json = format!("[{}]", self.probes.borrow().iter().map(|i| format!("{{\"id\":\"{i}\"}}")).collect::<Vec<_>>().join(","));
                Out { rc: 0, text: format!("{json}\nwarning: schema skew\n"), out: json }
            }
            ("bd", ["-C", _, "show", _, "--json"]) => {
                let json = r#"[{"status":"open","labels":["spira-submitted"]}]"#;
                Out { rc: 0, text: format!("{json}\nwarning: schema skew\n"), out: json.into() }
            }
            ("bd", ["-C", _, "list", "--all", "--json"]) => {
                if self.bead_count_fails {
                    Out { rc: 1, text: "Error: database unreachable\n".into(), out: String::new() }
                } else {
                    ok(&format!("[{}]", vec!["{}"; self.next_id.get() as usize].join(",")))
                }
            }
            ("bd", ["-C", _, "memories"]) => ok("law-a\nlaw-b\n"),
            ("bd", _) => ok(""),
            ("git", ["-C", _, "rev-parse", "--abbrev-ref", "HEAD"]) => ok("main\n"),
            ("git", ["-C", _, "rev-parse", "origin/main"]) => ok(&format!("{}\n", if self.never_lands { "sha0".into() } else { self.sha() })),
            ("git", ["-C", _, "log", "--oneline", br]) => ok(&format!("abc {}: acceptance probe\n", br.trim_start_matches("spira/"))),
            ("git", ["-C", _, "log", "--format=%s", range]) => {
                let (from, to) = range.split_once("..").unwrap();
                let n = |s: &str| s.trim_start_matches("sha").parse::<usize>().unwrap();
                let p = self.probes.borrow();
                ok(&p[n(from)..n(to)].iter().map(|i| format!("{i}: acceptance probe")).collect::<Vec<_>>().join("\n"))
            }
            ("git", _) => ok(""),
            ("gh", _) => ok(""),
            _ => Out { rc: 127, text: format!("fake: no answer for {}\n", c.line()), out: String::new() },
        }
    }
}

impl Host for Fake {
    fn run(&self, c: &Cmd) -> Out {
        self.log.borrow_mut().push(c.clone());
        self.clock.set(self.clock.get() + 1);
        self.answer(c)
    }
    fn show(&self, c: &Cmd) -> i32 {
        self.run(c).rc
    }
    fn on_path(&self, prog: &str) -> bool {
        !prog.starts_with("spira-acceptance-nonexistent-") && Some(prog) != self.missing_tool
    }
    fn now(&self) -> u64 {
        self.clock.get()
    }
    fn sleep(&self, secs: u64) {
        self.clock.set(self.clock.get() + secs);
    }
}

struct Box_ {
    root: TempDir,
}

impl Box_ {
    fn new() -> Box_ {
        let root = TempDir::new("acc-run");
        fs::create_dir_all(root.join("scratch-repo/.git")).unwrap();
        fs::create_dir_all(root.join("tmp")).unwrap();
        fs::write(root.join("spira-20260930T000000Z.tar.gz"), "new").unwrap();
        fs::write(root.join("spira-20260901T000000Z.tar.gz"), "old").unwrap();
        Box_ { root }
    }
    fn opts(&self, extra: &[&str]) -> Opts {
        let mut argv = s(&["spira-release-spira-20260930T000000Z", "--scratch-repo"]);
        argv.push(self.root.join("scratch-repo").display().to_string());
        argv.push("--tarball".into());
        argv.push(self.root.join("spira-20260930T000000Z.tar.gz").display().to_string());
        argv.extend(s(extra));
        Opts {
            a: parse_args(&argv).unwrap(),
            bd_db: self.root.join("db"),
            tmp: self.root.join("tmp"),
            xdg_config: self.root.join("config"),
            forensics: self.root.join("forensics"),
            release_bin: PathBuf::from("/opt/candidate/bin/release"),
            base_path: "/usr/bin:/bin".into(),
            spira_run: self.root.join("run"),
            token_projects: self.root.join("projects"),
            gh_repo: None,
            notes_repo: Some(self.root.join("notes")),
            summon_secs: 180,
            asset_wait_secs: 600,
            pid: 4242,
        }
    }
    fn prev(&self) -> Vec<String> {
        s(&["--prev-tag", "spira-release-spira-20260901T000000Z", "--prev-tarball", &self.root.join("spira-20260901T000000Z.tar.gz").display().to_string()])
    }
    fn fake(&self) -> Fake {
        Fake::new(self.root.join("tmp/releases"))
    }
    fn checks(&self) -> Vec<serde_json::Value> {
        fs::read_to_string(self.root.join("forensics/checks.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }
    fn fails(&self) -> Vec<String> {
        self.checks().into_iter().filter(|c| c["verdict"] == "fail").map(|c| format!("{}: {}", c["check"].as_str().unwrap(), c["reason"].as_str().unwrap())).collect()
    }
}

fn with_prev(b: &Box_, extra: &[&str]) -> Opts {
    let mut v: Vec<String> = b.prev();
    v.extend(s(extra));
    b.opts(&v.iter().map(String::as_str).collect::<Vec<_>>())
}

#[test]
fn a_healthy_release_passes_all_four_phases_and_records_its_note() {
    let b = Box_::new();
    let f = b.fake();
    let rc = phases::run(&f, with_prev(&b, &["--record"]));
    assert_eq!(b.fails(), Vec::<String>::new());
    assert_eq!(rc, 0);
    let checks = b.checks();
    let names: Vec<&str> = checks.iter().map(|c| c["check"].as_str().unwrap()).collect();
    for want in [
        "positive-control: command -v catches missing tool",
        "positive-control: binary check names missing binary (spira-supervise)",
        "phase A: release install-tarball exits 0",
        "phase A: all native binaries present and executable",
        "phase A: bead is claimable by builder predicate (spira,plan)",
        "phase A: no spira-* units remain after uninstall",
        "phase B: .tag sidecar names spira-release-spira-20260930T000000Z",
        "phase B: SPIRA_PROD updated to releases path",
        "phase C: unit set after rollback matches pre-upgrade snapshot (no extra/dropped units)",
        "phase D: operator override survived aged upgrade",
        "phase D: rollback to spira-release-spira-20260901T000000Z succeeded",
    ] {
        assert!(names.contains(&want), "missing check {want:?} in {names:#?}");
    }
    assert!(names.iter().any(|n| n.starts_with("phase A stage 5: bead sp-p1 landed on ")));
    assert!(names.iter().any(|n| n.starts_with("phase D stage 5: bead sp-p") && n.contains("landed by ancestry")));
    assert!(names.iter().any(|n| n.starts_with("phase D: bead count preserved through migration (")));
    // Every check is in one phase or another.
    let phases: std::collections::BTreeSet<&str> = checks.iter().map(|c| c["phase"].as_str().unwrap()).collect();
    assert_eq!(phases.into_iter().collect::<Vec<_>>(), vec!["phase-A", "phase-B", "phase-C", "phase-D", "prerequisites"]);

    let log = f.log.borrow();
    let note = log.iter().find(|c| c.args.contains(&"notes".to_string())).expect("a note");
    let m = note.args.iter().position(|a| a == "-m").unwrap();
    let text = &note.args[m + 1];
    assert!(text.starts_with("PASS\n"), "{text}");
    assert!(text.contains("sha256:") && text.contains("tarball:spira-20260930T000000Z.tar.gz"), "{text}");
    assert!(text.ends_with("aged-install from=spira-release-spira-20260901T000000Z: PASS"), "{text}");
    assert_eq!(note.args.last().unwrap(), "refs/tags/spira-release-spira-20260930T000000Z");
    assert_eq!(note.args[1], b.root.join("notes").display().to_string());
}

#[test]
fn every_tool_runs_on_the_release_launcher_path_and_every_deploy_of_the_tag_allows_a_draft() {
    let b = Box_::new();
    let f = b.fake();
    phases::run(&f, with_prev(&b, &[]));
    let cur = b.root.join("tmp/releases/current");
    let want_path = format!("{}:{}:/usr/bin:/bin", cur.join("bin").display(), cur.join("spira").display());
    let log = f.log.borrow();
    let tools: Vec<&Cmd> = log.iter().filter(|c| ["deploy.sh", "uninstall.sh", "world.sh", "doctor"].contains(&c.prog.as_str())).collect();
    assert!(tools.len() >= 11, "{} tool calls", tools.len());
    for c in &tools {
        assert_eq!(c.env_of("PATH"), Some(want_path.as_str()), "{}", c.line());
        assert_eq!(c.env_of("SPIRA_CONF"), Some(b.root.join("config/spira/spira.conf").display().to_string().as_str()), "{}", c.line());
    }
    let deploys_of_tag: Vec<&&Cmd> = tools.iter().filter(|c| c.prog == "deploy.sh" && c.args.last().unwrap().ends_with("20260930T000000Z")).collect();
    assert_eq!(deploys_of_tag.len(), 2, "phase B and phase D");
    for c in deploys_of_tag {
        assert!(c.args.contains(&"--allow-draft".to_string()), "{}", c.line());
        assert!(c.args.contains(&"--tarball".to_string()), "a local tarball is handed to every deploy of the tag: {}", c.line());
    }
    // install-tarball is this binary, with a run dir beside the releases dir.
    let inst: Vec<&Cmd> = log.iter().filter(|c| c.args.first().map(String::as_str) == Some("install-tarball")).collect();
    assert_eq!(inst.len(), 3, "A, B and D");
    for c in inst {
        assert_eq!(c.prog, "/opt/candidate/bin/release");
        assert_eq!(c.env_of("SPIRA_RUN"), Some(b.root.join("tmp/run").display().to_string().as_str()));
    }
    // install (sp-31dm0) always runs with SPIRA_OPERATED=0, on the launcher environment too.
    for c in log.iter().filter(|c| c.prog.ends_with("/bin/spira-install")) {
        assert_eq!(c.env_of("SPIRA_OPERATED"), Some("0"));
        assert_eq!(c.env_of("SPIRA_HOME_REPO"), Some("scratch-repo"));
        assert_eq!(c.env_of("PATH"), Some(want_path.as_str()), "install resolves doctor and spira-config by bare name");
        assert_eq!(c.env_of("SPIRA_RELEASE"), Some(cur.display().to_string().as_str()));
    }
    // The probe carries the builder partition's labels.
    let probe = log.iter().find(|c| c.prog == "bd" && c.args.iter().any(|a| a.starts_with("acceptance,"))).unwrap();
    assert!(probe.args.contains(&"acceptance,plan,spira,repo:scratch-repo".to_string()));
    // The sentinel is started by its installed (instance-suffixed) name.
    assert!(log.iter().any(|c| c.prog == "systemctl" && c.args == s(&["--user", "start", "spira-sentinel-t.service"])));
    assert!(!log.iter().any(|c| c.prog == "systemctl" && c.args == s(&["--user", "start", "spira-sentinel.service"])));
}

#[test]
fn the_waiver_skips_upgrade_phases_and_says_so_in_the_note() {
    let b = Box_::new();
    let f = b.fake();
    let rc = phases::run(&f, with_prev(&b, &["--waive-upgrade", "--record"]));
    assert_eq!(rc, 0, "{:?}", b.fails());
    let log = f.log.borrow();
    assert!(!log.iter().any(|c| c.prog == "deploy.sh"), "no upgrade was attempted");
    let note = log.iter().find(|c| c.args.contains(&"notes".to_string())).unwrap();
    let text = &note.args[note.args.iter().position(|a| a == "-m").unwrap() + 1];
    assert!(text.ends_with("upgrade phases waived by operator"), "{text}");
    assert!(!text.contains("aged-install from="), "{text}");
}

#[test]
fn a_missing_prerequisite_stops_before_any_phase_with_exit_2() {
    let b = Box_::new();
    let mut f = b.fake();
    f.missing_tool = Some("dolt");
    assert_eq!(phases::run(&f, b.opts(&[])), 2);
    assert_eq!(b.fails(), vec!["prereq: dolt on PATH: not found; install before running acceptance".to_string()]);
    assert!(!f.log.borrow().iter().any(|c| c.args.first().map(String::as_str) == Some("install-tarball")));
}

#[test]
fn an_uncountable_store_fails_phase_d_rather_than_reading_as_zero() {
    let b = Box_::new();
    let mut f = b.fake();
    f.bead_count_fails = true;
    assert_eq!(phases::run(&f, with_prev(&b, &[])), 1);
    let fails = b.fails();
    assert_eq!(fails.len(), 1, "{fails:#?}");
    assert!(fails[0].starts_with("phase D: bead count preserved through migration: could not count beads"), "{fails:#?}");
}

#[test]
fn a_wrong_sidecar_fails_phase_b() {
    let b = Box_::new();
    let mut f = b.fake();
    f.sidecar_wrong = true;
    assert_eq!(phases::run(&f, with_prev(&b, &[])), 1);
    assert_eq!(b.fails(), vec!["phase B: .tag sidecar names spira-release-spira-20260930T000000Z: wanted [spira-release-spira-20260930T000000Z] got [spira-release-other]".to_string()]);
    // The first failure in a phase snapshots once.
    assert!(b.root.join("forensics/01-first-fail-phase-B/units.txt").is_file());
}

#[test]
fn a_failed_unit_after_the_aged_upgrade_is_a_fail() {
    let b = Box_::new();
    let mut f = b.fake();
    f.failed_unit = true;
    assert_eq!(phases::run(&f, with_prev(&b, &[])), 1);
    assert_eq!(b.fails(), vec!["phase D: no failed spira units 2 min after aged upgrade: spira-ops-t.service".to_string()]);
}

#[test]
fn a_refused_rollback_must_name_the_migration() {
    let b = Box_::new();
    let mut f = b.fake();
    f.rollback = (1, "deploy: refusing — schema Migration 12 is not reversible\n");
    assert_eq!(phases::run(&f, with_prev(&b, &[])), 1, "phase C's rollback is refused too");
    let fails = b.fails();
    assert!(!fails.iter().any(|x| x.starts_with("phase D: rollback refused")), "{fails:#?}");
    assert!(b.checks().iter().any(|c| c["check"] == "phase D: rollback refused — names migration (law-pin-by-migration-count)"));

    let b = Box_::new();
    let mut f = b.fake();
    f.rollback = (1, "deploy: health check failed\n");
    phases::run(&f, with_prev(&b, &[]));
    assert!(b.fails().iter().any(|x| x.starts_with("phase D: rollback refused but output does not name migration: deploy: health check failed")));
}

#[test]
fn a_bead_that_never_lands_fails_stage_5_inside_its_budget() {
    let b = Box_::new();
    let mut f = b.fake();
    f.never_lands = true;
    let start = f.clock.get();
    assert_eq!(phases::run(&f, b.opts(&[])), 1);
    let fails = b.fails();
    assert_eq!(fails.len(), 1, "{fails:#?}");
    assert!(fails[0].starts_with("phase A stage 5: bead sp-p1 landed on ") && fails[0].contains("no commit with bead id on origin/main after 12"), "{fails:#?}");
    assert!(f.clock.get() - start < 400, "the budget bounds the wait");
}

#[test]
fn record_without_a_notes_repo_is_a_usage_error() {
    let a = s(&["t", "--scratch-repo", "/nonexistent", "--record"]);
    std::env::remove_var("SPIRA_NOTES_REPO");
    assert_eq!(main(&a), 2);
}

/// Regression sp-oppza: phase A's own bootstrap `spira.conf` — not `configure.sh`, which
/// never touches a file already there — is what a fresh acceptance install's box actually
/// gets its config from. sp-k6m1m made `spira.id_prefix` required by `spira-config validate`
/// (doctor, pre-activate) whenever `[spira]` sets anything, but this bootstrap text set
/// `SPIRA_OPERATED`/`SPIRA_RELEASES` without ever setting `SPIRA_ID_PREFIX` — so the box it
/// produced failed activation immediately. Runs the bootstrap text through the SAME
/// converter `conf.sh`'s auto-convert uses (`spira-config convert`'s own reader), the
/// positive control every absence check needs: before the fix, `id_prefix` came back `None`
/// and `require_id_prefix` refused exactly as the real box did.
#[test]
fn phase_a_bootstrap_conf_sets_id_prefix() {
    let b = Box_::new();
    let f = b.fake();
    let o = b.opts(&[]);
    let releases = o.releases();
    let r = Run::new(&f, o);
    let text = r.bootstrap_conf_text(&releases);

    assert!(text.contains("SPIRA_OPERATED = 0"), "sanity: this IS the bootstrap conf — {text:?}");

    let raw = spira_config::convert::read_conf(&text, "/home/test");
    let mut warnings = spira_config::convert::ConvertWarnings::default();
    let section = spira_config::convert::spira_section(&raw, &mut warnings).expect("no unknown keys in the bootstrap conf");
    assert_eq!(section.id_prefix.as_deref(), Some("sp"), "bootstrap conf must set SPIRA_ID_PREFIX (sp-k6m1m/sp-oppza)");

    let doc = spira_config::SpiraToml { spira: Some(section), repo: Default::default(), persona: Default::default() };
    assert!(spira_config::require_id_prefix(&doc).is_ok(), "the converted document must pass the same check doctor/pre-activate run");
}
