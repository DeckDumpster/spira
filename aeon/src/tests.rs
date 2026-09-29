//! Whole-run tests: the contract of DESIGN.md §2 driven through `Run::main` with fakes for
//! bd, the lib.sh seam, other programs and the model session; git is real (a temp repo),
//! because the worktree contract is about git's own refusals.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use crate::conf::{Conf, Fayth};
use crate::ledger::Ledger;
use crate::ports::{Bd, Env, Exec, RealGit, Seam};
use crate::run::{Deps, Mode, Run, State};
use crate::seam::Snapshot;
use crate::session::{Launcher, SessionSpec, Stop};
use crate::util::{MemSink, Out};

#[derive(Default)]
struct World {
    status: BTreeMap<String, String>,
    labels: BTreeMap<String, BTreeSet<String>>,
    issue_type: BTreeMap<String, String>,
    /// Beads whose row carries a `supersedes` dependency (the aeon recorded a successor).
    supersedes: BTreeSet<String>,
    states: BTreeMap<String, String>,
    notes: Vec<(String, String)>,
    ready: Vec<String>,
    seam_calls: Vec<(String, Vec<String>)>,
    exec_calls: Vec<(String, Vec<String>, Option<String>)>,
    ready_fails: bool,
    claim_taken: BTreeSet<String>,
    /// `spira-claim stack <id>`'s canned stdout for these tests — `None` falls back to `"{}"`
    /// (no `claimable` key, so `stack::parse_proposal` reads it as "no stack", same as a
    /// test that never mentions stacking at all).
    stack_answer: Option<String>,
}

type W = Arc<Mutex<World>>;

struct FakeBd(W);

fn row_json(w: &World, id: &str) -> String {
    let labels: Vec<String> = w.labels.get(id).map(|l| l.iter().cloned().collect()).unwrap_or_default();
    serde_json::json!([{
        "id": id,
        "status": w.status.get(id).cloned().unwrap_or_else(|| "open".into()),
        "labels": labels,
        "issue_type": w.issue_type.get(id).cloned().unwrap_or_else(|| "task".into()),
        "dependencies": if w.supersedes.contains(id) {
            serde_json::json!([{"depends_on_id": "sp-successor", "dependency_type": "supersedes"}])
        } else {
            serde_json::json!([])
        },
    }])
    .to_string()
}

