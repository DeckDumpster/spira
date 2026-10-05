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
    descriptions: BTreeMap<String, String>,
    metadata: BTreeMap<String, BTreeMap<String, String>>,
    notes: Vec<(String, String)>,
    ready: Vec<String>,
    seam_calls: Vec<(String, Vec<String>)>,
    exec_calls: Vec<(String, Vec<String>, Option<String>)>,
    /// Every `bd` invocation, verbatim argv — `seam_calls`/`exec_calls`' own generic
    /// recording, extended to the one port (`Bd`) that did not have it yet (sp-1zxru: the
    /// no-progress hold's `bd update <id> --defer <ts>` is otherwise invisible to a test —
    /// `FakeBd::bd`'s own match falls through every argv it does not special-case).
    bd_calls: Vec<Vec<String>>,
    ready_fails: bool,
    claim_taken: BTreeSet<String>,
    /// `spira-claim stack <id>`'s canned stdout for these tests — `None` falls back to `"{}"`
    /// (no `claimable` key, so `stack::parse_proposal` reads it as "no stack", same as a
    /// test that never mentions stacking at all).
    stack_answer: Option<String>,
    /// Makes the `spira-lc content-on-base` exec call refuse.
    fail_content_on_base: bool,
    /// `spira-lc holds <id>`'s answer, one hold kind a line.
    holds: BTreeMap<String, String>,
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
        "description": w.descriptions.get(id).cloned().unwrap_or_default(),
        "metadata": w.metadata.get(id).cloned().unwrap_or_default(),
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
        w.bd_calls.push(a.to_vec());
        let a: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
        match a.as_slice() {
            ["ready", ..] | ["list", ..] => {
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
            ["update", id, "--set-metadata", kv] => {
                let (k, v) = kv.split_once('=').unwrap();
                w.metadata.entry(id.to_string()).or_default().insert(k.to_string(), v.to_string());
                Out::ok("")
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
            // spira-claim `fayth-ready --json`: the lifecycle machine's ready rows. bd's own
            // status plays no part, so a row bd shows in_progress is offered like any other.
            "_aeon_ready_set" => {
                if w.ready_fails {
                    return Out::fail(1, "spira-claim: fayth_ready: cannot tell\n");
                }
                let rows: Vec<serde_json::Value> = w.ready.iter().map(|id| serde_json::from_str::<serde_json::Value>(&row_json(&w, id)).unwrap()[0].clone()).collect();
                Out::ok(serde_json::Value::Array(rows).to_string())
            }
            "aeon_count" => Out::ok("0"),
            "fayth_free" => Out::ok("1"),
            "_aeon_rebase" => Out::ok(""),
            "_aeon_thrash_meta" => Out::ok("\n\n\n"),
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
            "bead_is_work_type" => {
                if ["task", "bug", "feature", "chore"].contains(&args[0].as_str()) {
                    Out::ok("")
                } else {
                    Out::fail(1, "")
                }
            }
            "bead_is_decision_type" => {
                if args[0] == "decision" {
                    Out::ok("")
                } else {
                    Out::fail(1, "")
                }
            }
            "lc_bead_verified" => Out::fail(1, ""),
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
        if prog == "spira-lc" && args.first().map(String::as_str) == Some("holds") {
            return Out::ok(self.0.lock().unwrap().holds.get(&args[1]).cloned().unwrap_or_default());
        }
        if prog == "spira-lc" && args.first().map(String::as_str) == Some("content-on-base") && self.0.lock().unwrap().fail_content_on_base {
            return Out::fail(1, "spira-lc content-on-base: stub refusal");
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
    _dir: testkit::TempDir,
    home: PathBuf,
    run: PathBuf,
    repo: PathBuf,
    /// A directory holding a stub `work` binary, so the restricted-env resolution
    /// (`restrict::work_bin_dir`, sp-zpaq0) finds one exactly as the real release's
    /// `bin/` does — without it, every `enforce=true` test would exercise the "work is
    /// not on PATH" refusal instead of the restriction itself.
    bin: PathBuf,
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
    let dir = testkit::TempDir::new(&format!("aeon-run-{name}"));
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
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let work_stub = bin.join("work");
    // testkit::write_exe, never fs::write + set_permissions (sp-os3of): this fixture is
    // execed by name off PATH by every test built on Fx, so an ETXTBSY from a concurrent
    // fork elsewhere in the binary would not be an isolated flake.
    testkit::write_exe(&work_stub, "#!/bin/sh\nexit 0\n");
    // The release's model-bin/ (sp-zf4q3): the only directory the model's PATH may name,
    // holding `work` and nothing else — a sibling of bin/, linked the way the release
    // builder links it.
    let model_bin = dir.join(spira_config::release_env::MODEL_BIN_DIR);
    std::fs::create_dir_all(&model_bin).unwrap();
    std::os::unix::fs::symlink("../bin/work", model_bin.join("work")).unwrap();
    let w: W = Arc::new(Mutex::new(World::default()));
    Fx { _dir: dir, home, run, repo, bin, w }
}

struct Outcome {
    code: i32,
    log: String,
    ledger: String,
    w: W,
    seen: Vec<SessionSpec>,
    /// Seconds the summon jitter slept.
    slept: usize,
}

fn go(f: &Fx, labels: &str, extra: &[(&str, &str)], enforce: bool, mode: Mode, seam_answers: BTreeMap<&'static str, Out>, act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync>) -> Outcome {
    go_as("builder", f, labels, extra, enforce, mode, seam_answers, act)
}

/// `go`, with the fayth NAME parameterized (sp-1zxru-2: the no-progress fix is a property
/// of decide::disposition/teardown.rs, not of any one persona, so a non-"builder" fayth
/// needs covering too — `an_ops_lane_aeons_no_progress_exit_is_held_too` is the one caller;
/// it must have already written `chamber/<fayth_name>.md`/`.fayth` into `f.home`, same as
/// `fx()` does for "builder").
#[allow(clippy::too_many_arguments)]
fn go_as(fayth_name: &str, f: &Fx, labels: &str, extra: &[(&str, &str)], enforce: bool, mode: Mode, seam_answers: BTreeMap<&'static str, Out>, act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync>) -> Outcome {
    let mut vars: BTreeMap<String, String> = [
        ("SPIRA_RUN", f.run.display().to_string()),
        ("SPIRA_DB", "/db".to_string()),
        ("SPIRA_ASK_LABEL", "needs-ryan".to_string()), // literal-ok: fixture/fallback
        ("SPIRA_WORLD_STOP_LABEL", "world-stop".to_string()), // literal-ok: test fixture
        ("SPIRA_TESTDB_LIB", "spira/testdb.sh".to_string()),
        ("SPIRA_TRACE_MARK", "=== spira attempt".to_string()),
        ("SPIRA_CLAIM_RETRIES", "1".to_string()),
        ("SPIRA_CLAIM_RETRY_DELAY_S", "0".to_string()),
        ("FAYTH_LABELS", labels.to_string()),
        // spira_config::repos (sp-37rmg): no registered-repository fixture here, so the
        // home repo resolves through the SPIRA_REPO override exactly as the old FakeSeam's
        // "repo_root"/"repo_land"/"spira_home_repo" answers always did — f.repo, "fixture",
        // "push". A test wanting an UNMAPPED repo (there is exactly one) cancels the
        // override via `extra` instead (SPIRA_REPO_DERIVED == SPIRA_REPO).
        ("SPIRA_HOME_REPO", "fixture".to_string()),
        ("SPIRA_REPO", f.repo.display().to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    for (k, v) in extra {
        vars.insert(k.to_string(), v.to_string());
    }
    let mut base = BTreeMap::new();
    base.insert("PATH".to_string(), format!("{}:{}", f.bin.display(), std::env::var("PATH").unwrap_or_default()));
    base.insert("GH_TOKEN".to_string(), "secret".to_string());
    for (k, v) in [("GIT_AUTHOR_NAME", "t"), ("GIT_AUTHOR_EMAIL", "t@t"), ("GIT_COMMITTER_NAME", "t"), ("GIT_COMMITTER_EMAIL", "t@t")] {
        base.insert(k.into(), v.into());
    }
    let snap = Snapshot {
        env: base.clone(),
        vars: vars.clone(),
        ready_args: vec!["ready".into(), "--limit".into(), "0".into()],
        claim_exclude: "spira-poison".into(),
    };
    let env = Env::new(base.clone(), base);
    let conf = Conf::new(&snap, &f.home);
    let fayth = Fayth::from_vars(fayth_name, &vars);
    let bd = FakeBd(Arc::clone(&f.w));
    let seam = FakeSeam { w: Arc::clone(&f.w), answers: seam_answers };
    let git = RealGit { env: &env };
    let exec = FakeExec(Arc::clone(&f.w));
    let launcher = FakeLauncher { w: Arc::clone(&f.w), seen: Mutex::new(vec![]), act };
    let sink = MemSink::default();
    let clock = crate::util::now_epoch;
    let slept = std::sync::atomic::AtomicUsize::new(0);
    let sleep = |_: std::time::Duration| {
        slept.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    };
    let dry = mode == Mode::DryRun;
    let cwd = std::env::current_dir().unwrap();
    let code = {
        let mut run = Run {
            d: Deps { bd: &bd, seam: &seam, git: &git, exec: &exec, launcher: &launcher, sink: &sink, env: &env, clock: &clock, sleep: &sleep },
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
            claim_bin: "spira-claim".into(),
            stop: Arc::new(Stop::default()),
            hb_shutdown: Arc::new(AtomicBool::new(false)),
            hb_done: Arc::new(AtomicBool::new(false)),
            fayth_file: f.home.join(format!("chamber/{fayth_name}.fayth")),
            s: State::default(),
        };
        run.main()
    };
    let _ = std::env::set_current_dir(cwd);
    let seen = launcher.seen.lock().unwrap().clone();
    Outcome { code, log: sink.all(), ledger: std::fs::read_to_string(f.run.join("aeon-ledger.log")).unwrap_or_default(), w: Arc::clone(&f.w), seen, slept: slept.load(std::sync::atomic::Ordering::SeqCst) }
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
    // In-process now (wave 4.26): a real pause file replaces the old FakeSeam stub for
    // `_aeon_capacity_paused`.
    let now = crate::util::now_epoch();
    std::fs::write(f2.run.join("capacity-pause"), format!("{} iso why\n", now + 321)).unwrap();
    let o = go(&f2, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), no_session());
    assert_eq!(ledger_lines(&o)[1], "awake builder paused");
    assert!(o.log.contains("out of capacity for another 32") && o.log.contains("s — claiming nothing"), "{}", o.log);
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
fn binary_presence_alone_never_selects_the_restricted_path() {
    let f = fx("enf-off");
    seed(&f, "sp-l");
    let extra: Vec<(&str, &str)> = Vec::new();
    let o = go(&f, "spira,plan", &extra, false, Mode::Claim, BTreeMap::new(), commits_and_closes());
    let w = o.w.lock().unwrap();
    assert!(!w.seam_calls.iter().any(|c| c.0 == "lc_claim_bead"), "enforce off: no lifecycle CAS");
    assert!(!o.seen[0].env.contains_key("SPIRA_WORK_BEAD_ID"), "enforce off: the model is not run under the restricted environment");
    let task = std::fs::read_to_string(f.run.join("sp-l.task.md")).unwrap();
    assert!(!task.contains("You have no `bd`") && task.contains("bd -C /db close sp-l --reason-file -"));
}

#[test]
fn enforce_claims_through_the_machine_and_restricts_the_model() {
    let f = fx("enf-on");
    seed(&f, "sp-r");
    let extra: Vec<(&str, &str)> = Vec::new();
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
    assert_eq!(o.seen[0].env.get("SPIRA_WORK_BEAD_ID").map(|s| s.as_str()), Some("sp-r"), "the model runs bound to this bead");
    assert!(!o.seen[0].env.contains_key("SPIRA_DB"), "the restricted env never carries SPIRA_DB");
    assert!(!o.seen[0].env.contains_key("GH_TOKEN"), "the restricted env never carries GH_TOKEN");
    let task = std::fs::read_to_string(f.run.join("sp-r.task.md")).unwrap();
    assert!(task.contains("**You have no `bd`.**"));
    assert!(ledger_lines(&o).last().unwrap().contains("status=submitted"));
    assert!(!w.labels["sp-r"].contains("spira-submitted"), "no bd-close reinterpretation on the restricted path");
}

/// sp-zf4q3: the model's PATH names the release's model-bin/ (only `work`), never bin/ —
/// the directory that merely holds `work` also holds ~40 tools that call bd themselves.
#[test]
fn the_restricted_path_names_model_bin_and_never_the_full_bin_dir() {
    let f = fx("enf-model-bin");
    seed(&f, "sp-mb");
    let extra: Vec<(&str, &str)> = Vec::new();
    let mut a = BTreeMap::new();
    a.insert("lc_bead_verified", Out::ok(""));
    let o = go(&f, "spira,plan", &extra, true, Mode::Claim, a, commits_and_closes());
    let path = o.seen[0].env.get("PATH").cloned().unwrap_or_default();
    let model_bin = f.bin.parent().unwrap().join(spira_config::release_env::MODEL_BIN_DIR);
    let dirs: Vec<&str> = path.split(':').collect();
    assert!(dirs.contains(&model_bin.to_str().unwrap()), "PATH must name model-bin: {path}");
    assert!(!dirs.contains(&f.bin.to_str().unwrap()), "PATH must never name the full bin dir: {path}");
}

/// sp-zf4q3: a release with no model-bin/ refuses the session (fail-closed), naming the
/// override, rather than falling back to the full bin dir.
#[test]
fn a_release_without_model_bin_refuses_the_session() {
    let f = fx("enf-no-model-bin");
    seed(&f, "sp-nmb");
    std::fs::remove_dir_all(f.bin.parent().unwrap().join(spira_config::release_env::MODEL_BIN_DIR)).unwrap();
    let extra: Vec<(&str, &str)> = Vec::new();
    let mut a = BTreeMap::new();
    a.insert("lc_bead_verified", Out::ok(""));
    let o = go(&f, "spira,plan", &extra, true, Mode::Claim, a, commits_and_closes());
    assert!(o.seen.is_empty(), "no session may start without model-bin");
    assert!(o.log.contains(spira_config::release_env::MODEL_BIN_OVERRIDE_ENV), "the refusal names its override: {}", o.log);
}

#[test]
fn a_refused_lifecycle_claim_is_another_aeons_bead_and_is_left_alone() {
    let f = fx("enf-refused");
    seed(&f, "sp-z");
    let extra: Vec<(&str, &str)> = Vec::new();
    let mut a = BTreeMap::new();
    a.insert("lc_claim_bead", Out::fail(3, ""));
    let o = go(&f, "spira,plan", &extra, true, Mode::Claim, a, no_session());
    assert_eq!(o.code, 0);
    assert_eq!(ledger_lines(&o)[1], "awake builder idle", "{:?}", ledger_lines(&o));
    assert!(o.log.contains("refused ranked candidate sp-z"), "{}", o.log);
    let w = o.w.lock().unwrap();
    assert!(!w.seam_calls.iter().any(|c| c.0 == "release_own_claim"), "never release a claim this aeon does not hold");
}

/// sp-860zj: the lifecycle row is the claim. Under enforce the aeon reads its ready set from
/// the machine and claims with a Claim event alone — bd's `update --claim` is never run, so
/// a bead bd shows in_progress under a dead aeon's name is claimed like any READY row.
#[test]
fn enforce_claims_with_the_lifecycle_row_alone_and_never_bds_claim() {
    let f = fx("enf-row");
    seed(&f, "sp-ip");
    f.w.lock().unwrap().status.insert("sp-ip".into(), "in_progress".into());
    let mut a = BTreeMap::new();
    a.insert("lc_bead_verified", Out::ok(""));
    let o = go(&f, "spira,plan", &[], true, Mode::Claim, a, commits_and_closes());
    assert_eq!(ledger_lines(&o)[1], "awake builder sp-ip", "{}", o.log);
    let w = o.w.lock().unwrap();
    assert!(w.seam_calls.iter().any(|c| c.0 == "_aeon_ready_set"), "the ready set is the machine's");
    assert!(!w.bd_calls.iter().any(|c| c.iter().any(|a| a == "--claim")), "no bd claim: {:?}", w.bd_calls);
    assert!(!w.bd_calls.iter().any(|c| c.first().map(String::as_str) == Some("ready")), "bd's ready query is not read");
    assert!(!w.bd_calls.iter().any(|c| c.first().map(String::as_str) == Some("heartbeat")), "no bd lease to renew");
}

/// sp-2jf0a: under enforce the lifecycle row's lease is the claim's, and nothing renewed it
/// once sp-860zj dropped `bd heartbeat` — the stale-lease reaper cleared every long session.
/// The heartbeat now renews it on every beat, as the claim's own holder, to a deadline that
/// advances; and never through bd.
#[test]
fn enforce_renews_the_lifecycle_lease_on_every_heartbeat_while_the_session_runs() {
    let f = fx("enf-renew");
    seed(&f, "sp-lr");
    let mut a = BTreeMap::new();
    a.insert("lc_bead_verified", Out::ok(""));
    let renews = |w: &W| {
        w.lock().unwrap().exec_calls.iter().filter(|(p, a, _)| p == "spira-lc" && a.first().map(String::as_str) == Some("renew")).map(|(_, a, _)| a.clone()).collect::<Vec<_>>()
    };
    // The session outlives three heartbeats, then commits and finishes.
    let act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> = Box::new(move |spec, w, stop| {
        let t0 = std::time::Instant::now();
        while renews(w).len() < 3 && t0.elapsed() < std::time::Duration::from_secs(20) && stop.signalled().is_none() {
            crate::run::append(&spec.log, "{\"type\":\"assistant\"}\n");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        commits_and_closes()(spec, w, stop)
    });
    let o = go(&f, "spira,plan", &[("FAYTH_HEARTBEAT_SECONDS", "1")], true, Mode::Claim, a, act);
    assert_eq!(ledger_lines(&o)[1], "awake builder sp-lr", "{}", o.log);
    let w = o.w.clone();
    let r = renews(&w);
    assert!(r.len() >= 3, "the lease was renewed on every beat of a session three beats long: {r:?}\n{}", o.log);
    let w = w.lock().unwrap();
    let holder = w.seam_calls.iter().find(|c| c.0 == "lc_claim_bead").map(|c| c.1[1].clone()).expect("claimed");
    let mut last = 0i64;
    for args in &r {
        assert_eq!((args[1].as_str(), args[2].as_str()), ("sp-lr", holder.as_str()), "renewed as the claim's own holder: {args:?}");
        let until: i64 = args[3].parse().unwrap();
        assert!(until >= last, "the deadline never goes back: {r:?}");
        assert!(until > crate::util::now_epoch() - 60, "a deadline of now + lease, not the claim's: {until}");
        last = until;
    }
    assert!(!w.bd_calls.iter().any(|c| c.first().map(String::as_str) == Some("heartbeat")), "no bd lease under enforce");
}

#[test]
fn enforce_with_an_unreachable_machine_is_claim_error_not_idle() {
    let f = fx("enf-down");
    seed(&f, "sp-d");
    let mut a = BTreeMap::new();
    a.insert("lc_claim_bead", Out::fail(2, ""));
    let o = go(&f, "spira,plan", &[], true, Mode::Claim, a, no_session());
    assert_eq!(o.code, 1);
    assert_eq!(ledger_lines(&o)[1], "awake builder claim-error lifecycle machine unreachable");
}

#[test]
fn enforce_releases_a_bead_held_between_the_ready_read_and_the_claim() {
    let f = fx("enf-hold");
    seed(&f, "sp-h");
    f.w.lock().unwrap().holds.insert("sp-h".into(), "poison".into());
    let o = go(&f, "spira,plan", &[], true, Mode::Claim, BTreeMap::new(), no_session());
    assert_eq!(o.code, 0);
    assert!(ledger_lines(&o)[2].contains("status=hold-raced"), "{:?}", ledger_lines(&o));
    assert!(o.w.lock().unwrap().seam_calls.iter().any(|c| c.0 == "release_own_claim"));
}

/// No tool paths to hand in any more (sp-gypjk): spira-lc and work are found by name.
fn lc_bin_extra(_f: &Fx) -> Vec<(String, String)> {
    Vec::new()
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
    let base_sha: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let base_sha_cl = Arc::clone(&base_sha);
    let act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> =
        Box::new(move |spec, _w, _| {
            assert!(spec.cwd.join("a.txt").is_file(), "the worktree must contain A's own commit");
            // The worktree's HEAD right now, before this session's own commit, is the base
            // commit aeon built (main merged with A's certified tip) — captured here so the
            // "ahead of base" count below is against that merge, not main.
            *base_sha_cl.lock().unwrap() = rev_parse(&spec.cwd, "HEAD");
            std::fs::write(spec.cwd.join("b.txt"), "from B\n").unwrap();
            git(&spec.cwd, &["add", "b.txt"]);
            git(&spec.cwd, &["commit", "-qm", "sp-b work"]);
            0
        });
    let o = go(&f, "spira,plan", &extra, true, Mode::Claim, BTreeMap::new(), act);
    assert_eq!(o.code, 0, "{}", o.log);
    assert_eq!(o.seen.len(), 1, "the session must have started — the merge must not have conflicted");

    // design stacked-dependents-2026-09-28's own test-strategy item 1: B's "ahead of base"
    // is B only — A's commit is already inside the base merge, so it must not be counted as
    // B's own work.
    let base = base_sha.lock().unwrap().clone();
    assert!(!base.is_empty(), "the act closure must have captured the base commit");
    let parents = std::process::Command::new("git").arg("-C").arg(&f.repo).args(["log", "--format=%P", "-1", &base]).output().unwrap();
    let parents = String::from_utf8(parents.stdout).unwrap();
    assert_eq!(parents.split_whitespace().count(), 2, "the base commit must be a two-parent merge of main and sp-a's tip: {parents:?}");
    let ahead = std::process::Command::new("git")
        .arg("-C")
        .arg(&f.repo)
        .args(["rev-list", "--count", &format!("{base}..spira/sp-b")])
        .output()
        .unwrap();
    let ahead_n: usize = String::from_utf8(ahead.stdout).unwrap().trim().parse().unwrap();
    assert_eq!(ahead_n, 1, "B's own commits ahead of base must be exactly its own work, not A's");
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
    assert!(w.seam_calls.iter().any(|c| c.0 == "release_own_claim" && c.1[0] == "sp-c"), "the claim is handed back");
    assert!(!w.seam_calls.iter().any(|c| c.0 == "bead_reopen" || c.0 == "bump_requeue"), "the dependent stays held, not requeued as a fault");
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
    // Cancel go()'s default SPIRA_REPO override (spira_config::repos::Registry treats it as
    // "deliberate" only when it differs from SPIRA_REPO_DERIVED) so "fixture" falls through
    // to the map lookup — absent here — and repo_root refuses, same as the old FakeSeam's
    // `"repo_root" => Out::fail(1, "")` answer.
    let repo_derived = f.repo.display().to_string();
    let o = go(&f, "spira,plan", &[("SPIRA_REPO_DERIVED", repo_derived.as_str())], false, Mode::Claim, BTreeMap::new(), no_session());
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

/// The model's stand-in for a close with nothing committed: closes the bead, optionally
/// recording a successor, writes a result record.
fn closes_only(supersede: bool) -> Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> {
    Box::new(move |spec, w, _| {
        let id = spec.env.get("BEAD_ID").unwrap().clone();
        let mut w = w.lock().unwrap();
        w.status.insert(id.clone(), "closed".into());
        if supersede {
            w.supersedes.insert(id);
        }
        crate::run::append(&spec.log, "{\"type\":\"result\",\"duration_ms\":3000,\"num_turns\":3,\"total_cost_usd\":0.5}\n");
        0
    })
}

fn seed_typed(f: &Fx, id: &str, ty: &str, extra_labels: &[&str]) {
    seed(f, id);
    let mut w = f.w.lock().unwrap();
    w.issue_type.insert(id.into(), ty.into());
    w.labels.get_mut(id).unwrap().extend(extra_labels.iter().map(|l| l.to_string()));
}

#[test]
fn a_work_bead_closed_with_an_empty_branch_stays_closed_and_is_not_converted() {
    let f = fx("emptybranch");
    seed(&f, "sp-e");
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), closes_only(false));
    assert_eq!(o.code, 0, "{}", o.log);
    assert!(o.log.contains("carries no commit of its own ahead of"), "{}", o.log);
    assert!(l_done(&o).contains("status=closed"), "{}", o.ledger);
    let w = o.w.lock().unwrap();
    assert_eq!(w.status["sp-e"], "closed");
    assert!(!w.labels["sp-e"].contains("spira-submitted"));
    assert!(!w.seam_calls.iter().any(|c| c.0 == "bead_reopen"), "{:?}", w.seam_calls);
    assert!(!o.log.contains("REOPENED"), "{}", o.log);
}

#[test]
fn a_superseded_work_bead_with_no_commit_stays_closed_and_is_not_converted() {
    let f = fx("supersedednocommit");
    seed(&f, "sp-s");
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), closes_only(true));
    assert!(o.log.contains("closed with nothing committed and NOT reopened — superseded"), "{}", o.log);
    assert!(o.log.contains("closed a superseded work bead — not converted"), "{}", o.log);
    let w = o.w.lock().unwrap();
    assert_eq!(w.status["sp-s"], "closed");
    assert!(!w.labels["sp-s"].contains("spira-submitted"));
    assert!(!w.seam_calls.iter().any(|c| c.0 == "bead_reopen"));
}

#[test]
fn a_delivers_labelled_work_bead_with_no_commit_stays_closed_and_is_not_converted() {
    let f = fx("deliversaction");
    seed_typed(&f, "sp-d", "task", &["delivers:action"]);
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), closes_only(false));
    assert!(o.log.contains("closed with nothing committed and NOT reopened"), "{}", o.log);
    let w = o.w.lock().unwrap();
    assert_eq!(w.status["sp-d"], "closed");
    assert!(!w.labels["sp-d"].contains("spira-submitted"));
    assert!(!w.seam_calls.iter().any(|c| c.0 == "bead_reopen"));
    assert!(!o.log.contains("REOPENED"), "{}", o.log);
}

#[test]
fn a_non_work_bead_closed_with_nothing_committed_is_reopened_by_the_verdict() {
    let f = fx("spike");
    seed_typed(&f, "sp-k", "spike", &[]);
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), closes_only(false));
    assert!(o.log.contains("sp-k REOPENED — closed with nothing committed"), "{}", o.log);
    let w = o.w.lock().unwrap();
    assert!(w.seam_calls.iter().any(|c| c.0 == "bead_reopen" && c.1[0] == "sp-k" && c.1[1] == "closed-without-commit"), "{:?}", w.seam_calls);
    assert_eq!(w.status["sp-k"], "open");
    assert!(!w.labels["sp-k"].contains("spira-submitted"));
}

