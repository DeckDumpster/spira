//! The impure half: read one repo's queue state, the bead store and the forge into a
//! `Snapshot`. Every read that fails becomes an entry in `Snapshot::errors` — never an empty
//! value — so the core can say "blind" rather than "quiet".

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::core::{Batch, Bead, Ci, Member, Outcome, PrState, Publish, PublishOutcome, RepoState, Snapshot};

#[derive(Clone, Debug)]
pub struct Repo {
    pub name: String,
    pub path: PathBuf,
    pub base: String,
    pub forge: PathBuf,
    /// `true` for a `queue.local` repo: `base` is a LOCAL branch (`local/main`) already in
    /// this checkout, never a remote to fetch (see `read_on_base`).
    pub local: bool,
}

#[derive(Clone, Debug)]
pub struct Env {
    pub queue_dir: PathBuf,
    pub db: Option<PathBuf>,
    pub bd: String,
    pub express_label: String,
    /// `spira-lc` — the one binary allowed to touch `spira_lifecycle` (see `read_certified`).
    pub lc_bin: Option<PathBuf>,
    pub lc_timeout: u64,
}

/// `key=value` lines, as the queue writes its open-batch file. Ok(None) when absent.
fn read_kv(path: &Path) -> Result<Option<BTreeMap<String, String>>, String> {
    match fs::read_to_string(path) {
        Ok(t) => Ok(Some(
            t.lines()
                .filter_map(|l| l.split_once('='))
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .collect(),
        )),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn members(s: &str) -> Vec<Member> {
    s.split_whitespace()
        .map(|t| match t.split_once(':') {
            Some((id, tip)) => Member { id: id.into(), tip: tip.into() },
            None => Member { id: t.into(), tip: String::new() },
        })
        .collect()
}

pub fn read_batch(dir: &Path) -> Result<Option<Batch>, String> {
    let Some(kv) = read_kv(&dir.join("open"))? else { return Ok(None) };
    let pr = kv.get("pr").cloned().unwrap_or_default();
    if pr.is_empty() {
        // The queue writes the file before it knows the PR number; an open file with no PR
        // is a cut in progress, not a batch anyone can watch yet.
        return Ok(None);
    }
    Ok(Some(Batch {
        pr,
        head: kv.get("head").cloned().unwrap_or_default(),
        branch: kv.get("branch").cloned().unwrap_or_default(),
        members: members(kv.get("members").map(String::as_str).unwrap_or("")),
    }))
}

/// The `publish` record queue.sh's `_lc_publish_step` writes for a queue.local repo, once its
/// PR number is known — written atomically (a temp file renamed into place), unlike `open`,
/// so there is no "cut in progress" partial state to filter out here.
pub fn read_publish(dir: &Path) -> Result<Option<Publish>, String> {
    let Some(kv) = read_kv(&dir.join("publish"))? else { return Ok(None) };
    let pr = kv.get("pr").cloned().unwrap_or_default();
    if pr.is_empty() {
        return Ok(None);
    }
    Ok(Some(Publish {
        pr,
        head: kv.get("head").cloned().unwrap_or_default(),
        branch: kv.get("branch").cloned().unwrap_or_default(),
    }))
}

pub fn read_bisect(dir: &Path) -> Result<Option<(Vec<String>, u64)>, String> {
    let p = dir.join("bisect");
    let text = match fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", p.display())),
    };
    let Some(first) = text.lines().find(|l| !l.trim().is_empty()) else { return Ok(None) };
    // Line 1 is "<base-sha> id:tip id:tip ..." (sp-55j4m): the base commit the split was
    // made against, recorded so batch.sh can tell whether the group is still valid. Watching
    // only reports membership, so skip that leading field.
    let members_field = first.split_once(' ').map_or("", |(_, rest)| rest);
    let mtime = fs::metadata(&p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Ok(Some((members(members_field).into_iter().map(|m| m.id).collect(), mtime)))
}

/// Every bead `spira-lc` reports CERTIFIED — `spira_lifecycle` is the one place this state
/// lives now, and `spira-lc` is the one binary allowed to query it (design intent #6), so
/// this shells out rather than reading a directory or re-deriving the query in SQL here.
/// Records are shared across repos; the caller narrows by the bead's `repo:` label.
pub fn read_certified(env: &Env) -> Result<Vec<String>, String> {
    let bin = env.lc_bin.as_ref().ok_or_else(|| "SPIRA_LC_BIN unset".to_string())?;
    let mut cmd = Command::new("timeout");
    cmd.arg(env.lc_timeout.to_string()).arg(bin).args(["list", "--state", "CERTIFIED"]);
    let text = run(&mut cmd, "spira-lc list --state CERTIFIED")?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("spira-lc list: unparsed output: {e}"))?;
    let items = match v {
        serde_json::Value::Array(a) => a,
        o => vec![o],
    };
    let mut out: Vec<String> = items.into_iter().filter_map(|it| it.get("bead_id").and_then(|x| x.as_str()).map(str::to_string)).collect();
    out.sort();
    Ok(out)
}

fn run(cmd: &mut Command, what: &str) -> Result<String, String> {
    let o = cmd.output().map_err(|e| format!("{what}: {e}"))?;
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr);
        let err = err.lines().last().unwrap_or("").trim();
        return Err(format!("{what} exited {}{}", o.status.code().unwrap_or(-1), if err.is_empty() { String::new() } else { format!(": {err}") }));
    }
    Ok(String::from_utf8_lossy(&o.stdout).to_string())
}