impl Bd for FakeBd {
    fn bd(&self, a: &[String]) -> Out {
        let mut w = self.0.lock().unwrap();
        let a: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
        match a.as_slice() {
            ["ready", ..] => {
                if w.ready_fails {
                    return Out::fail(1, "dolt: connection refused\n");
                }
                if a.contains(&"--json") {
                    let rows: Vec<serde_json::Value> = w.ready.iter().map(|id| serde_json::from_str::<serde_json::Value>(&row_json(&w, id)).unwrap()[0].clone()).collect();
                    Out::ok(serde_json::Value::Array(rows).to_string())
                } else {
                    Out::ok(w.ready.iter().map(|i| format!("{i} · title\n💡 hint\n")).collect::<String>())
                }
            }
            ["update", id, "--claim", "--json"] => {
                if w.claim_taken.contains(*id) {
                    return Out::ok("");
                }
                w.claim_taken.insert(id.to_string());
                w.status.insert(id.to_string(), "in_progress".into());
                Out::ok(format!("warning: noise\n{}", row_json(&w, id)))
            }
            ["show", id, "--json"] => Out::ok(row_json(&w, id)),
            ["show", id] => Out::ok(format!("{id} · the title\n💡 tip\nNOTES\nsome note\nLABELS: spira\n")),
            ["note", id, text] => {
                w.notes.push((id.to_string(), text.to_string()));
                Out::ok("")
            }
            ["state", id, "branch"] => Out::ok(w.states.get(*id).cloned().unwrap_or_else(|| "(no branch state set)".into())),
            ["set-state", id, kv] => {
                w.states.insert(id.to_string(), kv.trim_start_matches("branch=").to_string());
                Out::ok("")
            }
            ["label", "add", id, l] => {
                w.labels.entry(id.to_string()).or_default().insert(l.to_string());
                Out::ok("")
            }
            ["heartbeat", _] => Out::ok(""),
            ["memories", "--json"] => Out::ok(r#"{"law-core":"Core text.","law-other":"Other."}"#),
            _ => Out::ok(""),
        }
    }
}

struct FakeSeam {
    w: W,
    repo: PathBuf,
    answers: BTreeMap<&'static str, Out>,
}

impl Seam for FakeSeam {
    fn call(&self, func: &str, args: &[String]) -> Out {
        let mut w = self.w.lock().unwrap();
        w.seam_calls.push((func.to_string(), args.to_vec()));
        if let Some(o) = self.answers.get(func) {
            return o.clone();
        }
        match func {
            "aeon_count" => Out::ok("0"),
            "fayth_free" => Out::ok("1"),
            "_aeon_capacity_paused" => Out::fail(1, ""),
            "aeon_name_take" => Out::ok("ifrit"),
            "spira_home_repo" => Out::ok("fixture"),
            "repo_root" => Out::ok(self.repo.display().to_string()),
            "repo_land" => Out::ok("push"),
            "_aeon_base" => Out::ok("main\nmain\n\n"),
            "qualify_base_ref" => Out::ok("main"),
            "_aeon_rebase" => Out::ok(""),
            "_aeon_thrash_meta" => Out::ok("\n\n\n"),
            "_aeon_repo_info" => Out::ok(format!("fixture\t{}\tmain\n", self.repo.display())),
            "release_own_claim" => {
                let id = args[0].clone();
                if w.status.get(&id).map(|s| s.as_str()) == Some("in_progress") {
                    w.status.insert(id, "open".into());
                }
                Out::ok("")
            }
            "bead_reopen" => {
                w.status.insert(args[0].clone(), "open".into());
                Out::ok("")
            }
            "verdict_committed" => {
                let o = std::process::Command::new("git").arg("-C").arg(&args[0]).args(["log", "--format=%s", &args[1]]).output().unwrap();
                Out::ok(if String::from_utf8_lossy(&o.stdout).contains(&args[2]) { "yes" } else { "no" })
            }
            "close_verdict" => Out::ok(if args[1] != "closed" || args[4] == "yes" { "keep|committed" } else { "reopen|closed-without-commit|x" }),
            "bead_is_work_type" => {
                if ["task", "bug", "feature", "chore"].contains(&args[0].as_str()) {
                    Out::ok("")
                } else {
                    Out::fail(1, "")
                }
            }
            "land_state" | "capacity_reset_at" | "lc_bead_verified" | "open_ask_blocker" | "session_yield_headless" | "repo_land_queued" => Out::fail(1, ""),
            "session_outcome" => Out::ok("unlanded"),
            "requeues_of" => Out::ok("1"),
            _ => Out::ok(""),
        }
    }
}

struct FakeExec(W);

impl Exec for FakeExec {
    fn exec(&self, prog: &str, args: &[String], stdin: Option<Vec<u8>>, _cwd: Option<&Path>) -> Out {
        let sin = stdin.map(|b| String::from_utf8_lossy(&b).into_owned());
        self.0.lock().unwrap().exec_calls.push((prog.to_string(), args.to_vec(), sin.clone()));
        if prog.ends_with("spira-claim") {
            let ready: serde_json::Value = serde_json::from_str(sin.as_deref().unwrap_or("[]")).unwrap_or_default();
            let ids: Vec<String> = ready.as_array().map(|a| a.iter().map(|r| r["id"].as_str().unwrap().to_string()).collect()).unwrap_or_default();
            return match args[0].as_str() {
                "epics" => Out::ok(r#"{"prio":{},"started":[]}"#),
                "select" if args.contains(&"--top-tier".to_string()) => Out::ok(ids.iter().map(|i| format!("{i}||fixture|2|1|2\n")).collect::<String>()),
                "select" => Out::ok(ids.iter().map(|i| format!("2\t1\t2\t1\tt\t{i}\t\n")).collect::<String>()),
                "stack" => Out::ok(self.0.lock().unwrap().stack_answer.clone().unwrap_or_else(|| "{}".into())),
                _ => Out::ok("{}"),
            };
        }
        Out::ok("")
    }
}

/// Stands in for the model: runs a closure against the worktree, writes a result record.
struct FakeLauncher {
    w: W,
    seen: Mutex<Vec<SessionSpec>>,
    act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync>,
}

impl Launcher for FakeLauncher {
    fn run(&self, spec: &SessionSpec, stop: &Stop) -> i32 {
        self.seen.lock().unwrap().push(spec.clone());
        (self.act)(spec, &self.w, stop)
    }
}

struct Fx {
    dir: PathBuf,
    home: PathBuf,
    run: PathBuf,
    repo: PathBuf,
    w: W,
}

fn git(dir: &Path, args: &[&str]) {
    let o = std::process::Command::new("git").arg("-C").arg(dir).args(args).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

fn rev_parse(dir: &Path, rev: &str) -> String {
    let o = std::process::Command::new("git").arg("-C").arg(dir).args(["rev-parse", rev]).output().unwrap();
    assert!(o.status.success(), "rev-parse {rev}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

fn fx(name: &str) -> Fx {
    let dir = std::env::temp_dir().join(format!("aeon-run-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let home = dir.join("home");
    let run = dir.join("run");
    let repo = dir.join("repo");
    std::fs::create_dir_all(home.join("chamber")).unwrap();
    std::fs::create_dir_all(&run).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(
        home.join("chamber/builder.md"),
        "You are a builder. DB {{DB}}.\n<!-- task -->\n## The bead\n{{BEAD}}\nwork {{BEAD_ID}} in {{REPO}} on {{BRANCH}} ({{LANDING}})\n{{PARK}}\n{{FIXTURE}}\n## Finishing\n{{FINISH}}\n",
    )
    .unwrap();
    std::fs::write(home.join("chamber/builder.fayth"), "").unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f"), "seed\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-qm", "seed"]);
    let w: W = Arc::new(Mutex::new(World::default()));
    Fx { dir: dir.canonicalize().unwrap(), home, run, repo, w }
}

struct Outcome {
    code: i32,
    log: String,
    ledger: String,
    w: W,
    seen: Vec<SessionSpec>,
}

fn go(f: &Fx, labels: &str, extra: &[(&str, &str)], enforce: bool, mode: Mode, seam_answers: BTreeMap<&'static str, Out>, act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync>) -> Outcome {
    let mut vars: BTreeMap<String, String> = [
        ("SPIRA_RUN", f.run.display().to_string()),
        ("SPIRA_DB", "/db".to_string()),
        ("SPIRA_ASK_LABEL", "needs-ryan".to_string()),
        ("SPIRA_WORLD_STOP_LABEL", "world-stop".to_string()), // literal-ok: test fixture
        ("SPIRA_TESTDB_LIB", "spira/testdb.sh".to_string()),
        ("SPIRA_TRACE_MARK", "=== spira attempt".to_string()),
        ("SPIRA_CLAIM_RETRIES", "1".to_string()),
        ("SPIRA_CLAIM_RETRY_DELAY_S", "0".to_string()),
        ("FAYTH_LABELS", labels.to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    for (k, v) in extra {
        vars.insert(k.to_string(), v.to_string());
    }
    let mut base = BTreeMap::new();
    base.insert("PATH".to_string(), std::env::var("PATH").unwrap_or_default());
    base.insert("GH_TOKEN".to_string(), "secret".to_string());
    for (k, v) in [("GIT_AUTHOR_NAME", "t"), ("GIT_AUTHOR_EMAIL", "t@t"), ("GIT_COMMITTER_NAME", "t"), ("GIT_COMMITTER_EMAIL", "t@t")] {
        base.insert(k.into(), v.into());
    }
    let snap = Snapshot { env: base.clone(), vars: vars.clone(), ready_args: vec!["ready".into(), "--limit".into(), "0".into()], claim_exclude: "spira-poison".into() };
    let env = Env::new(base.clone(), base);
    let conf = Conf::new(&snap, &f.home);
    let fayth = Fayth::from_vars("builder", &vars);
    let bd = FakeBd(Arc::clone(&f.w));
    let seam = FakeSeam { w: Arc::clone(&f.w), repo: f.repo.clone(), answers: seam_answers };
    let git = RealGit { env: &env };
    let exec = FakeExec(Arc::clone(&f.w));
    let launcher = FakeLauncher { w: Arc::clone(&f.w), seen: Mutex::new(vec![]), act };
    let sink = MemSink::default();
    let clock = crate::util::now_epoch;
    let dry = mode == Mode::DryRun;
    let cwd = std::env::current_dir().unwrap();
    let code = {
        let mut run = Run {
            d: Deps { bd: &bd, seam: &seam, git: &git, exec: &exec, launcher: &launcher, sink: &sink, env: &env, clock: &clock },
            ledger: Ledger { path: conf.ledger(), dry },
            conf,
            fayth,
            snap,
            mode,
            pid: 4242,
            own_unit: String::new(),
            t0: crate::util::now_epoch(),
            enforce,
            // path-ok: a fake binary path in a unit-test fixture, never resolved
            claim_bin: Some("/bin/spira-claim".into()),
            stop: Arc::new(Stop::default()),
            hb_shutdown: Arc::new(AtomicBool::new(false)),
            hb_done: Arc::new(AtomicBool::new(false)),
            fayth_file: f.home.join("chamber/builder.fayth"),
            s: State::default(),
        };
        run.main()
    };
    let _ = std::env::set_current_dir(cwd);
    let seen = launcher.seen.lock().unwrap().clone();
    Outcome { code, log: sink.all(), ledger: std::fs::read_to_string(f.run.join("aeon-ledger.log")).unwrap_or_default(), w: Arc::clone(&f.w), seen }
}

fn no_session() -> Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> {
    Box::new(|_, _, _| panic!("the session must not start"))
}

/// The model's stand-in: commit work naming the bead, "close" it (legacy path), write a
/// result record.
fn commits_and_closes() -> Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> {
    Box::new(|spec, w, _| {
        let id = spec.env.get("BEAD_ID").unwrap().clone();
        std::fs::write(spec.cwd.join("f"), "work\n").unwrap();
        git(&spec.cwd, &["commit", "-qam", &format!("{id} — the work")]);
        w.lock().unwrap().status.insert(id, "closed".into());
        crate::run::append(&spec.log, "{\"type\":\"result\",\"duration_ms\":3000,\"num_turns\":3,\"total_cost_usd\":0.5}\n");
        0
    })
}

fn seed(f: &Fx, id: &str) {
    let mut w = f.w.lock().unwrap();
    w.ready.push(id.into());
    w.labels.entry(id.into()).or_default().extend(["spira".to_string(), "plan".into(), "repo:fixture".into()]);
}

fn ledger_lines(o: &Outcome) -> Vec<String> {
    o.ledger.lines().map(|l| l.splitn(2, ' ').nth(1).unwrap_or("").to_string()).collect()
}

// ---- declines -------------------------------------------------------------------------

#[test]
fn unfenced_predicate_refuses_before_any_ledger_line() {
    let f = fx("fence");
    let o = go(&f, "plan", &[("SPIRA_SCOPE_LABEL", "spira")], false, Mode::Claim, BTreeMap::new(), no_session());
    assert_eq!(o.code, 1);
    assert!(o.log.contains("FATAL builder: refusing to claim behind an unfenced predicate"));
    assert_eq!(o.ledger, "");
}

#[test]
fn at_capacity_lives_and_declines() {
    let f = fx("cap");
    let mut a = BTreeMap::new();
    a.insert("fayth_free", Out::ok("0"));
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, a, no_session());
    assert_eq!(o.code, 0);
    assert_eq!(ledger_lines(&o), vec!["born builder 4242", "awake builder capacity"]);
    assert!(o.log.contains("builder: at capacity (0/1), not summoning"));
}

#[test]
fn halted_draining_and_paused_decline_in_order() {
    let f = fx("gates");
    std::fs::write(f.run.join("world.draining"), "").unwrap();
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), no_session());
    assert_eq!(ledger_lines(&o)[1], "awake builder draining");
    std::fs::write(f.run.join("world.halted"), "").unwrap();
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), no_session());
    assert_eq!(ledger_lines(&o)[3], "awake builder halted", "halted is checked before draining");
    let f2 = fx("paused");
    let mut a = BTreeMap::new();
    a.insert("_aeon_capacity_paused", Out::ok("321"));
    let o = go(&f2, "spira,plan", &[], false, Mode::Claim, a, no_session());
    assert_eq!(ledger_lines(&o)[1], "awake builder paused");
    assert!(o.log.contains("out of capacity for another 321s — claiming nothing"));
}

#[test]
fn dry_run_writes_no_ledger_and_prints_candidates() {
    let f = fx("dry");
    seed(&f, "sp-dry");
    let o = go(&f, "spira,plan", &[], false, Mode::DryRun, BTreeMap::new(), no_session());
    assert_eq!(o.code, 0);
    assert_eq!(o.ledger, "", "a dry run inspects; it does not summon");
    assert!(o.log.contains("dry run — candidates:") && o.log.contains("sp-dry · title") && !o.log.contains("💡"));
    assert!(!f.w.lock().unwrap().claim_taken.contains("sp-dry"));
}

#[test]
fn a_failed_ready_query_is_claim_error_not_idle() {
    let f = fx("claimerr");
    f.w.lock().unwrap().ready_fails = true;
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), no_session());
    assert_eq!(o.code, 1);
    assert_eq!(ledger_lines(&o)[1], "awake builder claim-error claim_retry: query failed after 1 attempt(s): dolt: connection refused");
}

#[test]
fn nothing_ready_is_idle() {
    let f = fx("idle");
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), no_session());
    assert_eq!((o.code, ledger_lines(&o)[1].as_str()), (0, "awake builder idle"));
    // The ready set reached spira-claim on stdin, never argv.
    let w = o.w.lock().unwrap();
    let sel: Vec<_> = w.exec_calls.iter().filter(|c| c.0.ends_with("spira-claim")).collect();
    assert!(!sel.is_empty() && sel.iter().all(|c| c.2.as_deref() == Some("[]")));
}