#[test]
fn a_decision_bead_closed_with_nothing_committed_stands_closed() {
    let f = fx("decision");
    seed_typed(&f, "sp-d", "decision", &[]);
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), closes_only(false));
    assert!(o.log.contains("decision type, no commit expected, close stands"), "{}", o.log);
    let w = o.w.lock().unwrap();
    assert!(!w.seam_calls.iter().any(|c| c.0 == "bead_reopen"), "{:?}", w.seam_calls);
    assert_eq!(w.status["sp-d"], "closed");
}

fn l_done(o: &Outcome) -> String {
    ledger_lines(o).into_iter().find(|l| l.starts_with("done ")).unwrap_or_default()
}

fn brief_fx(name: &str, extra: &[(&str, &str)]) -> String {
    let f = fx(name);
    std::fs::write(f.home.join("chamber/builder.md"), "sys\n<!-- task -->\nwork {{BEAD_ID}}\n{{DEADLINE}}\n{{PARK}}\n").unwrap();
    seed(&f, "sp-b");
    go(&f, "spira,plan", extra, false, Mode::Claim, BTreeMap::new(), closes_only(false));
    std::fs::read_to_string(f.run.join("sp-b.task.md")).unwrap()
}

#[test]
fn a_persona_with_a_wall_is_told_its_deadline_inside_the_window() {
    let before = crate::util::now_epoch();
    let task = brief_fx("walled", &[("FAYTH_TIMEOUT_SECONDS", "300")]);
    assert!(!task.contains("{{"), "{task}");
    assert!(task.contains("This session is killed at"), "{task}");
    assert!(!task.contains("no wall-clock deadline"), "{task}");
    let left: i64 = task.split("— ").filter_map(|p| p.split_once(" seconds from now")).filter_map(|(n, _)| n.trim().parse().ok()).next().expect(&task);
    assert!(left > 0 && left <= 300, "{left}");
    let epoch: i64 = task.split("echo $(( ").nth(1).and_then(|r| r.split_whitespace().next()).and_then(|n| n.parse().ok()).expect(&task);
    assert!(epoch > before && epoch <= crate::util::now_epoch() + 300, "{epoch}");
}

