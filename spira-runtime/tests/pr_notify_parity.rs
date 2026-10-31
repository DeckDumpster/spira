//! For one fixture forge, pr-notify.sh and the pr-notify handler say the same lines and mail the
//! operator the same notes. The script is the oracle: it runs unmodified against a stubbed `gh`
//! and `mail`; the handler runs on a virtual clock against an HTTP stub of the forge. Both are
//! fed from one description of the world, so a disagreement is a disagreement about the same facts.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use runtime::host::EffectRunner;
use runtime::ports::VirtualClock;
use runtime::{Engine, Executor, MemJournal, Outcome};
use serde_json::{json, Value};
use spira_runtime::forge::Forge;
use spira_runtime::watchers::pr_notify::PrNotify;
use spira_runtime::world::World;
use spira_runtime::Effects;

const INTERVAL_MS: u64 = 1000;
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

#[derive(Clone)]
struct Pr {
    n: u64,
    head: &'static str,
    checks: Vec<(&'static str, &'static str, &'static str)>,
}

#[derive(Clone)]
struct Gone {
    state: &'static str,
    sha: &'static str,
    body: &'static str,
}

#[derive(Clone)]
struct RunOf {
    conclusion: &'static str,
}

#[derive(Clone, Default)]
struct Repo {
    open: Vec<Pr>,
    gone: BTreeMap<u64, Gone>,
    runs: BTreeMap<&'static str, RunOf>,
    branches: Vec<&'static str>,
    down: bool,
}

type Forges = BTreeMap<&'static str, Repo>;

fn pr(n: u64, checks: &[(&'static str, &'static str, &'static str)]) -> Pr {
    Pr { n, head: "feature/x", checks: checks.to_vec() }
}

const PASS: (&str, &str, &str) = ("suites", "COMPLETED", "SUCCESS");
const FAIL: (&str, &str, &str) = ("suites", "COMPLETED", "FAILURE");
const LINT_FAIL: (&str, &str, &str) = ("lint", "COMPLETED", "TIMED_OUT");
const RUNNING: (&str, &str, &str) = ("suites", "IN_PROGRESS", "");

fn title(n: u64) -> String {
    format!("Title of {n}")
}

fn pr_json_gh(p: &Pr) -> Value {
    let rollup: Vec<Value> = p.checks.iter().map(|(name, status, concl)| json!({"name": name, "status": status, "conclusion": concl})).collect();
    json!({"number": p.n, "title": title(p.n), "headRefName": p.head, "statusCheckRollup": rollup})
}

fn stamp(secs: u64) -> String {
    format!("2026-10-28T10:{:02}:{:02}Z", secs / 60, secs % 60)
}

fn lower_or_null(s: &str) -> Value {
    if s.is_empty() {
        Value::Null
    } else {
        json!(s.to_lowercase())
    }
}

fn gone_json_rest(g: &Gone) -> Value {
    json!({"state": if g.state == "OPEN" { "open" } else { "closed" }, "merged": g.state == "MERGED", "merge_commit_sha": if g.sha.is_empty() { Value::Null } else { json!(g.sha) }, "body": g.body})
}

fn route(forges: &Forges, path: &str) -> Option<Value> {
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let rest = path.strip_prefix("/repos/acme/")?;
    let (name, rest) = rest.split_once('/')?;
    let repo = forges.get(name)?;
    if repo.down {
        return None;
    }
    if rest == "pulls" {
        return Some(Value::Array(repo.open.iter().map(|p| json!({"number": p.n, "title": title(p.n), "head": {"ref": p.head, "sha": format!("sha{}", p.n)}})).collect()));
    }
    if let Some(n) = rest.strip_prefix("pulls/") {
        return repo.gone.get(&n.parse().ok()?).map(gone_json_rest);
    }
    if let Some(sha) = rest.strip_prefix("commits/").and_then(|r| r.strip_suffix("/check-runs")) {
        let p = repo.open.iter().find(|p| format!("sha{}", p.n) == sha)?;
        return Some(json!({"check_runs": p.checks.iter().map(|(n, s, c)| json!({"name": n, "status": s.to_lowercase(), "conclusion": lower_or_null(c)})).collect::<Vec<_>>()}));
    }
    if rest.starts_with("actions/workflows/deploy.yml/runs") {
        let sha = query.split('&').find_map(|q| q.strip_prefix("head_sha=")).unwrap_or("");
        let runs: Vec<Value> = repo.runs.get(sha).map(|r| json!({"status": "completed", "conclusion": r.conclusion, "html_url": format!("http://runs/{sha}"), "created_at": stamp(0), "updated_at": stamp(125)})).into_iter().collect();
        return Some(json!({"workflow_runs": runs}));
    }
    if rest.starts_with("branches") {
        return Some(json!(repo.branches.iter().map(|b| json!({"name": b})).collect::<Vec<_>>()));
    }
    if rest.starts_with("compare/") {
        return Some(json!({"ahead_by": 1}));
    }
    None
}

fn serve_forge(forges: Arc<Mutex<Forges>>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            let mut reader = BufReader::new(&stream);
            reader.read_line(&mut line).unwrap();
            let mut header = String::new();
            while reader.read_line(&mut header).is_ok_and(|n| n > 2) {
                header.clear();
            }
            let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
            let found = route(&forges.lock().unwrap(), &path);
            let (status, body) = match found {
                Some(v) => ("200 OK", v.to_string()),
                None => ("404 Not Found", "{}".to_string()),
            };
            let mut w = &stream;
            let _ = write!(w, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    api
}

const GH_STUB: &str = r#"#!/usr/bin/env bash
D="$PWD/.gh-fx"
[ -e "$D/down" ] && exit 1
case "$1 $2" in
  "pr list")
    case "$*" in
      *--head*) cat "$D/head.${4//\//_}" 2>/dev/null || echo 0 ;;
      *) cat "$D/list.json" ;;
    esac ;;
  "pr view")
    case "$*" in
      *"--json state"*) cat "$D/state.$3" 2>/dev/null || exit 1 ;;
      *) cat "$D/full.$3" 2>/dev/null || exit 1 ;;
    esac ;;
  "run list")
    while [ $# -gt 0 ]; do [ "$1" = --commit ] && sha="$2"; shift; done
    cat "$D/runs.$sha" 2>/dev/null || echo '[]' ;;
  *) exit 1 ;;