#[test]
fn poison_raced_releases_and_records() {
    let f = fx("poison");
    seed(&f, "sp-p");
    f.w.lock().unwrap().labels.get_mut("sp-p").unwrap().insert("spira-poison".into());
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), no_session());
    assert_eq!(o.code, 0);
    let l = ledger_lines(&o);
    assert_eq!(l[1], "awake builder sp-p");
    assert!(l[2].starts_with("done builder sp-p rc=0 status=poison-raced wall_s=?"));
    assert!(o.w.lock().unwrap().seam_calls.iter().any(|c| c.0 == "release_own_claim"));
}

// ---- lifecycle_enforce ---------------------------------------------------------------

#[test]
fn enforce_with_a_missing_binary_refuses_before_any_setup() {
    let f = fx("enf-missing");
    seed(&f, "sp-e");
    let o = go(&f, "spira,plan", &[("SPIRA_LC_BIN", "/nonexistent/spira-lc"), ("SPIRA_WORK_BIN", "/nonexistent/work")], true, Mode::Claim, BTreeMap::new(), no_session());
    assert_eq!(o.code, 1);
    assert!(ledger_lines(&o)[2].contains("status=lifecycle-enforce-binary-missing"));
    let w = o.w.lock().unwrap();
    assert!(!w.seam_calls.iter().any(|c| c.0 == "lc_claim_bead"));
    assert!(!f.run.join("worktree").exists(), "no session setup");
}