#[test]
fn a_persona_without_a_wall_is_told_it_has_no_clock() {
    let task = brief_fx("nowall", &[]);
    assert!(!task.contains("{{"), "{task}");
    assert!(task.contains("no wall-clock deadline"), "{task}");
    assert!(!task.contains("This session is killed at"), "{task}");
}

#[test]
fn the_brief_names_supersede_for_already_done_work() {
    let task = brief_fx("alreadydone", &[]);
    assert!(task.contains("bd -C /db supersede sp-b --with <successor-id>"), "{task}");
    assert!(task.contains("Verify the successor actually landed"), "{task}");
}

// sp-1zxru: aeon-ledger.log since 2026-10-02T07:36Z showed sp-6a4rb/sp-0k18y/etc resumed
// every ~75s by the SAME aeon — a session that ends in_progress with no commit was treated
// exactly like a real failed attempt (charged, immediately reclaimable). The three tests
// below pin the fix: no commit is a no-progress exit (held, not resumed, not charged, not
// asked about); a commit still charges (a real failed attempt still counts); and a streak
// of no-progress exits at the cap is routed to the Concierge, never to Ryan.

#[test]
fn a_session_that_leaves_the_bead_open_with_no_commit_is_a_no_progress_exit_held_not_resumed() {
    let f = fx("unlanded");
    seed(&f, "sp-o");
    // A believable trace (acted, no API error) so the native session_outcome classifies it
    // as `unlanded` rather than `refused` — this is the exact shape test-attempts.sh's
    // "clean.log" fixture asserted against the bash classifier. No commit lands on the
    // branch (the shape aeon-ledger.log showed for sp-0k18y: "Nothing has changed since the
    // last check, so I'm leaving the bead open with a note").
    let act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> = Box::new(|spec, _, _| {
        crate::run::append(
            &spec.log,
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Nothing has changed since the last check, so I'm leaving the bead open with a note\"}]}}\n\
             {\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{}}]}}\n\
             {\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"num_turns\":1}\n",
        );
        1
    });
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), act);
    assert_eq!(o.code, 1, "{}", o.log);
    let l = ledger_lines(&o);
    assert!(l[2].starts_with("done builder sp-o rc=1 status=in_progress"), "{l:?}");
    let w = o.w.lock().unwrap();
    assert!(
        w.notes.iter().any(|(_, n)| n.starts_with("No progress (unlanded): Nothing has changed since the last check")),
        "the note carries the aeon's own last word: {:?}",
        w.notes
    );
    assert_eq!(w.status["sp-o"], "open", "released, not left claimed");
    // Exempt, not charged: the events fold's `unjudged-` prefix, same as every other
    // never-judged disposition, so CHECK 4 never counts this toward the attempts ask.
    assert!(w.seam_calls.iter().any(|c| c.0 == "bump_requeue" && c.1 == vec!["sp-o", "unjudged-no-progress"]), "{:?}", w.seam_calls);
    assert!(
        w.bd_calls.iter().any(|c| c.first().map(String::as_str) == Some("update") && c.contains(&"--status".to_string()) && c.contains(&"open".to_string())),
        "a timed hold must leave status open so it releases itself: {:?}",
        w.bd_calls
    );
    // Held, not resumed: a real defer, not just release() (which alone put it right back
    // in front of the next summon — the whole bug).
    assert!(
        w.bd_calls.iter().any(|c| c.first().map(String::as_str) == Some("update") && c.contains(&"--defer".to_string())),
        "{:?}",
        w.bd_calls
    );
    // The streak this hold counts is the SAME tip-keyed counter real thrash uses — reused,
    // not a second one built in parallel.
    assert!(w.seam_calls.iter().any(|c| c.0 == "thrash_streak_bump" && c.1[0] == "sp-o"), "{:?}", w.seam_calls);
}