esac
"#;

const MAIL_STUB: &str = r#"#!/usr/bin/env bash
printf '%s\n' "$*" >> "$OLD_MAIL.args"
subject=""
while [ $# -gt 0 ]; do [ "$1" = --subject ] && subject="$2"; shift; done
n=$(ls "$OLD_MAIL" | wc -l)
{ printf '%s\n' "$subject"; cat; } > "$OLD_MAIL/$n"
"#;

struct Parity {
    dir: testkit::TempDir,
    repos: Vec<(&'static str, &'static str)>,
    forges: Arc<Mutex<Forges>>,
    engine: Engine,
    clock: VirtualClock,
    fx: Effects,
    inbox_seen: usize,
}

fn debug_dir() -> PathBuf {
    std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().to_path_buf()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(["-c", "user.email=t@t", "-c", "user.name=t"]).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn mail_env(root: &Path) -> mail::env::Env {
    mail::env::Env {
        mail_root: root.join("mail"),
        kinds_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/mail/kinds"),
        index_file: root.join("mail-index"),
        mute: false,
        loom_budget_ms: 1000,
        repeat_window_s: 600,
        tidy_fresh_s: 0,
        id_prefix: "sp".into(),
        ask_label: "ask".into(),
        db: String::new(),
        bd_bin: String::new(),
        bd_conn_retries: 1,
        operator_actor: "operator".into(),
        run_dir: root.join("run"),
        home: root.to_path_buf(),
        mail_from: None,
        lint_considered: None,
        repeat_considered: None,
        operator_considered: None,
        statute_guards: String::new(),
        statute_considered: None,
        allow_blocking: false,
        bead_id: None,
        lock_timeout_ms: 1000,
    }
}

impl Parity {
    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn build(name: &str, repos: Vec<(&'static str, &'static str)>, branches: &[&str]) -> Parity {
        let dir = testkit::TempDir::new(&format!("prn-{name}"));
        let root = dir.path().to_path_buf();
        for d in ["bin", "run", "old-mail"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        let mut rows = String::new();
        for (repo, land) in &repos {
            let path = root.join(repo);
            fs::create_dir_all(&path).unwrap();
            git(&path, &["init", "-q"]);
            git(&path, &["remote", "add", "origin", &format!("file:///nonexistent/acme/{repo}.git")]);
            git(&path, &["commit", "-q", "--allow-empty", "-m", "base"]);
            let head = git(&path, &["rev-parse", "HEAD"]);
            git(&path, &["update-ref", "refs/remotes/origin/main", &head]);
            let tree = git(&path, &["rev-parse", "HEAD^{tree}"]);
            for b in branches {
                let c = git(&path, &["commit-tree", &tree, "-p", &head, "-m", b]);
                git(&path, &["update-ref", &format!("refs/remotes/origin/{b}"), &c]);
            }
            let base = if *land == "pr" { "main" } else { "origin/main" };
            rows.push_str(&format!("{repo} | {} | {land} | {base} | |\n", path.display()));
        }
        fs::write(root.join("rows"), rows).unwrap();
        for (name, body) in [("gh", GH_STUB), ("mail", MAIL_STUB)] {
            testkit::write_exe(root.join("bin").join(name), body);
        }
        fs::write(root.join("layer.toml"), format!("[spira]\nrun = \"{}\"\nrepo_map = \"{}\"\n", root.join("run").display(), root.join("rows").display())).unwrap();

        let forges: Arc<Mutex<Forges>> = Arc::new(Mutex::new(repos.iter().map(|(r, _)| (*r, Repo::default())).collect()));
        let api = serve_forge(forges.clone());
        let fx = Effects {
            world: World { run_dir: root.join("run"), queue_dir: root.join("run/queue"), repo_map: root.join("rows"), home_repo: "home".into(), inbox: root.join("new-inbox"), lc_socket: root.join("no.sock"), ..World::default() },
            forge: Forge { api, token: "t0ken".into() },
            mail: Some(Arc::new(spira_runtime::operator_mail(mail_env(&root)))),
            ..Effects::default()
        };
        let clock = VirtualClock::new(1_800_000_000_000);
        let cfg = PrNotify { interval_ms: INTERVAL_MS, deploy_giveup_secs: 777 };
        let mut engine = Engine::open(MemJournal::new(), Arc::new(clock.clone()), vec![cfg.registration()], 0).unwrap();
        let (id, stream, kind) = PrNotify::boot();
        engine.submit(id, stream, kind).unwrap();
        Parity { dir, repos, forges, engine, clock, fx, inbox_seen: 0 }
    }

    fn publish_to_gh(&self) {
        let forges = self.forges.lock().unwrap();
        for (name, _) in &self.repos {
            let r = &forges[name];
            let d = self.p(name).join(".gh-fx");
            let _ = fs::remove_dir_all(&d);
            fs::create_dir_all(&d).unwrap();
            if r.down {
                fs::write(d.join("down"), "").unwrap();
            }
            fs::write(d.join("list.json"), Value::Array(r.open.iter().map(pr_json_gh).collect()).to_string()).unwrap();
            for b in ["spira/covered", "spira/stray", "feature/nope"] {
                let n = r.open.iter().filter(|p| p.head == b).count();
                fs::write(d.join(format!("head.{}", b.replace('/', "_"))), n.to_string()).unwrap();
            }
            for (n, g) in &r.gone {
                fs::write(d.join(format!("state.{n}")), g.state).unwrap();
                fs::write(d.join(format!("full.{n}")), json!({"mergeCommit": if g.sha.is_empty() { Value::Null } else { json!({"oid": g.sha}) }, "body": g.body}).to_string()).unwrap();
            }
            for (sha, run) in &r.runs {
                fs::write(d.join(format!("runs.{sha}")), json!([{"status": "completed", "conclusion": run.conclusion, "url": format!("http://runs/{sha}"), "createdAt": stamp(0), "updatedAt": stamp(125)}]).to_string()).unwrap();
            }
        }
    }

    fn script_tick(&self) -> (Vec<String>, Vec<String>) {
        assert!(debug_dir().join("spira-config").exists(), "build spira-config first: pr-notify.sh resolves the repo map through it");
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let path = format!("{}:{}:/usr/bin:/bin", self.p("bin").display(), debug_dir().display());
        let before: Vec<_> = fs::read_dir(self.p("old-mail")).unwrap().flatten().map(|e| e.file_name()).collect();
        let out = Command::new("bash")
            .arg(root.join("spira/pr-notify.sh"))
            .arg("--show")
            .env_clear()
            .env("PATH", path)
            .env("HOME", self.dir.path())
            .env("OLD_MAIL", self.p("old-mail"))
            .env("SPIRA_TOML", format!("{}:{}", root.join("spira-config/tests/fixtures/complete.toml").display(), self.p("layer.toml").display()))
            .env("SPIRA_REPO", self.p("home"))
            .output()
            .unwrap();
        assert!(out.status.success(), "pr-notify.sh failed: {}", String::from_utf8_lossy(&out.stderr));
        let lines = String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect();
        let mut notes: Vec<String> = fs::read_dir(self.p("old-mail")).unwrap().flatten().filter(|e| !before.contains(&e.file_name())).map(|e| fs::read_to_string(e.path()).unwrap().trim_end().to_string()).collect();
        notes.sort();
        (lines, notes)
    }

    fn handler_tick(&mut self) -> (Vec<String>, Vec<String>) {
        let before = self.delivered();
        self.engine.run_until_idle().unwrap();
        loop {
            let pending = self.engine.pending_effects();
            if pending.is_empty() {
                break;
            }
            for p in pending {
                let out = match self.fx.run(&p.key, &p.spec) {
                    Ok(d) => Outcome::Succeeded(d),
                    Err(d) => panic!("effect {} failed: {d}", p.spec.name),
                };
                self.engine.complete(&p.key, out).unwrap();
            }
            self.engine.run_until_idle().unwrap();
        }
        self.clock.advance(INTERVAL_MS);
        let inbox = fs::read_to_string(self.p("new-inbox")).unwrap_or_default();
        let lines = inbox.lines().skip(self.inbox_seen).map(|l| l.split_once(" [watch:pr-notify] ").expect("an inbox row names its watcher").1.to_string()).collect();
        self.inbox_seen = inbox.lines().count();
        let mut notes: Vec<String> = self.delivered().into_iter().filter(|n| !before.contains(n)).collect();
        notes.sort();
        (lines, notes)
    }

    /// Delivered operator notes as `Subject\nbody`, the shape the stub `mail` records.
    fn delivered(&self) -> Vec<String> {
        let new = self.p("mail/operator/new");
        fs::read_dir(new)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| {
                let text = fs::read_to_string(e.path()).unwrap();
                let (head, body) = text.split_once("\n\n").unwrap();
                let subject = head.lines().find_map(|l| l.strip_prefix("Subject: ")).unwrap();
                format!("{subject}\n{}", body.trim_end())
            })
            .collect()
    }

    /// Change the world, let both watchers look at it once, and require that they said the same.
    fn phase(&mut self, change: impl FnOnce(&mut Forges)) -> (Vec<String>, Vec<String>) {
        change(&mut self.forges.lock().unwrap());
        self.publish_to_gh();
        let old = self.script_tick();
        let new = self.handler_tick();
        assert_eq!(new, old, "the handler and pr-notify.sh disagree");
        old
    }
}

fn widgets() -> Parity {
    Parity::build("w", vec![("widgets", "pr")], &[])
}

#[test]
fn e2e_a_pr_is_opened_failed_and_greened_once_each_in_every_land_mode() {
    let mut p = Parity::build("modes", vec![("widgets", "pr"), ("gadgets", "queue.forge")], &[]);
    let (lines, _) = p.phase(|f| f.get_mut("gadgets").unwrap().open = vec![pr(3, &[RUNNING])]);
    assert_eq!(lines, [format!("OPENED #3 {} [gadgets]", title(3))]);
    assert!(p.phase(|_| {}).0.is_empty(), "an unchanged PR is silent");
    let (lines, _) = p.phase(|f| {
        f.get_mut("gadgets").unwrap().open = vec![pr(3, &[FAIL, LINT_FAIL, PASS])];
        f.get_mut("widgets").unwrap().open = vec![pr(9, &[FAIL])];
    });
    assert_eq!(lines.iter().filter(|l| l.starts_with("FAIL RED")).count(), 2, "{lines:?}");
    assert!(lines.iter().any(|l| l.ends_with("[gadgets]: suites,lint")), "{lines:?}");
    let (lines, _) = p.phase(|f| f.get_mut("gadgets").unwrap().open = vec![pr(3, &[PASS])]);
    assert_eq!(lines, [format!("GREEN #3 {} [gadgets]: lands automatically", title(3))]);
}

#[test]
fn e2e_a_merge_mails_the_operator_and_its_deploy_is_reported_when_the_run_completes() {
    let mut p = widgets();
    p.phase(|f| f.get_mut("widgets").unwrap().open = vec![pr(5, &[PASS])]);
    let body = "Batch\n\n- sp-aaa11 — first change\n- sp-bbb22 — second change\nnot a bead line";
    let (lines, notes) = p.phase(|f| {
        let w = f.get_mut("widgets").unwrap();
        w.open.clear();
        w.gone.insert(5, Gone { state: "MERGED", sha: SHA, body });
    });
    assert_eq!(lines, [format!("MERGED #5 {} [widgets]", title(5))]);
    assert_eq!(notes.len(), 1);
    assert!(notes[0].starts_with("MERGED #5 [widgets] 012345678\n"), "{notes:?}");
    let args = fs::read_to_string(p.p("old-mail.args")).unwrap();
    assert!(args.contains("send operator --from PR Notify <pr-notify@spira> --subject MERGED #5 [widgets] 012345678 --kind note --digest"), "{args}");
    let sent = fs::read_dir(p.p("mail/operator/new")).unwrap().flatten().map(|e| fs::read_to_string(e.path()).unwrap()).next().unwrap();
    assert!(sent.contains("From: PR Notify <pr-notify@spira>\n") && sent.contains("X-Spira-Kind: note\n") && sent.contains("X-Spira-Digest: yes\n"), "{sent}");
    assert!(notes[0].contains("Beads:\n- bead sp-aaa11 — first change\n- bead sp-bbb22 — second change"), "{notes:?}");
    assert!(p.phase(|_| {}).0.is_empty(), "no run yet: the merge stays queued");
    let (lines, notes) = p.phase(|f| {
        f.get_mut("widgets").unwrap().runs.insert(SHA, RunOf { conclusion: "success" });
    });
    assert_eq!(lines, [format!("DEPLOYED #5 {} [widgets]: success in 125s http://runs/{SHA}", title(5))]);
    assert!(notes[0].contains("Run: success in 2m5s"), "{notes:?}");
    assert!(p.phase(|_| {}).0.is_empty(), "a deploy is reported once");
}

#[test]
fn e2e_a_failed_deploy_run_is_named_as_failed() {
    let mut p = widgets();
    p.phase(|f| f.get_mut("widgets").unwrap().open = vec![pr(6, &[PASS])]);
    p.phase(|f| {
        let w = f.get_mut("widgets").unwrap();
        w.open.clear();
        w.gone.insert(6, Gone { state: "MERGED", sha: SHA, body: "" });
    });
    let (lines, notes) = p.phase(|f| {
        f.get_mut("widgets").unwrap().runs.insert(SHA, RunOf { conclusion: "failure" });
    });
    assert!(lines[0].starts_with("DEPLOY FAILED #6"), "{lines:?}");
    assert!(notes[0].starts_with("DEPLOY FAILED #6 [widgets] 012345678\n"), "{notes:?}");
}

#[test]
fn e2e_a_pr_the_forge_cannot_account_for_stays_tracked_until_it_can() {
    let mut p = widgets();
    p.phase(|f| f.get_mut("widgets").unwrap().open = vec![pr(4, &[RUNNING])]);
    assert!(p.phase(|f| f.get_mut("widgets").unwrap().open.clear()).0.is_empty(), "gone from the list, unexplained");
    assert!(p.phase(|f| { f.get_mut("widgets").unwrap().gone.insert(4, Gone { state: "OPEN", sha: "", body: "" }); }).0.is_empty());
    let (lines, notes) = p.phase(|f| { f.get_mut("widgets").unwrap().gone.insert(4, Gone { state: "CLOSED", sha: "", body: "" }); });
    assert_eq!(lines, [format!("CLOSED #4 {} [widgets]", title(4))]);
    assert!(notes.is_empty(), "closing is not an operator note");
}

#[test]
fn e2e_a_forge_that_is_down_leaves_state_untouched_and_the_next_answer_is_judged_against_it() {
    let mut p = widgets();
    p.phase(|f| f.get_mut("widgets").unwrap().open = vec![pr(8, &[RUNNING])]);
    assert!(p.phase(|f| f.get_mut("widgets").unwrap().down = true).0.is_empty(), "down is not an empty list");
    let (lines, _) = p.phase(|f| {
        let w = f.get_mut("widgets").unwrap();
        w.down = false;
        w.open = vec![pr(8, &[FAIL])];
    });
    assert_eq!(lines, [format!("FAIL RED #8 {} [widgets]: suites", title(8))], "no second OPENED for a PR that was already tracked");
}

#[test]
fn e2e_a_pushed_branch_without_a_pr_is_named_only_where_prs_are_the_land_mode() {
    let mut p = Parity::build("stray", vec![("widgets", "pr"), ("gadgets", "queue.forge")], &["spira/covered", "spira/stray", "feature/nope"]);
    let (lines, _) = p.phase(|f| {
        let mut covered = pr(2, &[RUNNING]);
        covered.head = "spira/covered";
        for r in f.values_mut() {
            r.branches = vec!["main", "spira/covered", "spira/stray", "feature/nope"];
        }
        f.get_mut("widgets").unwrap().open = vec![covered];
    });
    assert!(lines.iter().any(|l| l == "⚠ BRANCH spira/stray: no PR [widgets]"), "{lines:?}");
    assert!(lines.iter().all(|l| !l.contains("covered: no PR") && !l.contains("[gadgets]") && !l.contains("feature/nope")), "{lines:?}");
}

#[test]
fn e2e_the_forge_token_rides_the_request_and_is_not_in_the_debug_output() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let api = format!("http://{}", l.local_addr().unwrap());
    let seen = std::thread::spawn(move || {
        let (mut c, _) = l.accept().unwrap();
        let mut buf = [0u8; 2048];
        let n = c.read(&mut buf).unwrap();
        let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]");
        String::from_utf8_lossy(&buf[..n]).to_lowercase()
    });
    let forge = Forge { api, token: "sekrit-token".into() };
    assert_eq!(forge.open_prs("acme/widgets"), Some(vec![]));
    assert!(seen.join().unwrap().contains("authorization: bearer sekrit-token"));
    assert!(!format!("{forge:?}").contains("sekrit"));
}

#[test]
fn e2e_a_list_that_fills_a_page_is_not_taken_for_the_whole_list() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let api = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut c in l.incoming().flatten() {
            let mut buf = [0u8; 1024];
            let _ = c.read(&mut buf);
            let body = Value::Array(vec![json!({}); 100]).to_string();
            let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    let forge = Forge { api, token: String::new() };
    assert_eq!(forge.open_prs("acme/widgets"), None);
    assert_eq!(forge.stray_branches("acme/widgets", "main", &[]), None);
}