#[test]
fn binary_presence_alone_never_selects_the_restricted_path() {
    let f = fx("enf-off");
    seed(&f, "sp-l");
    let bin = f.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for b in ["spira-lc", "work"] {
        let p = bin.join(b);
        testkit::write_exe(&p, "#!/bin/sh\n");
    }
    let extra = [("SPIRA_LC_BIN", bin.join("spira-lc").display().to_string()), ("SPIRA_WORK_BIN", bin.join("work").display().to_string())];
    let extra: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let o = go(&f, "spira,plan", &extra, false, Mode::Claim, BTreeMap::new(), commits_and_closes());
    let w = o.w.lock().unwrap();
    assert!(!w.seam_calls.iter().any(|c| c.0 == "lc_claim_bead"), "enforce off: no lifecycle CAS");
    assert_ne!(o.seen[0].prog, "bash", "enforce off: the model is not wrapped in work-env.sh");
    let task = std::fs::read_to_string(f.run.join("sp-l.task.md")).unwrap();
    assert!(!task.contains("You have no `bd`") && task.contains("bd -C /db close sp-l --reason-file -"));
}

#[test]
fn enforce_claims_through_the_machine_and_restricts_the_model() {
    let f = fx("enf-on");
    seed(&f, "sp-r");
    let bin = f.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for b in ["spira-lc", "work"] {
        let p = bin.join(b);
        testkit::write_exe(&p, "#!/bin/sh\n");
    }
    let extra = [("SPIRA_LC_BIN", bin.join("spira-lc").display().to_string()), ("SPIRA_WORK_BIN", bin.join("work").display().to_string())];
    let extra: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut a = BTreeMap::new();
    a.insert("lc_bead_verified", Out::ok(""));
    let act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> = Box::new(|spec, _w, _| {
        // `work submit` moves the machine, never bd's status.
        std::fs::write(spec.cwd.join("f"), "work\n").unwrap();
        git(&spec.cwd, &["commit", "-qam", "sp-r — the work"]);
        0
    });
    let o = go(&f, "spira,plan", &extra, true, Mode::Claim, a, act);
    assert_eq!(o.code, 0);
    let w = o.w.lock().unwrap();
    let cas = w.seam_calls.iter().find(|c| c.0 == "lc_claim_bead").expect("lifecycle CAS claim");
    assert_eq!(cas.1[1], "aeon-ifrit");
    assert_eq!(o.seen[0].prog, "bash");
    assert!(o.seen[0].args[0].ends_with("/work-env.sh") && o.seen[0].args[1] == "sp-r" && o.seen[0].args[2] == "--");
    let task = std::fs::read_to_string(f.run.join("sp-r.task.md")).unwrap();
    assert!(task.contains("**You have no `bd`.**"));
    assert!(ledger_lines(&o).last().unwrap().contains("status=submitted"));
    assert!(!w.labels["sp-r"].contains("spira-submitted"), "no bd-close reinterpretation on the restricted path");
}