#[test]
fn a_committed_but_still_open_session_still_charges_a_real_attempt() {
    let f = fx("unlanded-committed");
    seed(&f, "sp-p");
    // A commit naming the bead DOES land on the branch this time — the session made
    // progress and still left the bead open. `law-attempts-count-the-harness` cuts the
    // other way here: this is a verdict about the work, so it counts.
    let act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> = Box::new(|spec, _, _| {
        let id = spec.env.get("BEAD_ID").unwrap().clone();
        std::fs::write(spec.cwd.join("f"), "partial work\n").unwrap();
        git(&spec.cwd, &["commit", "-qam", &format!("{id} — partial, not done")]);
        crate::run::append(&spec.log, "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{}}]}}\n{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"num_turns\":1}\n");
        1
    });
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), act);
    assert_eq!(o.code, 1, "{}", o.log);
    let w = o.w.lock().unwrap();
    assert!(
        w.notes.iter().any(|(_, n)| n.starts_with("Unlanded (unlanded): the session ran to its own end")),
        "{:?}",
        w.notes
    );
    assert!(!w.seam_calls.iter().any(|c| c.0 == "bump_requeue"), "a charged attempt writes no exempt requeue cause: {:?}", w.seam_calls);
    assert!(!w.seam_calls.iter().any(|c| c.0 == "thrash_streak_bump"), "committed work is not a no-progress exit: {:?}", w.seam_calls);
    assert!(!w.bd_calls.iter().any(|c| c.contains(&"--defer".to_string())), "a real attempt is not held for a backoff: {:?}", w.bd_calls);
    assert_eq!(w.status["sp-p"], "open");
}