/// Like `run`, but a dropped pooled Dolt connection ("invalid connection" — the Go driver
/// detected it dead before sending the query, so it never ran) is retried once rather than
/// surfaced (sp-ydog2).
fn run_bd(cmd: &mut Command, what: &str) -> Result<String, String> {
    match run(cmd, what) {
        Err(e) if e.contains("invalid connection") => run(cmd, what),
        r => r,
    }
}

pub fn read_beads(env: &Env, ids: &BTreeSet<String>) -> Result<(BTreeMap<String, Bead>, BTreeMap<String, String>), String> {
    let mut beads = BTreeMap::new();
    let mut repos = BTreeMap::new();
    if ids.is_empty() {
        return Ok((beads, repos));
    }
    let mut cmd = Command::new(&env.bd);
    if let Some(db) = &env.db {
        cmd.current_dir(db);
    }
    cmd.arg("show").arg("--json").args(ids);
    let text = run_bd(&mut cmd, "bd show")?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("bd show: unparsed output: {e}"))?;
    let items = match v {
        serde_json::Value::Array(a) => a,
        o => vec![o],
    };
    for it in items {
        let Some(id) = it.get("id").and_then(|x| x.as_str()) else { continue };
        let labels: Vec<&str> = it
            .get("labels")
            .and_then(|l| l.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();
        if let Some(r) = labels.iter().find_map(|l| l.strip_prefix("repo:")) {
            repos.insert(id.to_string(), r.to_string());
        }
        beads.insert(
            id.to_string(),
            Bead {
                priority: it.get("priority").and_then(|p| p.as_u64()).map(|p| p.min(9) as u8),
                title: it.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                express: labels.contains(&env.express_label.as_str()),
            },
        );
    }
    Ok((beads, repos))
}

fn forge(
    repo: &Repo,
    sub: &str,
    pr: &str,
    branch: Option<&str>,
    path: Option<&std::ffi::OsStr>,
) -> Result<String, String> {
    // A bare name (the release's `forge` binary, default since sp-yv4b3) is exec'd by name
    // on PATH; a configured path via bash.
    let mut c = if repo.forge.components().count() == 1 {
        Command::new(&repo.forge)
    } else {
        let mut c = Command::new("bash");
        c.arg(&repo.forge);
        c
    };
    if let Some(p) = path {
        c.env("PATH", p);
    }
    c.arg(sub).arg(&repo.path).arg(pr);
    if let Some(b) = branch {
        c.arg(b);
    }
    let out = run(
        &mut c,
        &format!("forge {sub} {pr}"),
    )?;
    Ok(out.lines().next().unwrap_or("").trim().to_string())
}

pub fn read_ci(repo: &Repo, pr: &str, branch: &str) -> Result<Ci, String> {
    read_ci_on(repo, pr, branch, None)
}

/// `path`, when given, is the child's PATH — the seam tests use instead of mutating the process PATH.
pub fn read_ci_on(repo: &Repo, pr: &str, branch: &str, path: Option<&std::ffi::OsStr>) -> Result<Ci, String> {
    match forge(repo, "check-status", pr, Some(branch), path)?.as_str() {
        "pending" => Ok(Ci::Pending),
        "green" => Ok(Ci::Green),
        "red" => Ok(Ci::Red),
        k @ ("harness_fault" | "provision_fault") => Ok(Ci::Fault(k.to_string())),
        other => Err(format!("forge check-status {pr}: unrecognised answer {other:?}")),
    }
}