#[test]
fn a_refused_lifecycle_claim_releases() {
    let f = fx("enf-refused");
    seed(&f, "sp-z");
    let bin = f.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for b in ["spira-lc", "work"] {
        let p = bin.join(b);
        testkit::write_exe(&p, "#!/bin/sh\n");
    }
    let extra = [("SPIRA_LC_BIN", bin.join("spira-lc").display().to_string()), ("SPIRA_WORK_BIN", bin.join("work").display().to_string())];
    let extra: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut a = BTreeMap::new();
    a.insert("lc_claim_bead", Out::fail(3, ""));
    let o = go(&f, "spira,plan", &extra, true, Mode::Claim, a, no_session());
    assert_eq!(o.code, 0);
    assert!(ledger_lines(&o)[2].contains("status=lc-claim-refused"));
}

fn lc_bin_extra(f: &Fx) -> Vec<(String, String)> {
    let bin = f.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for b in ["spira-lc", "work"] {
        let p = bin.join(b);
        testkit::write_exe(&p, "#!/bin/sh\n");
    }
    vec![("SPIRA_LC_BIN".to_string(), bin.join("spira-lc").display().to_string()), ("SPIRA_WORK_BIN".to_string(), bin.join("work").display().to_string())]
}

// ---- the stacked base (design stacked-dependents-2026-09-28 §1, sp-falao) --------------