#[test]
fn a_no_progress_streak_at_the_cap_is_routed_to_the_concierge_not_ryan() {
    let f = fx("unlanded-cap");
    seed(&f, "sp-q");
    let act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> = Box::new(|spec, _, _| {
        crate::run::append(
            &spec.log,
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Still blocked on the same missing fixture.\"}]}}\n\
             {\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{}}]}}\n\
             {\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"num_turns\":1}\n",
        );
        1
    });
    // literal-ok: SPIRA_THRASH_STREAK_CAP defaults to 2 (unset by this fixture's `vars`) —
    // a canned streak at the cap stands in for the Nth consecutive no-progress exit at the
    // same tip without actually running N sessions.
    let answers = BTreeMap::from([("thrash_streak_bump", Out::ok("2"))]);
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, answers, act);
    assert_eq!(o.code, 1, "{}", o.log);
    let w = o.w.lock().unwrap();
    assert!(
        !w.bd_calls.iter().any(|c| c.contains(&"--defer".to_string())),
        "at the cap this routes to the Concierge instead of deferring again: {:?}",
        w.bd_calls
    );
    let mail = w.exec_calls.iter().find(|c| c.0 == "env" && c.1.iter().any(|a| a == "mail")).unwrap_or_else(|| panic!("no mail send concierge: {:?}", w.exec_calls));
    assert_eq!(mail.1[0], "SPIRA_MAIL_ALLOW_BLOCKING=1", "the question must wire a blocking edge so the bead is unclaimable");
    assert_eq!(mail.1[1..3], ["mail", "send"]);
    assert_eq!(mail.1[3], "concierge", "never operator/ryan — the Concierge's own mailbox");
    assert!(mail.1.windows(2).any(|p| p == ["--kind", "question"]), "{:?}", mail.1);
    let bead_at = mail.1.iter().position(|a| a == "--bead").expect("--bead flag");
    assert_eq!(mail.1[bead_at + 1], "sp-q");
    let subj_at = mail.1.iter().position(|a| a == "--subject").expect("--subject flag");
    assert!(mail.1[subj_at + 1].starts_with("aeon cannot progress:"), "{:?}", mail.1);
    assert!(w.notes.iter().any(|(_, n)| n.contains("routed to the Concierge")), "{:?}", w.notes);
    assert!(w.seam_calls.iter().any(|c| c.0 == "bump_requeue" && c.1 == vec!["sp-q", "unjudged-no-progress"]), "still exempt past the cap: {:?}", w.seam_calls);
    assert_eq!(w.status["sp-q"], "open", "released, not left claimed, not re-held");
}

// sp-1zxru-2: round 221 (~09:20Z) still thrashed sp-iku03 and sp-al5ng — branches already
// ahead from an EARLIER session, with THIS session adding nothing, kept reading `committed`
// (verdict_committed: a commit naming the bead ANYWHERE in the window) as true forever, so
// disposition never reached NoteKey::NoProgress no matter how many more sessions landed
// nothing. The fix: the Unlanded/NoProgress split reads `tip_moved` (THIS session's own
// branch-tip movement, decide::tip_moved), not `committed`.

#[test]
fn a_branch_already_ahead_with_no_new_commit_this_session_is_still_held_not_charged() {
    let f = fx("already-ahead");
    // An EARLIER session already committed and left the bead open — sp-iku03's own shape
    // (aeon-ledger.log: branch +1 ahead, a commit naming the bead from ~2 minutes earlier).
    git(&f.repo, &["checkout", "-qb", "spira/sp-aa"]);
    std::fs::write(f.repo.join("f"), "an earlier session's work\n").unwrap();
    git(&f.repo, &["add", "f"]);
    git(&f.repo, &["commit", "-qm", "sp-aa — the work"]);
    git(&f.repo, &["checkout", "-q", "main"]);
    seed(&f, "sp-aa");
    // THIS session commits nothing — a believable trace (acted, no API error) so
    // session_outcome classifies it as unlanded, same shape sp-iku03's later summons used.
    let act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> = Box::new(|spec, _, _| {
        crate::run::append(&spec.log, "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{}}]}}\n{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"num_turns\":1}\n");
        1
    });
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), act);
    assert_eq!(o.code, 1, "{}", o.log);
    let w = o.w.lock().unwrap();
    assert!(
        w.notes.iter().any(|(_, n)| n.starts_with("No progress (unlanded):")),
        "a branch already ahead from an earlier session is not `committed` enough on its own — nothing moved THIS session: {:?}",
        w.notes
    );
    assert!(!w.notes.iter().any(|(_, n)| n.starts_with("Unlanded (unlanded):")), "{:?}", w.notes);
    assert!(w.seam_calls.iter().any(|c| c.0 == "bump_requeue" && c.1 == vec!["sp-aa", "unjudged-no-progress"]), "{:?}", w.seam_calls);
    assert!(
        w.bd_calls.iter().any(|c| c.first().map(String::as_str) == Some("update") && c.contains(&"--defer".to_string())),
        "held, not resumed: {:?}",
        w.bd_calls
    );
    assert_eq!(w.status["sp-aa"], "open");
}