/// Epoch the earliest still-queued job of `branch`'s latest run was created, or `None` when
/// nothing is queued (already picked up, or no run at all). Best-effort: this only sharpens
/// an existing stall judgement, so a forge hiccup here must not blind the rest of the poll —
/// the caller falls back to the coarser head-stall check instead.
pub fn read_queued_since(repo: &Repo, branch: &str) -> Option<u64> {
    if branch.is_empty() {
        return None;
    }
    let out = run(
        Command::new("bash").arg(&repo.forge).arg("queued-since").arg(&repo.path).arg(branch),
        "forge queued-since",
    )
    .ok()?;
    out.lines().next().unwrap_or("").trim().parse().ok()
}

pub fn read_pr_state(repo: &Repo, pr: &str) -> PrState {
    match forge(repo, "pr-state", pr, None, None).as_deref() {
        Ok("open") => PrState::Open,
        Ok("merged") => PrState::Merged,
        Ok("closed") => PrState::Closed,
        _ => PrState::Unknown,
    }
}

/// Is each member's tip on the base branch? Fetches first: an unfetched base is the stale
/// checkout that reads as "not landed" (law-closed-is-not-landed).
///
/// A `queue.local` repo's base (`local/main`) is a LOCAL branch already in this checkout, not
/// a remote-tracking one — `repo.local` is the structural fact from the repo-map's land
/// column, so this never falls back to guessing a remote off the ref's own spelling. Nothing
/// to fetch, and the ref is qualified to `refs/heads/...` so a repo that happens to have a
/// remote genuinely named `local` still resolves the LOCAL branch, never that remote's copy.
pub fn read_on_base(repo: &Repo, ms: &[Member]) -> Vec<(String, Option<bool>)> {
    if repo.local {
        return read_on_local_base(repo, ms);
    }
    let remote = repo.base.split_once('/').map(|(r, _)| r).unwrap_or("origin");
    let fetched = Command::new("git")
        .arg("-C")
        .arg(&repo.path)
        .args(["fetch", "-q", remote])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ms.iter()
        .map(|m| {
            if !fetched || m.tip.is_empty() {
                return (m.id.clone(), None);
            }
            let st = Command::new("git")
                .arg("-C")
                .arg(&repo.path)
                .args(["merge-base", "--is-ancestor", &m.tip, &repo.base])
                .status();
            let v = match st.ok().and_then(|s| s.code()) {
                Some(0) => Some(true),
                Some(1) => Some(false),
                _ => None,
            };
            (m.id.clone(), v)
        })
        .collect()
}

fn read_on_local_base(repo: &Repo, ms: &[Member]) -> Vec<(String, Option<bool>)> {
    let base_ref = format!("refs/heads/{}", repo.base);
    ms.iter()
        .map(|m| {
            if m.tip.is_empty() {
                return (m.id.clone(), None);
            }
            let st = Command::new("git")
                .arg("-C")
                .arg(&repo.path)
                .args(["merge-base", "--is-ancestor", &m.tip, &base_ref])
                .status();
            let v = match st.ok().and_then(|s| s.code()) {
                Some(0) => Some(true),
                Some(1) => Some(false),
                _ => None,
            };
            (m.id.clone(), v)
        })
        .collect()
}