#[test]
fn a_stacked_claim_merges_the_certified_prerequisites_tip_into_the_worktrees_base() {
    let f = fx("stack-ok");
    git(&f.repo, &["checkout", "-qb", "spira/sp-a"]);
    std::fs::write(f.repo.join("a.txt"), "from A\n").unwrap();
    git(&f.repo, &["add", "a.txt"]);
    git(&f.repo, &["commit", "-qm", "sp-a work"]);
    let tip_a = rev_parse(&f.repo, "HEAD");
    git(&f.repo, &["checkout", "-q", "main"]);

    seed(&f, "sp-b");
    f.w.lock().unwrap().stack_answer = Some(format!(r#"{{"claimable":true,"stack":{{"sp-a":"{tip_a}"}},"stack_depth":1,"stack_max_depth":4}}"#));

    let extra = lc_bin_extra(&f);
    let extra: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> =
        Box::new(|spec, _w, _| {
            assert!(spec.cwd.join("a.txt").is_file(), "the worktree must contain A's own commit");
            0
        });
    let o = go(&f, "spira,plan", &extra, true, Mode::Claim, BTreeMap::new(), act);
    assert_eq!(o.code, 0, "{}", o.log);
    assert_eq!(o.seen.len(), 1, "the session must have started — the merge must not have conflicted");
    let parents = std::process::Command::new("git").arg("-C").arg(&f.repo).args(["log", "--format=%P", "-1", "spira/sp-b"]).output().unwrap();
    let parents = String::from_utf8(parents.stdout).unwrap();
    assert_eq!(parents.split_whitespace().count(), 2, "the branch's base must be a two-parent merge of main and sp-a's tip: {parents:?}");
}

#[test]
fn a_stack_conflict_refuses_the_claim_and_notes_both_prerequisites() {
    let f = fx("stack-conflict");
    git(&f.repo, &["checkout", "-qb", "spira/sp-a"]);
    std::fs::write(f.repo.join("f"), "from A\n").unwrap();
    git(&f.repo, &["commit", "-qam", "sp-a work"]);
    let tip_a = rev_parse(&f.repo, "HEAD");
    git(&f.repo, &["checkout", "-q", "main"]);
    git(&f.repo, &["checkout", "-qb", "spira/sp-b-prereq"]);
    std::fs::write(f.repo.join("f"), "from B\n").unwrap();
    git(&f.repo, &["commit", "-qam", "sp-b-prereq work"]);
    let tip_b = rev_parse(&f.repo, "HEAD");
    git(&f.repo, &["checkout", "-q", "main"]);

    seed(&f, "sp-c");
    f.w.lock().unwrap().stack_answer =
        Some(format!(r#"{{"claimable":true,"stack":{{"sp-a":"{tip_a}","sp-b-prereq":"{tip_b}"}},"stack_depth":1,"stack_max_depth":4}}"#));

    let extra = lc_bin_extra(&f);
    let extra: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let o = go(&f, "spira,plan", &extra, true, Mode::Claim, BTreeMap::new(), no_session());
    assert_eq!(o.code, 0, "{}", o.log);
    assert!(ledger_lines(&o).last().unwrap().contains("status=stack-conflict"), "{:?}", ledger_lines(&o));
    let w = o.w.lock().unwrap();
    assert_eq!(w.status.get("sp-c").map(String::as_str), Some("open"), "the dependent stays held, not requeued as a fault");
    let notes: Vec<&str> = w.notes.iter().filter(|(id, _)| id == "sp-a" || id == "sp-b-prereq").map(|(_, t)| t.as_str()).collect();
    assert_eq!(notes.len(), 2, "both prerequisites get a note: {:?}", w.notes);
    assert!(notes.iter().all(|n| n.contains("stack conflict: sp-a x sp-b-prereq")), "{notes:?}");
}

// ---- fences before the workspace ------------------------------------------------------

#[test]
fn world_stop_bead_with_live_peers_is_released() {
    let f = fx("wstop");
    seed(&f, "sp-w");
    f.w.lock().unwrap().labels.get_mut("sp-w").unwrap().insert("world-stop".into()); // literal-ok: test fixture
    std::fs::write(f.run.join("aeon-builder-sp-other.pid"), format!("{}\n", std::process::id())).unwrap();
    std::fs::write(f.run.join("aeon-builder-sp-dead.pid"), "999999999\n").unwrap();
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), no_session());
    assert_eq!(o.code, 0);
    assert!(ledger_lines(&o)[2].contains("status=world-stop-fence")); // literal-ok: asserts the ledger status name
    assert!(o.log.contains("live aeons present (aeon-builder-sp-other)"));
    assert!(!f.run.join("aeon-builder-sp-dead.pid").exists(), "a dead peer's pidfile is removed");
    let w = o.w.lock().unwrap();
    assert!(w.notes.iter().any(|(_, n)| n.contains("requires the world halted")));
}

#[test]
fn unmapped_repo_is_parked_and_the_world_restarted() {
    let f = fx("unmapped");
    seed(&f, "sp-u");
    f.w.lock().unwrap().labels.get_mut("sp-u").unwrap().insert("world-stop".into()); // literal-ok: test fixture
    let mut a = BTreeMap::new();
    a.insert("repo_root", Out::fail(1, ""));
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, a, no_session());
    assert_eq!(o.code, 1);
    assert!(ledger_lines(&o)[2].contains("done builder sp-u rc=1 status=unmapped-repo"));
    let w = o.w.lock().unwrap();
    assert!(w.seam_calls.iter().any(|c| c.0 == "park_unmapped" && c.1 == vec!["sp-u", "fixture"]));
    let world: Vec<_> = w.exec_calls.iter().filter(|c| c.0.ends_with("world.sh")).map(|c| c.1[0].clone()).collect();
    assert_eq!(world, vec!["stop", "start"], "stop precedes start (DESIGN.md §8.2)");
}

// ---- the whole run --------------------------------------------------------------------

#[test]
fn happy_path_legacy_close_is_converted_to_submitted() {
    let f = fx("happy");
    seed(&f, "sp-h");
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), commits_and_closes());
    assert_eq!(o.code, 0, "{}", o.log);
    let l = ledger_lines(&o);
    assert_eq!(l[0], "born builder 4242");
    assert_eq!(l[1], "awake builder sp-h");
    assert_eq!(l[2], "done builder sp-h rc=0 status=submitted wall_s=3 api_s=? turns=3 in_tok=? cache_read_tok=? out_tok=? think_tok=? cost_usd=0.5000");
    let w = o.w.lock().unwrap();
    assert_eq!(w.status["sp-h"], "open");
    assert!(w.labels["sp-h"].contains("spira-submitted"));
    assert_eq!(w.states["sp-h"], "spira/sp-h");
    // One aeon, one worktree, under the sanctioned root.
    assert!(f.run.join("worktree/sp-h/.git").exists());
    assert!(!f.run.join("aeon-builder-sp-h.pid").exists(), "pidfile removed at teardown");
    // The session's launch.
    let spec = &o.seen[0];
    assert_eq!(spec.stdin_file, f.run.join("sp-h.task.md"));
    assert_eq!(spec.log, f.run.join("sp-h.log"));
    assert_eq!(spec.cwd, f.run.join("worktree/sp-h"));
    assert_eq!(spec.env.get("BEADS_ACTOR").map(|s| s.as_str()), Some("aeon-ifrit"));
    assert_eq!(spec.env.get("BEAD_ID").map(|s| s.as_str()), Some("sp-h"));
    assert_eq!(spec.env.get("SPIRA_WORK").map(|p| PathBuf::from(p)), Some(f.run.join("worktree/sp-h")));
    assert_eq!(spec.env.get("SPIRA_MAIL_FROM").map(|s| s.as_str()), Some("Builder <builder@spira>"));
    assert!(spec.env.get("GH_TOKEN").is_none(), "no credential-shaped var leaked to the model");
    assert_eq!(spec.env.get("GIT_TERMINAL_PROMPT").map(|s| s.as_str()), Some("0"));
    let a = spec.args.join(" ");
    assert!(a.starts_with("-p --output-format stream-json --verbose --include-partial-messages --system-prompt-snapshot on --append-system-prompt-file "));
    assert!(a.contains("--model claude-opus-5 --allowedTools Bash,Read,Edit,Write,Glob,Grep --dangerously-skip-permissions --settings {"));
    // The brief.
    let sys = std::fs::read_to_string(f.run.join("sp-h.system.md")).unwrap();
    let task = std::fs::read_to_string(f.run.join("sp-h.task.md")).unwrap();
    assert!(sys.starts_with("# Memories in force\n\n## Statutes in force") && sys.contains("You are a builder. DB /db."));
    assert!(task.starts_with("## The bead\nsp-h · the title\nNOTES\nsome note\nLABELS: spira"), "{task}");
    assert!(!task.contains("💡"));
    assert!(task.contains(&format!("work sp-h in {} on spira/sp-h (the sentinel merges `spira/sp-h` into `main`", f.run.join("worktree/sp-h").display())));
    assert!(task.contains("## Your lifetime: do the work, then exit") && task.contains("fixture lands by `push`"));
    assert!(task.contains("This repository has no shared test fixture"));
    assert!(task.contains("bd -C /db close sp-h --reason-file -"));
    assert!(task.contains("## If you find the work is already done") && task.contains("## Before you close: rebase onto `main`"));
    assert!(!task.contains("{{"), "no unreplaced placeholder reaches the model");
    // The attempt's segment was opened before the session.
    let trace = std::fs::read_to_string(f.run.join("sp-h.log")).unwrap();
    assert!(trace.starts_with("=== spira attempt 1 aeon=ifrit at="));
    assert!(o.log.contains("builder/ifrit: claiming sp-h (epic-first rank)"));
    assert!(o.log.contains("builder: sp-h closed a work bead directly — converted to submitted"));
}

