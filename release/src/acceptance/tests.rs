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

/// Local acceptance on d40bbb589: under the lifecycle cutover the model finishes with `work
/// submit` and never closes the bead, so a bd-only stage 4 read "not closed" while the
/// history already said SUBMITTED.
#[test]
fn lifecycle_submitted_is_any_state_past_the_model() {
    let st = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert!(lifecycle_submitted(&st(&["READY", "WORKING", "SUBMITTED"])));
    assert!(lifecycle_submitted(&st(&["READY", "WORKING", "SUBMITTED", "CERTIFIED"])));
    assert!(!lifecycle_submitted(&st(&["READY", "WORKING", "READY"])));
    assert!(!lifecycle_submitted(&[]));
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
        // testkit::write_exe, never fs::write + set_permissions (sp-os3of).
        testkit::write_exe(&p, "#!/bin/sh\n");
    }
    assert_eq!(missing_release_bins(&d), "bin/spira-supervise, bin/landing-pass");
    for b in ["spira-supervise", "landing-pass"] {
        let p = d.join("bin").join(b);
        fs::write(&p, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
    }
    assert_eq!(missing_release_bins(&d), "bin/spira-supervise, bin/landing-pass", "not executable is missing");
    for b in ["spira-supervise", "landing-pass"] {
        // chmod alone, on a file already written above (no write happens here) — no
        // fresh write-fd opens, so no ETXTBSY exposure (allow-listed, chmod-exec-leak).
        fs::set_permissions(d.join("bin").join(b), fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert_eq!(missing_release_bins(&d), "");
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

#[test]
fn lifecycle_states_keep_applied_events_in_order_without_repeats() {
    let h = r#"[{"to_state":"READY","applied":1},{"to_state":"READY","applied":1},{"to_state":"WORKING","applied":0},{"to_state":"WORKING","applied":true},{"to_state":"LANDED","applied":1}]"#;
    assert_eq!(lifecycle_states(h), s(&["READY", "WORKING", "LANDED"]));
    assert!(lifecycle_states("not json").is_empty());
}

#[test]
fn the_expected_sequence_adds_delivery_only_for_delivery_queue_modes() {
    let full = s(&["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "QUEUED", "BATCHED", "LANDED"]);
    assert_eq!(missing_in_order(&expected_lifecycle("queue.local"), &full), None);
    let push = s(&["READY", "WORKING", "SUBMITTED", "CERTIFIED", "LANDED"]);
    assert_eq!(missing_in_order(&expected_lifecycle("push"), &push), None);
    assert_eq!(missing_in_order(&expected_lifecycle("queue"), &push), Some("IN_DELIVERY"));
    assert_eq!(missing_in_order(&expected_lifecycle("push"), &s(&["READY", "LANDED", "WORKING"])), Some("SUBMITTED"));
}

#[test]
fn all_three_land_modes_are_required_with_either_queue_spelling() {
    assert_eq!(land_modes_missing(&s(&["push", "queue.forge"])), vec!["pr"]);
    assert!(land_modes_missing(&s(&["push", "queue", "pr"])).is_empty());
    assert_eq!(land_modes_missing(&[]), vec!["queue", "pr", "push"]);
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
    history_gap: bool,
    lc_create_fails: bool,
    history_late: Cell<u32>,
    rows: RefCell<Vec<String>>,
    no_cutover_script: bool,
    cutover_rc: i32,
    land_modes: Vec<(&'static str, &'static str)>,
    fast_tier_rc: i32,
    bare_lint_rc: i32,
    tag_notes: Vec<(&'static str, &'static str)>,
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
            history_gap: false,
            lc_create_fails: false,
            history_late: Cell::new(0),
            rows: RefCell::new(Vec::new()),
            no_cutover_script: false,
            cutover_rc: 0,
            land_modes: vec![("scratch-repo", "push"), ("scratch-q", "queue.local"), ("scratch-pr", "pr")],
            fast_tier_rc: 0,
            bare_lint_rc: 3,
            tag_notes: Vec::new(),
        }
    }

    /// Unpack a fake release named after the tarball and point `current` at it.
    fn activate(&self, name: &str) {
        let d = self.releases.join(name);
        fs::create_dir_all(d.join("bin")).unwrap();
        fs::create_dir_all(d.join("spira")).unwrap();
        for b in RELEASE_BINS {
            let p = d.join("bin").join(b);
            // testkit::write_exe, never fs::write + set_permissions (sp-os3of).
            testkit::write_exe(&p, "#!/bin/sh\n");
        }
        fs::write(d.join("spira/conf.sh"), "").unwrap();
        if !self.no_cutover_script {
            testkit::write_exe(&d.join("spira/cutover-deploy.sh"), "#!/bin/sh\n");
        }
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
            (_, ["install-tarball", "--skip-restart", tb, "--answers", answers]) => {
                // As the real one does: the operator's answers become the one config file.
                let text = fs::read_to_string(answers).unwrap();
                let toml = Path::new(c.env_of("SPIRA_TOML").expect("install-tarball names SPIRA_TOML"));
                spira_config::init::ensure(toml, spira_config::init::parse_answers(&text).unwrap(), None, &registry()).unwrap();
                self.activate(Path::new(tb).file_name().unwrap().to_string_lossy().trim_end_matches(".tar.gz"));
                ok("")
            }
            (_, ["install-tarball", tb]) | (_, ["install-tarball", "--skip-restart", tb]) => {
                self.activate(Path::new(tb).file_name().unwrap().to_string_lossy().trim_end_matches(".tar.gz"));
                ok("")
            }
            ("bash", ["-c", script, "_", _]) if script.contains("spira-lint") => Out { rc: self.bare_lint_rc, text: "plan-matrix: error: no base to compare against\n".into(), out: String::new() },
            ("aeon", ["fast-tier", ..]) => Out { rc: self.fast_tier_rc, text: if self.fast_tier_rc == 0 { "fast tier green\n".into() } else { "spira-lint failed (rc=3):\nplan-matrix: no base to compare against\n".into() }, out: String::new() },
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
            ("spira-config", ["repo", "names"]) => ok(&self.land_modes.iter().map(|(n, _)| format!("{n}\n")).collect::<String>()),
            ("spira-config", ["repo", "land", n]) => ok(&format!("{}\n", self.land_modes.iter().find(|(x, _)| x == n).map_or("", |(_, m)| m))),
            ("spira-lc", ["create-bead", id]) => {
                if self.lc_create_fails {
                    return Out { rc: 2, text: "cannot tell: connecting to the socket: Connection refused\n".into(), out: String::new() };
                }
                self.rows.borrow_mut().push(id.to_string());
                ok("{}\n")
            }
            ("spira-lc", ["history", _]) => {
                if self.history_late.get() > 0 {
                    self.history_late.set(self.history_late.get() - 1);
                    let ev: Vec<String> = ["WORKING", "SUBMITTED", "CERTIFIED"].iter().map(|s| format!(r#"{{"from_state":"x","to_state":"{s}","applied":"1"}}"#)).collect();
                    return ok(&format!("[{}]", ev.join(",")));
                }
                let states = if self.history_gap { &["READY", "WORKING", "SUBMITTED", "LANDED"][..] } else { &["READY", "WORKING", "SUBMITTED", "CERTIFIED", "LANDED"][..] };
                let ev: Vec<String> = states.iter().map(|s| format!(r#"{{"to_state":"{s}","applied":1}}"#)).collect();
                ok(&format!("[{}]", ev.join(",")))
            }
            ("cutover-deploy.sh", _) => Out { rc: self.cutover_rc, text: if self.cutover_rc == 0 { "cutover-deploy: done\n".into() } else { "cutover-deploy: refusing\n".into() }, out: String::new() },
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
            ("git", ["-C", _, "tag", "--list", ..]) => ok(&self.tag_notes.iter().map(|(t, _)| format!("{t}\n")).collect::<String>()),
            ("git", ["-C", _, "notes", "--ref=acceptance", "show", r]) => match self.tag_notes.iter().find(|(t, _)| *r == format!("refs/tags/{t}")) {
                Some((_, n)) => ok(n),
                None => Out { rc: 1, text: "no note\n".into(), out: String::new() },
            },
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
        fs::create_dir_all(root.join("config/spira")).unwrap();
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
        assert_eq!(c.env_of("SPIRA_TOML"), Some(spira_config::toml_path_at(&b.root.join("config/spira")).display().to_string().as_str()), "{}", c.line());
    }
    // uninstall.sh (phases A, C, D) carries the SAME SPIRA_HOME_REPO install_env() gave the
    // install it undoes. Without it, owned.sh's manifest re-derives repo_is_git_checkout
    // from scratch under bare launcher_env(), resolves a different answer than install saw,
    // and leaves the cert-sweep units unlisted — hence unremoved (sp-dn2rl).
    let uninstalls: Vec<&&Cmd> = tools.iter().filter(|c| c.prog == "uninstall.sh").collect();
    assert_eq!(uninstalls.len(), 3, "phase A, C and D each uninstall once");
    for c in &uninstalls {
        assert_eq!(c.env_of("SPIRA_HOME_REPO"), Some("scratch-repo"), "{}", c.line());
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

const WAIVED_NOTE: &str = "PASS\nD t  3 passed, 0 failed\nupgrade phases waived by operator\n";

#[test]
fn a_waiver_is_honoured_for_its_own_cut_and_the_next_cut_is_refused_until_the_phases_run() {
    let b = Box_::new();
    let cut = "spira-release-spira-20260930T000000Z";
    let prior = "spira-release-spira-20260901T000000Z";

    let mut f = b.fake();
    f.tag_notes = vec![(cut, ""), (prior, "PASS\nD t  3 passed, 0 failed\naged-install from=x: PASS\n"), ("spira-release-spira-20260801T000000Z", WAIVED_NOTE)];
    assert_eq!(phases::run(&f, b.opts(&["--waive-upgrade"])), 0, "a cut whose predecessor ran the phases may waive: {:?}", b.fails());

    let mut f = b.fake();
    f.tag_notes = vec![(cut, ""), (prior, WAIVED_NOTE)];
    assert_eq!(phases::run(&f, b.opts(&["--waive-upgrade"])), 2, "the next cut may not waive again");
    assert!(!f.log.borrow().iter().any(|c| c.args.first().map(String::as_str) == Some("install-tarball")), "refused before any phase");

    let f = {
        let mut f = b.fake();
        f.tag_notes = vec![(cut, ""), (prior, WAIVED_NOTE)];
        f
    };
    assert_eq!(phases::run(&f, b.opts(&[])), 2, "nor may it skip the phases by naming no predecessor");

    let mut f = b.fake();
    f.tag_notes = vec![(cut, ""), (prior, WAIVED_NOTE)];
    let rc = phases::run(&f, with_prev(&b, &[]));
    assert_eq!(rc, 0, "running the phases clears it: {:?}", b.fails());
    assert!(f.log.borrow().iter().any(|c| c.prog == "deploy.sh"), "the upgrade ran");
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
    fs::write(b.root.join("config/spira/spira-lc.credential"), "x").unwrap();
    fs::write(b.root.join("config/spira/spira-lc.credential-ro"), "x").unwrap();
    fs::write(b.root.join("config/spira/conf"), "x").unwrap();
    f.sidecar_wrong = true;
    assert_eq!(phases::run(&f, with_prev(&b, &[])), 1);
    assert_eq!(b.fails(), vec!["phase B: .tag sidecar names spira-release-spira-20260930T000000Z: wanted [spira-release-spira-20260930T000000Z] got [spira-release-other]".to_string()]);
    // The first failure in a phase snapshots once.
    assert!(b.root.join("forensics/01-first-fail-phase-B/units.txt").is_file());
    let snap = b.root.join("forensics/01-first-fail-phase-B");
    assert!(snap.join("conf").is_file(), "the positive control: config files are still collected");
    let leaked: Vec<_> = fs::read_dir(&snap).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.contains("credential")).collect();
    assert!(leaked.is_empty(), "credential files collected: {leaked:?}");
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
fn phase_d_runs_cutover_deploy_dry_run_with_the_scratch_repo_on_the_launcher_path() {
    let b = Box_::new();
    let f = b.fake();
    assert_eq!(phases::run(&f, with_prev(&b, &[])), 0);
    let log = f.log.borrow();
    let c = log.iter().find(|c| c.prog == "cutover-deploy.sh").expect("cutover-deploy.sh was never run");
    assert_eq!(c.args, s(&["--repo", "scratch-repo", "--dry-run"]));
    assert!(c.env_of("PATH").unwrap().contains("current/spira"));
    assert!(b.checks().iter().any(|c| c["check"] == "phase D: cutover-deploy.sh --dry-run runs on the aged install"));
}

#[test]
fn a_failing_cutover_deploy_is_a_phase_d_fail() {
    let b = Box_::new();
    let mut f = b.fake();
    f.cutover_rc = 1;
    assert_eq!(phases::run(&f, with_prev(&b, &[])), 1);
    assert!(b.fails().iter().any(|x| x.starts_with("phase D: cutover-deploy.sh --dry-run runs on the aged install: exit 1")), "{:#?}", b.fails());
}

#[test]
fn an_upgraded_release_without_cutover_deploy_is_a_phase_d_fail() {
    let b = Box_::new();
    let mut f = b.fake();
    f.no_cutover_script = true;
    assert_eq!(phases::run(&f, with_prev(&b, &[])), 1);
    assert!(b.fails().iter().any(|x| x.starts_with("phase D: upgraded release carries cutover-deploy.sh: missing")), "{:#?}", b.fails());
    assert!(!f.log.borrow().iter().any(|c| c.prog == "cutover-deploy.sh"));
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
fn the_probe_bead_is_filed_with_its_lifecycle_row() {
    let b = Box_::new();
    let f = b.fake();
    assert_eq!(phases::run(&f, b.opts(&[])), 0, "{:#?}", b.fails());
    assert_eq!(*f.rows.borrow(), *f.probes.borrow(), "every probe gets a row, and only probes");
}

#[test]
fn a_probe_whose_row_cannot_be_created_fails_bead_filed() {
    let b = Box_::new();
    let mut f = b.fake();
    f.lc_create_fails = true;
    assert_eq!(phases::run(&f, b.opts(&[])), 1);
    let fails = b.fails();
    assert!(fails.iter().any(|x| x.starts_with("phase A: bead filed") && x.contains("create-bead")), "{fails:#?}");
}

#[test]
fn a_history_missing_a_state_fails_the_lifecycle_check() {
    let b = Box_::new();
    let mut f = b.fake();
    f.history_gap = true;
    assert_eq!(phases::run(&f, b.opts(&[])), 1);
    let fails = b.fails();
    assert_eq!(fails.len(), 1, "{fails:#?}");
    assert!(fails[0].contains("lifecycle event sequence") && fails[0].contains("CERTIFIED"), "{fails:#?}");
}

#[test]
fn a_scratch_setup_without_all_land_modes_fails_the_scratch_setup_check() {
    let b = Box_::new();
    let mut f = b.fake();
    f.land_modes = vec![("scratch-repo", "push")];
    assert_eq!(phases::run(&f, b.opts(&[])), 1);
    let fails = b.fails();
    assert!(fails.iter().any(|x| x.contains("each land mode") && x.contains("queue, pr")), "{fails:#?}");
}

#[test]
fn record_without_a_notes_repo_is_a_usage_error() {
    let a = s(&["t", "--scratch-repo", "/nonexistent", "--record"]);
    let _env = testkit::env(&[("SPIRA_NOTES_REPO", None)]);
    assert_eq!(main(&a), 2);
}

/// The registry this tree ships, as the real install-tarball reads it out of the tarball.
fn registry() -> spira_config::init::Registry<'static> {
    // A bd on the box's PATH, as the real install-tarball's PATH carries one.
    let bin: &'static testkit::TempDir = Box::leak(Box::new(testkit::TempDir::new("acc-registry-bd")));
    testkit::write_exe(bin.path().join("bd"), "#!/bin/sh\n");
    let env: &'static std::collections::BTreeMap<String, String> =
        Box::leak(Box::new([("HOME".to_string(), "/home/test".to_string()), ("PATH".to_string(), bin.path().display().to_string())].into_iter().collect()));
    spira_config::init::Registry { conf_d: Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../spira/conf.d")), env }
}

/// Phase A's answers are exactly what the installer needs from an operator: every required
/// input present (so `release install-tarball --answers` never prompts or refuses), and the
/// config they produce passes the same id_prefix check doctor/pre-activate run.
#[test]
fn phase_a_answers_produce_a_valid_spira_toml() {
    let b = Box_::new();
    let f = b.fake();
    let o = b.opts(&[]);
    let releases = o.releases();
    let r = Run::new(&f, o);
    let a = spira_config::init::parse_answers(&r.answers_text(&releases)).expect("answers parse");
    assert!(spira_config::init::missing(&a).is_empty(), "missing: {:?}", spira_config::init::missing(&a));
    let text = spira_config::init::render(&a, &spira_config::toml_path_at(&b.root.join("config/spira")), &registry()).expect("answers render");
    let doc = spira_config::validate(&text).expect("valid");
    assert!(spira_config::require_id_prefix(&doc).is_ok());
    assert_eq!(spira_config::get_path(&doc, "spira.releases").as_deref(), Some(releases.display().to_string().as_str()));
    assert_eq!(spira_config::get_path(&doc, "spira.operated").as_deref(), Some("0"));
}

#[test]
fn lifecycle_states_reads_the_store_s_real_history_shape() {
    // As production's spira-lc history prints it (2026-10-04): applied is the string "1",
    // the row's READY is only the first event's from_state, and refused events repeat.
    let h = r#"[{"from_state":"READY","to_state":"WORKING","applied":"1"},
        {"from_state":"WORKING","to_state":"SUBMITTED","applied":"1"},
        {"from_state":"SUBMITTED","to_state":"SUBMITTED","applied":"0"},
        {"from_state":"SUBMITTED","to_state":"CERTIFIED","applied":"1"},
        {"from_state":"CERTIFIED","to_state":"LANDED","applied":"1"}]"#;
    let got = crate::acceptance::lifecycle_states(h);
    assert_eq!(got, vec!["READY", "WORKING", "SUBMITTED", "CERTIFIED", "LANDED"]);
    assert_eq!(crate::acceptance::missing_in_order(&crate::acceptance::expected_lifecycle("push"), &got), None);
    assert!(crate::acceptance::lifecycle_states(r#"[{"from_state":"READY","to_state":"WORKING","applied":"0"}]"#).is_empty(), "nothing applied, nothing passed through");
}

#[test]
fn a_landed_event_recorded_a_pass_later_is_waited_for() {
    // The audit worker records LANDED one sentinel pass after the push (sp-53own).
    let b = Box_::new();
    let f = b.fake();
    f.history_late.set(3);
    assert_eq!(phases::run(&f, b.opts(&[])), 0, "{:#?}", b.fails());
    assert_eq!(f.history_late.get(), 0, "the late reads were all consumed");
}

#[test]
fn every_deploy_carries_the_forge_repository_this_run_resolved() {
    // An installed release has no git remote; deploy.sh must be told its repo (sp-j0vhm).
    let b = Box_::new();
    let f = b.fake();
    let mut o = with_prev(&b, &[]);
    o.gh_repo = Some("Owner/spira".into());
    phases::run(&f, o);
    let log = f.log.borrow();
    let deploys: Vec<&Cmd> = log.iter().filter(|c| c.prog == "deploy.sh").collect();
    assert!(deploys.len() >= 3, "upgrade, rollback and aged deploys ran: {}", deploys.len());
    for c in deploys {
        assert_eq!(c.env_of("SPIRA_FORGE_REPO"), Some("Owner/spira"), "{}", c.line());
    }
}

#[test]
fn a_builders_handoff_goes_through_the_fast_tier_in_phases_a_and_d() {
    let b = Box_::new();
    let f = b.fake();
    assert_eq!(phases::run(&f, with_prev(&b, &[])), 0);
    let names: Vec<String> = b.checks().iter().map(|c| c["check"].as_str().unwrap().to_string()).collect();
    for label in ["phase A", "phase D"] {
        for what in ["positive control: spira-lint with no base refuses", "stub builder's handoff passes the fast tier"] {
            assert!(names.iter().any(|n| n.starts_with(&format!("{label}: {what}"))), "{label}: {what} in {names:#?}");
        }
    }
    let log = f.log.borrow();
    let tiers: Vec<&Cmd> = log.iter().filter(|c| c.prog == "aeon").collect();
    assert_eq!(tiers.len(), 2);
    let cur = b.root.join("tmp/releases/current");
    for c in tiers {
        assert_eq!(c.args[0], "fast-tier");
        assert_eq!(c.args[3], "spira/acceptance-fast-tier");
        assert_eq!(c.env_of("PATH").map(|p| p.starts_with(&cur.join("bin").display().to_string())), Some(true), "{}", c.line());
    }
}

/// The release this bead was filed against: the fast tier refused every handoff.
#[test]
fn a_release_whose_fast_tier_refuses_the_handoff_fails_acceptance() {
    let b = Box_::new();
    let mut f = b.fake();
    f.fast_tier_rc = 1;
    assert_eq!(phases::run(&f, with_prev(&b, &[])), 1);
    let fails = b.fails();
    assert!(fails.iter().any(|x| x.starts_with("phase A: stub builder's handoff passes the fast tier") && x.contains("no base to compare against")), "{fails:#?}");
    assert!(fails.iter().any(|x| x.starts_with("phase D: stub builder's handoff passes the fast tier")), "{fails:#?}");
}

#[test]
fn a_fast_tier_check_that_cannot_fail_is_itself_a_failure() {
    let b = Box_::new();
    let mut f = b.fake();
    f.bare_lint_rc = 0;
    assert_eq!(phases::run(&f, b.opts(&[])), 1);
    assert!(b.fails().iter().any(|x| x.starts_with("phase A: positive control: spira-lint with no base refuses")), "{:#?}", b.fails());
}