// sp-al5ng's own shape: fayth "ops", not "builder" — the no-progress fix is a property of
// decide::disposition/teardown.rs, not of any one persona's code path, so an ops-lane aeon's
// no-progress exit must be held exactly the same way.
#[test]
fn an_ops_lane_aeons_no_progress_exit_is_held_too() {
    let f = fx("ops-no-progress");
    // go_as looks up chamber/<fayth_name>.md/.fayth by name (run.rs::write_prompt,
    // main.rs::fayth_file) — fx() only wrote "builder"'s; "ops" needs its own (content is
    // irrelevant to this test, same shape as fx()'s own builder.md/.fayth).
    std::fs::write(
        f.home.join("chamber/ops.md"),
        "You are ops. DB {{DB}}.\n<!-- task -->\n## The bead\n{{BEAD}}\nwork {{BEAD_ID}} in {{REPO}} on {{BRANCH}} ({{LANDING}})\n{{PARK}}\n{{FIXTURE}}\n## Finishing\n{{FINISH}}\n",
    )
    .unwrap();
    std::fs::write(f.home.join("chamber/ops.fayth"), "").unwrap();
    seed(&f, "sp-ao");
    let act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> = Box::new(|spec, _, _| {
        crate::run::append(&spec.log, "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{}}]}}\n{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"num_turns\":1}\n");
        1
    });
    let o = go_as("ops", &f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), act);
    assert_eq!(o.code, 1, "{}", o.log);
    let l = ledger_lines(&o);
    assert!(l[2].starts_with("done ops sp-ao rc=1 status=in_progress"), "{l:?}");
    let w = o.w.lock().unwrap();
    assert!(w.notes.iter().any(|(_, n)| n.starts_with("No progress (unlanded):")), "{:?}", w.notes);
    assert!(w.seam_calls.iter().any(|c| c.0 == "bump_requeue" && c.1 == vec!["sp-ao", "unjudged-no-progress"]), "{:?}", w.seam_calls);
    assert!(
        w.bd_calls.iter().any(|c| c.first().map(String::as_str) == Some("update") && c.contains(&"--defer".to_string())),
        "{:?}",
        w.bd_calls
    );
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
    // verdict() is native now (no seam call to prove it ran); its first act is always this
    // exact log line, so its absence proves the whole block was skipped.
    assert!(!o.log.contains("sp-s status="), "the verdict block is skipped, as bash's trap skipped it");
    assert!(!f.run.join("aeon-builder-sp-s.pid").exists());
}

#[test]
fn a_worktree_failure_is_a_pre_session_death() {
    let f = fx("presession");
    seed(&f, "sp-d");
    // Block worktree creation directly by pre-occupying the target path with a plain file:
    // `git worktree add` then refuses regardless of which base ref is in play.
    // qualify_base_ref can no longer be fed a bogus ref to force this — spira_config::repos'
    // port (sp-o88bx, "wave 4.12") only ever returns a ref it has itself verified exists.
    let work = f.run.join("worktree").join("sp-d");
    std::fs::create_dir_all(work.parent().unwrap()).unwrap();
    std::fs::write(&work, "occupied").unwrap();
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), no_session());
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

fn close_after_editing_description(stamp: bool) -> Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> {
    Box::new(move |spec, w, _| {
        std::fs::write(spec.cwd.join("f"), "mine\n").unwrap();
        git(&spec.cwd, &["commit", "-qam", "sp-e — the work"]);
        let mut w = w.lock().unwrap();
        w.descriptions.insert("sp-e".into(), "edited while claimed".into());
        if stamp {
            let h = bead::claimdesc::desc_hash(&row_json(&w, "sp-e")).unwrap();
            w.metadata.entry("sp-e".into()).or_default().insert(bead::claimdesc::HASH_KEY.into(), h);
        }
        w.status.insert("sp-e".into(), "closed".into());
        0
    })
}

#[test]
fn a_description_edited_since_claim_reopens_the_close_as_a_free_requeue() {
    let f = fx("descedit");
    seed(&f, "sp-e");
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), close_after_editing_description(false));
    let l = ledger_lines(&o);
    assert!(l[2].starts_with("done builder sp-e rc=0 status=requeue-desc-changed-since-claim"), "{l:?}\n{}", o.log);
    let w = o.w.lock().unwrap();
    assert!(w.metadata["sp-e"].contains_key(bead::claimdesc::HASH_KEY), "the claim stamps its baseline");
    assert!(w.seam_calls.iter().any(|c| c.0 == "bead_reopen" && c.1[1] == "desc-changed-since-claim"));
}

#[test]
fn an_acknowledged_description_edit_does_not_reopen_the_close() {
    let f = fx("descack");
    seed(&f, "sp-e");
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), close_after_editing_description(true));
    let w = o.w.lock().unwrap();
    assert!(!w.seam_calls.iter().any(|c| c.0 == "bead_reopen" && c.1[1] == "desc-changed-since-claim"), "{}", o.log);
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
fn a_close_behind_base_with_its_content_already_there_records_content_on_base() {
    let f = fx("a_close_behind_base_with_its_content_already_there_records_content_on_base");
    seed(&f, "sp-m");
    let mut a = BTreeMap::new();
    a.insert("_aeon_rebase", Out { code: 1, stdout: "f ".into(), stderr: String::new() });
    let repo = f.repo.clone();
    let act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> = Box::new(move |spec, w, _| {
        std::fs::write(spec.cwd.join("f"), "mine\n").unwrap();
        git(&spec.cwd, &["commit", "-qam", "sp-m — hand-landed"]);
        std::fs::write(repo.join("f"), "mine\n").unwrap();
        git(&repo, &["commit", "-qam", "someone else, same change"]);
        let mut w = w.lock().unwrap();
        w.status.insert("sp-m".into(), "closed".into());
        0
    });
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, a, act);
    let w = o.w.lock().unwrap();
    assert!(
        w.exec_calls.iter().any(|(prog, args, _)| prog == "spira-lc" && args.first().map(String::as_str) == Some("content-on-base") && args.get(1).map(String::as_str) == Some("sp-m")),
        "{:?}",
        w.exec_calls
    );
    assert!(o.log.contains("its content is already on"), "{}", o.log);
    assert!(!o.log.contains("FAILED"), "a successful event logs nothing alarming: {}", o.log);
}

#[test]
fn a_failed_content_on_base_event_is_logged_loudly_not_discarded() {
    let f = fx("a_failed_content_on_base_event_is_logged_loudly_not_discarded");
    seed(&f, "sp-m");
    let mut a = BTreeMap::new();
    a.insert("_aeon_rebase", Out { code: 1, stdout: "f ".into(), stderr: String::new() });
    let repo = f.repo.clone();
    let act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> = Box::new(move |spec, w, _| {
        std::fs::write(spec.cwd.join("f"), "mine\n").unwrap();
        git(&spec.cwd, &["commit", "-qam", "sp-m — hand-landed"]);
        std::fs::write(repo.join("f"), "mine\n").unwrap();
        git(&repo, &["commit", "-qam", "someone else, same change"]);
        let mut w = w.lock().unwrap();
        w.status.insert("sp-m".into(), "closed".into());
        w.fail_content_on_base = true;
        0
    });
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, a, act);
    let w = o.w.lock().unwrap();
    assert!(
        w.exec_calls.iter().any(|(prog, args, _)| prog == "spira-lc" && args.first().map(String::as_str) == Some("content-on-base") && args.get(1).map(String::as_str) == Some("sp-m")),
        "{:?}",
        w.exec_calls
    );
    assert!(o.log.contains("its content is already on"), "{}", o.log);
    assert!(o.log.contains("spira-lc content-on-base merge-tree:"), "{}", o.log);
    assert!(o.log.contains("FAILED"), "{}", o.log);
    assert!(o.log.contains("stub refusal"), "{}", o.log);
}