#[test]
fn a_session_that_leaves_the_bead_open_is_unlanded_and_exits_its_rc() {
    let f = fx("unlanded");
    seed(&f, "sp-o");
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), Box::new(|_, _, _| 1));
    assert_eq!(o.code, 1, "{}", o.log);
    let l = ledger_lines(&o);
    assert!(l[2].starts_with("done builder sp-o rc=1 status=in_progress"), "{l:?}");
    let w = o.w.lock().unwrap();
    assert!(w.notes.iter().any(|(_, n)| n.starts_with("Unlanded (unlanded): the session ran to its own end")));
    assert_eq!(w.status["sp-o"], "open", "released");
}

#[test]
fn slain_mid_session_is_free_and_exits_143() {
    let f = fx("slain");
    seed(&f, "sp-s");
    let run = f.run.clone();
    let act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> = Box::new(move |_, _, stop| {
        std::fs::write(run.join("sp-s.slain"), "t\twhy\n").unwrap();
        stop.trip(15);
        143
    });
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), act);
    assert_eq!(o.code, 143);
    let l = ledger_lines(&o);
    assert!(l[2].starts_with("done builder sp-s rc=143 status=slain"), "{l:?}");
    let w = o.w.lock().unwrap();
    assert!(w.seam_calls.iter().any(|c| c.0 == "bump_requeue" && c.1 == vec!["sp-s", "unjudged-slain"]));
    assert!(!w.seam_calls.iter().any(|c| c.0 == "verdict_committed"), "the verdict block is skipped, as bash's trap skipped it");
    assert!(!f.run.join("aeon-builder-sp-s.pid").exists());
}

