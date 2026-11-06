//! `release acceptance` unit tests: the pure readers, the argument contract, and whole runs
//! (all four phases) against a fake host with a fake clock — a PASS, the waiver, and each
//! class of FAIL (DESIGN.md "acceptance").

use super::*;
use std::cell::{Cell, RefCell};
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
fn unit_set_leaves_out_units_a_release_directory_render_never_keeps() {
    let t = "spira-cert-sweep-full-prod.timer enabled\nspira-round-template-prod.service static\nspira-a.service static\n";
    assert_eq!(unit_set(t), s(&["spira-a.service static"]));
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
    let n = Note { verdict: "PASS", date: "D", tag: "t", pass: 3, fail: 0, tarball: Some(("abc", "spira-1.tar.gz")), prev_tag: Some("p"), waived: false, standing: false };
    assert_eq!(n.text(), "PASS\nD t  3 passed, 0 failed\nsha256:abc tarball:spira-1.tar.gz\naged-install from=p: PASS");
    let n = Note { verdict: "FAIL", date: "D", tag: "t", pass: 1, fail: 2, tarball: None, prev_tag: None, waived: true, standing: false };
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
fn fired_at_reads_one_tasks_last_fired_time_from_scheduler_status() {
    let doc = r#"{"schema":1,"at":9,"halted":false,"held":0,"tasks":[{"name":"sentinel-call","last_fired_at":7},{"name":"landing","last_fired_at":null}],"classes":[]}"#;
    assert_eq!(fired_at(doc, "sentinel-call"), Some(7));
    assert_eq!(fired_at(doc, "landing"), None, "not fired yet");
    assert_eq!(fired_at(doc, "nope"), None, "not registered");
    assert_eq!(fired_at("not json", "sentinel-call"), None);
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

/// A scripted world: a scratch repo where every probe bead is called, committed, closed and
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
    seed_no_rows: bool,
    lc_create_fails: bool,
    history_late: Cell<u32>,
    rows: RefCell<Vec<String>>,
    no_cutover_script: bool,
    cutover_rc: i32,
    land_modes: Vec<(&'static str, &'static str)>,
    fast_tier_rc: i32,
    bare_lint_rc: i32,
    tag_notes: Vec<(&'static str, &'static str)>,
    tag_commit: &'static str,
    /// The release under test ships no sentinel unit: the loop is the scheduler's tasks.
    no_sentinel_unit: bool,
    /// Each `world scheduler-status --json` read advances every task's last fired time.
    fired: Cell<u64>,
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
            seed_no_rows: false,
            lc_create_fails: false,
            history_late: Cell::new(0),
            rows: RefCell::new(Vec::new()),
            no_cutover_script: false,
            cutover_rc: 0,
            land_modes: vec![("scratch-repo", "push"), ("scratch-q", "queue.local"), ("scratch-pr", "pr")],
            fast_tier_rc: 0,
            bare_lint_rc: 3,
            tag_notes: Vec::new(),
            tag_commit: "",
            no_sentinel_unit: false,
            fired: Cell::new(0),
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
            testkit::write_exe(d.join("spira/cutover-deploy.sh"), "#!/bin/sh\n");
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
            ("systemctl", ["--user", "list-unit-files", "spira-sentinel*.service", ..]) => ok(if self.no_sentinel_unit { "" } else { "spira-sentinel-t.service static\n" }),
            ("systemctl", ["--user", "list-unit-files", "spira-runtime*.service", ..]) => ok("spira-runtime-t.service enabled\n"),
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
            ("dwarf", ["fast-tier", ..]) => Out { rc: self.fast_tier_rc, text: if self.fast_tier_rc == 0 { "fast tier green\n".into() } else { "spira-lint failed (rc=3):\nplan-matrix: no base to compare against\n".into() }, out: String::new() },
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
            ("spira-lc", ["history", id]) if self.seed_no_rows && id.starts_with("sp-s") => ok("[]"),
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
            ("world", ["scheduler-status", "--json"]) => {
                self.fired.set(self.fired.get() + 1);
                let n = self.fired.get();
                ok(&format!(r#"{{"schema":1,"at":{n},"halted":false,"held":0,"tasks":[{{"name":"sentinel-call","last_fired_at":{n}}},{{"name":"landing","last_fired_at":{n}}}],"classes":[]}}"#))
            }
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
            ("git", ["-C", _, "rev-parse", r]) if r.ends_with("^{commit}") => match self.tag_commit {
                "" => Out { rc: 128, text: "unknown revision\n".into(), out: String::new() },
                c => ok(&format!("{c}\n")),
            },
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
            call_secs: 180,
            asset_wait_secs: 600,
            pid: 4242,
        }
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
fn an_aged_flag_without_its_anchor_is_a_usage_error() {
    let base = s(&["t", "--scratch-repo", "r"]);
    let mut a = base.clone();
    a.extend(s(&["--aged-tag", "x"]));
    assert!(parse_args(&a).is_err(), "--aged-tag alone");
    let mut a = base.clone();
    a.extend(s(&["--prev-tag", "p", "--aged-tarball", "x"]));
    assert!(parse_args(&a).is_err(), "--aged-tarball alone");
    let mut a = base;
    a.extend(s(&["--prev-tag", "p", "--aged-tag", "x", "--waive-upgrade"]));
    assert_eq!(parse_args(&a).unwrap().aged_tag, None, "the waiver clears the aged base too");
}

#[test]
fn exec_failures_names_the_log_and_line_of_every_unexecable_tool() {
    let root = testkit::TempDir::new("exec-failures");
    let run = root.path();
    fs::create_dir_all(run.join("sub")).unwrap();
    fs::write(run.join("landing.log"), "fine\nspira: queue early: No such file or directory (os error 2)\n").unwrap();
    fs::write(run.join("sub/verdict.log"), "spira-lc list exited 127: timeout: failed to execute process\n").unwrap();
    fs::write(run.join("watchd.log"), "watchd: could not exec gh-intake: x\n").unwrap();
    fs::write(run.join("inbox-keeper.log"), "inbox-keeper.sh: line 25: /proc/5281/cmdline: No such file or directory\n").unwrap();
    fs::write(run.join("notes.txt"), "exit 127\n").unwrap();
    let got = exec_failures(run);
    assert_eq!(got.len(), 3, "{got:?}");
    assert!(got[0].starts_with("landing.log:2: "), "{got:?}");
    assert!(got[1].starts_with("sub/verdict.log:1: "), "{got:?}");
    assert!(got[2].starts_with("watchd.log:1: "), "{got:?}");
}

#[test]
fn exec_failures_is_empty_for_a_clean_run_dir() {
    let root = testkit::TempDir::new("exec-clean");
    fs::write(root.join("landing.log"), "all good\n").unwrap();
    assert!(exec_failures(root.path()).is_empty());
}

#[test]
fn skipped_upgrade_phases_without_a_waiver_are_partial() {
    use super::phases::verdict_for;
    assert_eq!(verdict_for(0, true, false), "PASS");
    assert_eq!(verdict_for(0, false, true), "PASS");
    assert_eq!(verdict_for(0, false, false), "PARTIAL");
    assert_eq!(verdict_for(2, false, false), "FAIL");
    assert_eq!(verdict_for(1, true, false), "FAIL");
}

// ---- the loop is the scheduler's tasks ---------------------------------------------------

fn lines(f: &Fake) -> Vec<String> {
    f.log.borrow().iter().map(Cmd::line).collect()
}

/// The release under test has no sentinel unit: a kick starts the scheduler and waits on
/// `scheduler status` until the task has fired since the kick.
#[test]
fn a_kick_starts_the_scheduler_and_waits_for_the_task_to_fire() {
    let b = Box_::new();
    let mut f = b.fake();
    f.no_sentinel_unit = true;
    let r = Run::new(&f, b.opts(&[]));
    r.kick("sentinel-call");
    let log = lines(&f);
    assert!(log.iter().any(|l| l == "systemctl --user start spira-runtime-t.service"), "{log:?}");
    assert!(!log.iter().any(|l| l.contains("start spira-sentinel") || l == "systemctl --user start "), "no sentinel unit is started: {log:?}");
    let reads = log.iter().filter(|l| l.ends_with("world scheduler-status --json")).count();
    assert!(reads >= 2, "a baseline read, then the read that saw it fire: {log:?}");
}

/// A predecessor release still ships the sentinel unit; its kick starts that unit as before.
#[test]
fn a_predecessor_with_the_sentinel_unit_is_kicked_as_before() {
    let b = Box_::new();
    let f = b.fake();
    let r = Run::new(&f, b.opts(&[]));
    r.kick("sentinel-call");
    let log = lines(&f);
    assert!(log.iter().any(|l| l == "systemctl --user start spira-sentinel-t.service"), "{log:?}");
    assert!(!log.iter().any(|l| l.contains("scheduler-status")), "{log:?}");
}