#[test]
fn sweep_runs_without_a_bead() {
    let f = fx("sweep");
    std::fs::write(f.home.join("chamber/builder.md"), "persona\n<!-- task -->\nstanding brief\n").unwrap();
    // A believable trace (acted, no API error) so the native session_outcome classifies it
    // as `unlanded` rather than `refused` — a bare result record with no tool_use is not an
    // attempt (see decide::tests::session_outcome_table).
    let act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> = Box::new(|spec, _, _| {
        crate::run::append(&spec.log, "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{}}]}}\n{\"type\":\"result\",\"num_turns\":1}\n");
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

#[test]
fn a_declared_core_statute_that_does_not_exist_refuses_the_start() {
    let f = fx("corefail");
    std::fs::write(f.home.join("chamber/builder.md"), "persona\n<!-- task -->\nstanding brief\n").unwrap();
    let cache = f.run.join("memories.json");
    std::fs::write(&cache, r#"{"law-real":"text"}"#).unwrap();
    let cache_s = cache.display().to_string();
    let act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> = Box::new(|_, _, _| 0);
    let o = go(&f, "spira,plan", &[("SPIRA_MEMORIES_CACHE", &cache_s), ("FAYTH_STATUTE_CORE", "law-slug-that-does-not-exist")], false, Mode::Sweep { prompt: Some("x".into()) }, BTreeMap::new(), act);
    assert_eq!(o.code, 1);
    assert!(o.log.contains("law-slug-that-does-not-exist"), "{}", o.log);
    assert!(o.seen.is_empty(), "no session may start");
    assert!(!f.run.join("sweep-builder-4242.task.md").exists());

    let f = fx("coreok");
    std::fs::write(f.home.join("chamber/builder.md"), "persona\n<!-- task -->\nstanding brief\n").unwrap();
    let cache = f.run.join("memories.json");
    std::fs::write(&cache, r#"{"law-real":"text"}"#).unwrap();
    let cache_s = cache.display().to_string();
    let act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> = Box::new(|_, _, _| 0);
    let o = go(&f, "spira,plan", &[("SPIRA_MEMORIES_CACHE", &cache_s), ("FAYTH_STATUTE_CORE", "law-real")], false, Mode::Sweep { prompt: Some("x".into()) }, BTreeMap::new(), act);
    assert!(!o.seen.is_empty(), "positive control: a resolvable core slug starts the session: {}", o.log);
}

// sp-crr3n: SPIRA_STATUTE_CORE_LOCAL is this installation's own addition to whatever core
// the persona already resolves (FAYTH_STATUTE_CORE here) — never shipped with the fayth,
// per law-harness-ships-mechanism-not-inventory. A slug it names renders in full exactly
// like a FAYTH_STATUTE_CORE one, and a missing one refuses the start the same way.
#[test]
fn spira_statute_core_local_renders_in_full_and_a_missing_one_refuses() {
    let f = fx("corelocalok");
    std::fs::write(f.home.join("chamber/builder.md"), "persona\n<!-- task -->\nstanding brief\n").unwrap();
    let cache = f.run.join("memories.json");
    std::fs::write(&cache, r#"{"law-real":"text","law-local":"local text"}"#).unwrap();
    let cache_s = cache.display().to_string();
    let act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> = Box::new(|_, _, _| 0);
    let o = go(
        &f,
        "spira,plan",
        &[("SPIRA_MEMORIES_CACHE", &cache_s), ("FAYTH_STATUTE_CORE", "law-real"), ("SPIRA_STATUTE_CORE_LOCAL", "law-local")],
        false,
        Mode::Sweep { prompt: Some("x".into()) },
        BTreeMap::new(),
        act,
    );
    assert!(!o.seen.is_empty(), "a resolvable local core slug, appended to the persona's own, starts the session: {}", o.log);

    let f = fx("corelocalfail");
    std::fs::write(f.home.join("chamber/builder.md"), "persona\n<!-- task -->\nstanding brief\n").unwrap();
    let cache = f.run.join("memories.json");
    std::fs::write(&cache, r#"{"law-real":"text"}"#).unwrap();
    let cache_s = cache.display().to_string();
    let act: Box<dyn Fn(&SessionSpec, &W, &Stop) -> i32 + Send + Sync> = Box::new(|_, _, _| 0);
    let o = go(
        &f,
        "spira,plan",
        &[("SPIRA_MEMORIES_CACHE", &cache_s), ("FAYTH_STATUTE_CORE", "law-real"), ("SPIRA_STATUTE_CORE_LOCAL", "law-gone-local")],
        false,
        Mode::Sweep { prompt: Some("x".into()) },
        BTreeMap::new(),
        act,
    );
    assert_eq!(o.code, 1);
    assert!(o.log.contains("law-gone-local"), "{}", o.log);
    assert!(o.seen.is_empty(), "a missing SPIRA_STATUTE_CORE_LOCAL slug refuses the start exactly like a missing FAYTH_STATUTE_CORE one");
}

// ---- admission and summon jitter (sp-f4ig1) -------------------------------------------

fn stub(f: &Fx, name: &str) -> PathBuf {
    let p = f.bin.join(name);
    // testkit::write_exe, never fs::write + set_permissions (sp-os3of).
    testkit::write_exe(&p, "#!/bin/sh\nexit 0\n");
    p
}

#[test]
fn an_agents_cargo_compiles_through_spira_admit_with_sccache_inside() {
    let f = fx("admit");
    seed(&f, "sp-adm");
    let admit = stub(&f, "spira-admit");
    let sccache = stub(&f, "sccache");
    let o = go(&f, "spira,plan", &[("SPIRA_SUMMON_JITTER", "0")], false, Mode::Claim, BTreeMap::new(), commits_and_closes());
    let env = &o.seen[0].env;
    let get = |k: &str| env.get(k).map(String::as_str);
    assert_eq!(get("RUSTC_WRAPPER"), Some(admit.to_str().unwrap()));
    assert_eq!(get("SPIRA_ADMIT_INNER"), Some(sccache.to_str().unwrap()));
    assert_eq!(get("SPIRA_ADMIT_WHO"), Some("sp-adm"));
    assert_eq!(get("SPIRA_RUN"), Some(f.run.to_str().unwrap()));
    assert!(!o.log.contains("not admitted"), "{}", o.log);
    assert_eq!(o.slept, 0, "SPIRA_SUMMON_JITTER=0 disables the jitter");
    assert!(!o.log.contains("summon jitter"));
}

#[test]
fn an_enforced_sessions_cargo_still_goes_through_spira_admit_and_sccache() {
    let f = fx("admit-enforced");
    seed(&f, "sp-admenf");
    let admit = stub(&f, "spira-admit");
    let sccache = stub(&f, "sccache");
    let o = go(&f, "spira,plan", &[("SPIRA_SUMMON_JITTER", "0"), ("SPIRA_SCCACHE_DAV_ADDR", "192.168.1.56:9431")], true, Mode::Claim, BTreeMap::new(), commits_and_closes());
    let env = &o.seen[0].env;
    let get = |k: &str| env.get(k).map(String::as_str);
    assert!(env.contains_key("SPIRA_WORK_BEAD_ID"), "the model must be restricted: {env:?}");
    assert_eq!(get("RUSTC_WRAPPER"), Some(admit.to_str().unwrap()));
    assert_eq!(get("SPIRA_ADMIT_INNER"), Some(sccache.to_str().unwrap()));
    assert_eq!(get("SPIRA_ADMIT_WHO"), Some("sp-admenf"));
    assert_eq!(get("SCCACHE_WEBDAV_ENDPOINT"), Some("http://192.168.1.56:9431"));
    assert_eq!(get("SCCACHE_WEBDAV_KEY_PREFIX"), Some("/"));
}

/// THE POSITIVE CONTROL (sp-xtdqi): `SPIRA_SCCACHE_DAV_ADDR`, resolved in-process into
/// `self.conf` exactly like every other config-file-only key (`merge_resolved_config`),
/// reaches an agent's own build as the two `SCCACHE_WEBDAV_*` vars — the fix for "a server
/// restarted by a gate silently comes back on the local-disk cache" named in the bead.
#[test]
fn a_configured_shared_store_reaches_an_agents_build_as_webdav_vars() {
    let f = fx("store");
    seed(&f, "sp-store");
    stub(&f, "spira-admit");
    stub(&f, "sccache");
    let o = go(&f, "spira,plan", &[("SPIRA_SUMMON_JITTER", "0"), ("SPIRA_SCCACHE_DAV_ADDR", "192.168.1.56:9431")], false, Mode::Claim, BTreeMap::new(), commits_and_closes());
    let env = &o.seen[0].env;
    let get = |k: &str| env.get(k).map(String::as_str);
    assert_eq!(get("SCCACHE_WEBDAV_ENDPOINT"), Some("http://192.168.1.56:9431"));
    assert_eq!(get("SCCACHE_WEBDAV_KEY_PREFIX"), Some("/"));
}

/// Without `SPIRA_SCCACHE_DAV_ADDR`, no webdav var reaches the build — unchanged from
/// before sp-xtdqi.
#[test]
fn no_configured_store_means_no_webdav_vars_on_an_agents_build() {
    let f = fx("nostore");
    seed(&f, "sp-nostore");
    stub(&f, "spira-admit");
    stub(&f, "sccache");
    let o = go(&f, "spira,plan", &[("SPIRA_SUMMON_JITTER", "0")], false, Mode::Claim, BTreeMap::new(), commits_and_closes());
    let env = &o.seen[0].env;
    assert!(!env.contains_key("SCCACHE_WEBDAV_ENDPOINT"), "{env:?}");
    assert!(!env.contains_key("SCCACHE_WEBDAV_KEY_PREFIX"), "{env:?}");
}

#[test]
fn without_spira_admit_the_session_still_runs_on_the_plain_cache_and_says_so() {
    let f = fx("admit-absent");
    seed(&f, "sp-nadm");
    let sccache = stub(&f, "sccache");
    let o = go(&f, "spira,plan", &[("SPIRA_SUMMON_JITTER", "0")], false, Mode::Claim, BTreeMap::new(), commits_and_closes());
    let env = &o.seen[0].env;
    if env.get("PATH").is_some_and(|p| p.split(':').any(|d| std::path::Path::new(d).join("spira-admit").exists())) {
        return; // the host's PATH carries a spira-admit the fixture cannot hide
    }
    assert_eq!(env.get("RUSTC_WRAPPER").map(String::as_str), Some(sccache.to_str().unwrap()));
    assert!(!env.contains_key("SPIRA_ADMIT_INNER"));
    assert!(o.log.contains("spira-admit not on PATH — this session's builds are not admitted"), "{}", o.log);
}

#[test]
fn the_summon_jitter_sleeps_what_it_logs_and_never_more_than_its_bound() {
    let f = fx("jitter");
    seed(&f, "sp-jit");
    let o = go(&f, "spira,plan", &[("SPIRA_SUMMON_JITTER", "5")], false, Mode::Claim, BTreeMap::new(), commits_and_closes());
    assert!(o.slept <= 5, "{}", o.slept);
    if o.slept > 0 {
        assert!(o.log.contains(&format!("summon jitter {}s", o.slept)), "{}", o.log);
    } else {
        assert!(!o.log.contains("summon jitter"));
    }
    assert_eq!(o.seen.len(), 1, "the session still ran");
}

// test-rapid-recur.sh (sp-fmvtv): three consecutive sub-10s summons on the same bead are a
// setup loop that recurs identically on every retry — park it instead of re-summoning
// forever. rapid_recur_check (lib.sh) is retired; this is aeon::run::Run::rapid_recur_check.
#[test]
fn rapid_recur_parks_a_bead_after_three_consecutive_sub_10s_summons() {
    let f = fx("rapidrecur");
    seed(&f, "sp-rr");
    std::fs::create_dir_all(&f.run).unwrap();
    std::fs::write(
        f.run.join("aeon-ledger.log"),
        "2026-09-27T00:00:00Z done builder sp-rr rc=0 status=unlanded wall_s=1 api_s=1 turns=1 in_tok=1 cache_read_tok=0 out_tok=1 think_tok=0 cost_usd=0.01\n\
         2026-09-27T00:00:01Z done builder sp-rr rc=0 status=unlanded wall_s=2 api_s=1 turns=1 in_tok=1 cache_read_tok=0 out_tok=1 think_tok=0 cost_usd=0.01\n",
    )
    .unwrap();
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), Box::new(|_, _, _| 1));
    assert_eq!(o.code, 1, "{}", o.log);
    let w = o.w.lock().unwrap();
    assert!(w.labels.get("sp-rr").is_some_and(|l| l.contains("needs-ryan")), "{:?}", w.labels.get("sp-rr")); // literal-ok: fixture/fallback
    assert!(w.labels.get("sp-rr").is_some_and(|l| l.contains("overseer")), "{:?}", w.labels.get("sp-rr"));
    assert!(w.notes.iter().any(|(id, n)| id == "sp-rr" && n.contains("RAPID-RECUR")), "{:?}", w.notes);
    assert!(o.log.contains("RAPID-RECUR: 3 consecutive sub-10s runs"), "{}", o.log);
    assert!(w.exec_calls.iter().any(|(prog, args, _)| prog == "spira-lc" && args.first().map(String::as_str) == Some("hold")), "{:?}", w.exec_calls);
}

// The positive control: two prior real (wall_s>=10) runs never trip the guard, however many
// sub-10s runs follow.
#[test]
fn rapid_recur_does_not_park_a_bead_with_real_prior_runs() {
    let f = fx("rapidrecur-ok");
    seed(&f, "sp-rr2");
    std::fs::create_dir_all(&f.run).unwrap();
    std::fs::write(
        f.run.join("aeon-ledger.log"),
        "2026-09-27T00:00:00Z done builder sp-rr2 rc=0 status=unlanded wall_s=90 api_s=1 turns=1 in_tok=1 cache_read_tok=0 out_tok=1 think_tok=0 cost_usd=0.01\n\
         2026-09-27T00:00:01Z done builder sp-rr2 rc=0 status=unlanded wall_s=90 api_s=1 turns=1 in_tok=1 cache_read_tok=0 out_tok=1 think_tok=0 cost_usd=0.01\n",
    )
    .unwrap();
    let o = go(&f, "spira,plan", &[], false, Mode::Claim, BTreeMap::new(), Box::new(|_, _, _| 1));
    assert_eq!(o.code, 1, "{}", o.log);
    let w = o.w.lock().unwrap();
    assert!(!w.labels.get("sp-rr2").is_some_and(|l| l.contains("needs-ryan")), "{:?}", w.labels.get("sp-rr2")); // literal-ok: fixture/fallback
}

// ---- spira-lc is the only state source ------------------------------------------------

fn reads_pre_lifecycle_state(src: &str) -> bool {
    src.lines().any(|l| {
        let l = l.trim_start();
        !l.starts_with("//") && (l.contains("landstate") || l.contains("land_state") || l.contains("LANDSTATE"))
    })
}

#[test]
fn no_aeon_source_reads_the_landstate_ledger() {
    assert!(reads_pre_lifecycle_state("let p = run.join(\"landstate\");"), "the matcher must fire on a planted read");
    assert!(!reads_pre_lifecycle_state("// landstate is gone\nlet x = 1;"));
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut checked = 0;
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "rs") && !p.file_name().is_some_and(|n| n == "tests.rs") {
            checked += 1;
            let src = std::fs::read_to_string(&p).unwrap();
            let code = src.split("#[cfg(test)]").next().unwrap();
            assert!(!reads_pre_lifecycle_state(code), "{} reads the landstate ledger; read state through spira-lc", p.display());
        }
    }
    assert!(checked > 10);
}