pub fn snapshot(env: &Env, repo: &Repo, prev: &RepoState, now: u64) -> Snapshot {
    let mut s = Snapshot { now, ..Default::default() };
    let qdir = env.queue_dir.join(&repo.name);

    match read_batch(&qdir) {
        Ok(b) => s.batch = b,
        Err(e) => s.errors.push(e),
    }
    match read_bisect(&qdir) {
        Ok(b) => s.bisect = b,
        Err(e) => s.errors.push(e),
    }
    match read_publish(&qdir) {
        Ok(p) => s.publish = p,
        Err(e) => s.errors.push(e),
    }
    let certified = match read_certified(env) {
        Ok(c) => c,
        Err(e) => {
            s.errors.push(e);
            vec![]
        }
    };
    if !s.errors.is_empty() {
        return s;
    }

    let mut ids: BTreeSet<String> = certified.iter().cloned().collect();
    for b in [s.batch.as_ref(), prev.batch()].into_iter().flatten() {
        ids.extend(b.members.iter().map(|m| m.id.clone()));
    }
    match read_beads(env, &ids) {
        Ok((beads, repos)) => {
            s.certified = certified
                .into_iter()
                .filter(|c| repos.get(c).map(|r| r == &repo.name).unwrap_or(false))
                .collect();
            s.beads = beads;
        }
        Err(e) => {
            s.errors.push(e);
            return s;
        }
    }

    if let Some(b) = &s.batch {
        match read_ci(repo, &b.pr, &b.branch) {
            Ok(c) => {
                if c == Ci::Pending {
                    s.queued_since = read_queued_since(repo, &b.branch);
                }
                s.ci = Some(c);
            }
            Err(e) => s.errors.push(e),
        }
    }

    // The previous batch is gone or replaced: ask the forge what became of it.
    if let Some(pb) = prev.batch() {
        if s.batch.as_ref().map(|b| b.pr != pb.pr).unwrap_or(true) {
            let state = read_pr_state(repo, &pb.pr);
            let on_base = if state == PrState::Merged { read_on_base(repo, &pb.members) } else { vec![] };
            s.outcome = Some(Outcome { pr: pb.pr.clone(), state, on_base });
        }
    }

    if let Some(p) = &s.publish {
        match read_ci(repo, &p.pr, &p.branch) {
            Ok(c) => s.publish_ci = Some(c),
            Err(e) => s.errors.push(e),
        }
    }
    if let Some(pp) = prev.publish() {
        if s.publish.as_ref().map(|p| p.pr != pp.pr).unwrap_or(true) {
            let state = read_pr_state(repo, &pp.pr);
            s.publish_outcome = Some(PublishOutcome { pr: pp.pr.clone(), state });
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn scratch_path(tag: &str) -> testkit::TempDir {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        testkit::TempDir::new(&format!("queue-watch-run_bd-test-{tag}-{n}"))
    }

    // The forge refuses to look a run up without a branch and answers pending; the stub is
    // red only for the real branch, so a call that drops it reads Pending, not Red.
    #[test]
    fn read_ci_passes_the_batch_branch_to_check_status() {
        let dir = scratch_path("ci-branch");
        let forge = dir.join("forge.sh");
        testkit::write_exe(
            &forge,
            "#!/bin/sh\nif [ \"$4\" = spira/queue/real ]; then echo red; else echo pending; fi\n",
        );
        let repo = Repo {
            name: "r".into(),
            path: PathBuf::from("/tmp/r"),
            base: "origin/main".into(),
            forge,
            local: false,
        };
        assert_eq!(read_ci(&repo, "7", "spira/queue/real"), Ok(Ci::Red));
        assert_eq!(read_ci(&repo, "7", ""), Ok(Ci::Pending));

        let q = dir.join("q");
        fs::create_dir_all(&q).unwrap();
        fs::write(q.join("open"), "pr=7\nhead=h\nbranch=spira/queue/real\n").unwrap();
        fs::write(q.join("publish"), "pr=8\nhead=h\nbranch=spira/publish/real\n").unwrap();
        assert_eq!(read_batch(&q).unwrap().unwrap().branch, "spira/queue/real");
        assert_eq!(read_publish(&q).unwrap().unwrap().branch, "spira/publish/real");
    }

    // The stub is run as `bash <script>`, never exec'd: exec'ing a just-written file races a
    // parallel fork that still holds the write fd (ETXTBSY).
    fn run_stub(body: &str) -> (Result<String, String>, usize) {
        let dir = scratch_path("stub");
        fs::create_dir_all(&dir).unwrap();
        let calls = dir.join("calls");
        let script = dir.join("bd.sh");
        fs::write(&calls, "0").unwrap();
        fs::write(&script, format!("n=$(($(cat \"{c}\") + 1)); echo \"$n\" > \"{c}\"\n{body}\n", c = calls.display())).unwrap();
        let out = run_bd(Command::new("bash").arg(&script), "bd show");
        let n = fs::read_to_string(&calls).unwrap().trim().parse().unwrap();
        (out, n)
    }

    #[test]
    fn run_bd_retries_once_on_invalid_connection() {
        let (out, n) = run_stub(
            r#"if [ "$n" -eq 1 ]; then echo "Error: failed to open database: invalid connection" >&2; exit 1; fi
echo ok"#,
        );
        assert_eq!(out.as_deref(), Ok("ok\n"));
        assert_eq!(n, 2);
    }

    // POSITIVE CONTROL: an error that is not "invalid connection" is not retried — one call.
    #[test]
    fn run_bd_does_not_retry_other_errors() {
        let (out, n) = run_stub(r#"echo "Error: something else went wrong" >&2; exit 1"#);
        assert!(out.is_err());
        assert_eq!(n, 1);
    }

    fn env_with_lc(bin: PathBuf) -> Env {
        Env { queue_dir: PathBuf::new(), db: None, bd: "bd".into(), express_label: "express".into(), lc_bin: Some(bin), lc_timeout: 5 }
    }

    // POSITIVE CONTROL: a stub that answers with something other than the JSON array
    // `spira-lc list` produces is how this test was seen to fail first — read_certified must
    // surface an error rather than silently reporting zero certified beads.
    #[test]
    fn read_certified_errors_on_unparseable_output() {
        let stub = make_stub("echo not-json");
        let err = read_certified(&env_with_lc(stub.clone())).unwrap_err();
        assert!(err.contains("unparsed output"), "{err}");
        let _ = fs::remove_file(&stub);
    }

    #[test]
    fn read_certified_calls_spira_lc_list_state_certified() {
        let stub = make_stub(
            r#"[ "$1" = list ] && [ "$2" = --state ] && [ "$3" = CERTIFIED ] || { echo "unexpected args: $*" >&2; exit 2; }
echo '[{"bead_id":"sp-b","state":"CERTIFIED"},{"bead_id":"sp-a","state":"CERTIFIED"}]'"#,
        );
        let out = read_certified(&env_with_lc(stub.clone())).unwrap();
        assert_eq!(out, vec!["sp-a".to_string(), "sp-b".to_string()]);
        let _ = fs::remove_file(&stub);
    }

    #[test]
    fn read_certified_needs_spira_lc_bin() {
        let env = Env { queue_dir: PathBuf::new(), db: None, bd: "bd".into(), express_label: "express".into(), lc_bin: None, lc_timeout: 5 };
        let err = read_certified(&env).unwrap_err();
        assert!(err.contains("SPIRA_LC_BIN unset"), "{err}");
    }

    // POSITIVE CONTROL: a connection that never recovers is retried exactly once, not forever.
    #[test]
    fn run_bd_bounds_the_retry() {
        let (out, n) = run_stub(r#"echo "Error: failed to open database: invalid connection" >&2; exit 1"#);
        assert!(out.is_err());
        assert_eq!(n, 2);
    }

    fn git(dir: &Path, args: &[&str]) {
        let st = Command::new("git").arg("-C").arg(dir).args(args).status().expect("git");
        assert!(st.success(), "git {args:?} in {}", dir.display());
    }

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git").arg("-C").arg(dir).args(args).output().expect("git");
        assert!(o.status.success(), "git {args:?} in {}", dir.display());
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    // POSITIVE CONTROL — the hazard this guards against: a queue.local repo's base is a LOCAL
    // branch that happens to contain a slash (local/main). Splitting it on the first '/' and
    // fetching what remains as a remote is exactly what `repo.local` exists to bypass; proved
    // here by configuring a remote genuinely NAMED "local" that cannot be fetched — deriving
    // the remote from the ref's own spelling would go blind on that fetch failure, while this
    // code answers correctly because it never attempts the fetch at all.
    #[test]
    fn read_on_local_base_needs_no_fetch_and_ignores_a_remote_named_local() {
        let dir = scratch_path("local-repo");
        fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "trunk"]);
        git(&dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "base"]);
        git(&dir, &["branch", "local/main", "trunk"]);
        // A member landed on local/main: an ancestor of the local branch.
        git(&dir, &["checkout", "-q", "local/main"]);
        git(&dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "landed"]);
        let tip_in = git_out(&dir, &["rev-parse", "HEAD"]);
        // A member still on trunk, never merged into local/main: not an ancestor.
        git(&dir, &["checkout", "-q", "trunk"]);
        git(&dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "open"]);
        let tip_out = git_out(&dir, &["rev-parse", "HEAD"]);

        // NEGATIVE CONTROL PROVING THE CHECK IS REAL, not a hardcoded "local" denylist: a
        // remote is genuinely named "local", pointed at a path that cannot be fetched.
        git(&dir, &["remote", "add", "local", "/nonexistent-local-remote"]);

        let repo = Repo { name: "l".into(), path: dir.to_path_buf(), base: "local/main".into(), forge: PathBuf::new(), local: true };
        let res = read_on_base(&repo, &[Member { id: "sp-in".into(), tip: tip_in }, Member { id: "sp-out".into(), tip: tip_out }]);
        assert_eq!(res, vec![("sp-in".to_string(), Some(true)), ("sp-out".to_string(), Some(false))]);
        let _ = fs::remove_dir_all(&dir);
    }
}