#[test]
fn a_worktree_failure_is_a_pre_session_death() {
    let f = fx("presession");
    seed(&f, "sp-d");
    let mut a = BTreeMap::new();
    a.insert("qualify_base_ref", Out::ok("refs/heads/no-such-base"));
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, a, no_session());
    assert_eq!(o.code, 1);
    assert!(o.log.contains("FATAL could not create a worktree at"));
    let l = ledger_lines(&o);
    assert!(l[2].starts_with("done builder sp-d rc=1 status=pre-session"), "{l:?}");
}

#[test]
fn the_verdict_reopens_a_close_behind_a_rebase_conflict_as_a_free_requeue() {
    let f = fx("rebase");
    seed(&f, "sp-c");
    let mut a = BTreeMap::new();
    // The session closed with work committed, but its branch does not replay on the base.
    a.insert("_aeon_rebase", Out { code: 1, stdout: "f ".into(), stderr: String::new() });
    let repo = f.repo.clone();
    let act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> = Box::new(move |spec, w, _| {
        std::fs::write(spec.cwd.join("f"), "mine\n").unwrap();
        git(&spec.cwd, &["commit", "-qam", "sp-c — the work"]);
        std::fs::write(repo.join("f"), "theirs\n").unwrap();
        git(&repo, &["commit", "-qam", "someone else"]);
        w.lock().unwrap().status.insert("sp-c".into(), "closed".into());
        0
    });
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, a, act);
    let l = ledger_lines(&o);
    assert!(l[2].starts_with("done builder sp-c rc=0 status=requeue-rebase-conflict"), "{l:?}\n{}", o.log);
    let w = o.w.lock().unwrap();
    assert!(w.seam_calls.iter().any(|c| c.0 == "bead_reopen" && c.1[1] == "rebase-conflict"));
    assert!(w.notes.iter().any(|(_, n)| n.starts_with("Requeue 1 (rebase-conflict):")));
}

#[test]
fn a_superseded_close_behind_a_conflicting_base_is_not_reopened() {
    // sp-dz39p: the session found its work superseded and closed with a successor recorded;
    // its stale branch no longer replays on the base. That is the expected state of
    // superseded work, not an unfinished rebase — the close stands.
    let f = fx("superseded");
    seed(&f, "sp-s");
    let mut a = BTreeMap::new();
    a.insert("_aeon_rebase", Out { code: 1, stdout: "f ".into(), stderr: String::new() });
    let repo = f.repo.clone();
    let act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> = Box::new(move |spec, w, _| {
        std::fs::write(spec.cwd.join("f"), "mine\n").unwrap();
        git(&spec.cwd, &["commit", "-qam", "sp-s — superseded"]);
        std::fs::write(repo.join("f"), "theirs\n").unwrap();
        git(&repo, &["commit", "-qam", "someone else"]);
        let mut w = w.lock().unwrap();
        w.status.insert("sp-s".into(), "closed".into());
        w.supersedes.insert("sp-s".into());
        0
    });
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, a, act);
    let w = o.w.lock().unwrap();
    assert!(!w.seam_calls.iter().any(|c| c.0 == "bead_reopen"), "{:?}\n{}", w.seam_calls, o.log);
    assert!(!o.log.contains("REOPENED"), "{}", o.log);
}

#[test]
fn sweep_runs_without_a_bead() {
    let f = fx("sweep");
    std::fs::write(f.home.join("chamber/builder.md"), "persona\n<!-- task -->\nstanding brief\n").unwrap();
    let act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> = Box::new(|spec, _, _| {
        crate::run::append(&spec.log, "{\"type\":\"result\",\"num_turns\":1}\n");
        1
    });
    let o = go(&f, "spira,plan", &[], false, Mode::Sweep { prompt: Some("look at X".into()) }, BTreeMap::new(), act);
    assert_eq!(o.code, 0, "a sweep that ran exits 0 even when the CLI exited 1");
    let l = ledger_lines(&o);
    assert_eq!(l[1], "awake builder sweep");
    assert!(l[2].starts_with("done builder sweep rc=1 status=sweep wall_s=? api_s=? turns=1"));
    let task = std::fs::read_to_string(f.run.join("sweep-builder-4242.task.md")).unwrap();
    assert_eq!(task, "look at X");
    assert!(!f.run.join("aeon-builder-sweep-4242.pid").exists());
}
