//! The real world: the destruction chokepoint in-process (`reap.rs`, sp-9envm); base
//! (family W) in-process too (sp-o88bx, "wave 4.12", `spira_config::repos`); what has not
//! moved to Rust yet (context/bead — families U/A/B) through the lib.sh seam; `gh` for the
//! one forge question; `spira-lc` by bare name on the launcher PATH.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::ports::{Base, Repo, Sent, World};
use crate::reap;
use crate::seam::{self, Op, FIELD};

pub struct Real {
    home: PathBuf,
    status: String,
    settings: BTreeMap<String, String>,
    pub submitted_label: String,
    enforce: bool,
    beads: RefCell<Option<BTreeMap<String, Value>>>,
}

/// A seam call's exit status and answer; lib.sh's own output has already been passed through.
struct Answer {
    rc: i32,
    text: String,
}

impl Real {
    /// Read the context once. Err: lib.sh could not be loaded, or it names no repository.
    /// Used by the full sweep, which needs the repository list; the chokepoint subcommands
    /// (destroy-worktree/destroy-branch/witness/reap-landed-branch) use `minimal` instead —
    /// they are handed a repo PATH directly, as the retired bash functions were, and must
    /// not be made to depend on the repo-name registry resolving at all.
    pub fn new(home: PathBuf, status: Option<String>) -> Result<(Real, Vec<Repo>), String> {
        let mut r = Real::minimal(home, status);
        let a = r.seam(Op::Context, &[]);
        if a.rc != 0 {
            return Err(format!("the lib.sh context seam exited {}", a.rc));
        }
        for rec in a.text.split('\0').filter(|s| !s.is_empty()) {
            let Some((k, v)) = rec.split_once('=') else { continue };
            r.settings.insert(k.into(), v.into());
        }
        // spira_repos/repo_root/repo_land_queued (family U) in-process (sp-k6lku, "wave
        // 4.13"), not this seam's own loop — Registry::all() always puts the home repo
        // first, so (unlike the retired bash loop) this can no longer come back empty.
        let reg = r.repo_registry();
        let repos: Vec<Repo> = reg
            .all()
            .into_iter()
            .map(|name| {
                let root = reg.root(&name).map(PathBuf::from);
                let queued = reg.land_queued(&name);
                Repo { name, root, queued }
            })
            .collect();
        r.submitted_label = r.setting("submitted", "spira-submitted");
        Ok((r, repos))
    }

    /// No context seam call at all — just `home`/`status`, for a caller that only needs
    /// the Base/Status/Bead seams (each self-contained) and never the repository registry.
    pub fn minimal(home: PathBuf, status: Option<String>) -> Real {
        Real { home, status: status.unwrap_or_default(), settings: BTreeMap::new(), submitted_label: String::new(), enforce: spira_config::lifecycle_enforce(None), beads: RefCell::new(None) }
    }

    fn setting(&self, k: &str, default: &str) -> String {
        self.settings.get(k).filter(|v| !v.is_empty()).cloned().unwrap_or_else(|| default.to_string())
    }

    /// The repo registry, in-process (sp-o88bx, "wave 4.12": family W — `spira_landref`/
    /// `spira_landrefs`/`ref_remote` — the `Op::Base` seam used to shell into, one bash
    /// subprocess per checkout). `Registry::from_env` (sp-k6lku, following the structural
    /// fix for sp-z3eyk) is the one production door onto a registry — building one from
    /// a bare `std::env::vars()` directly, with no resolution, found NO map and NO
    /// landref in production (conf.sh exports none of `SPIRA_REPO_MAP`/`SPIRA_HOME_REPO`/
    /// `SPIRA_REPO`/`SPIRA_REPO_DERIVED`); `from_env` resolves them in-process instead.
    fn repo_registry(&self) -> spira_config::repos::Registry {
        spira_config::repos::Registry::from_env(std::env::vars().collect(), &self.home)
    }

    /// `$SPIRA_RUN`, read straight from the environment — not through the context seam's
    /// own `run=` echo of the same variable, so this works whether or not that seam ran.
    pub fn run(&self) -> PathBuf {
        run_dir()
    }

    /// `${SPIRA_REAPLOG:-$SPIRA_RUN/reap.log}` — the chokepoint's own log, as a real path
    /// (distinct from the `World::reaplog` trait method, which is display text for a "see
    /// …" message and keeps its existing placeholder default unchanged).
    pub fn reaplog_path(&self) -> PathBuf {
        reaplog_path()
    }

    /// `bdq label remove <id> <label>` — label_add's own mirror, needed by
    /// `reap::reap_landed_branch` (law-branch-affinity-is-recorded) but not part of the
    /// `World` trait since nothing else in this crate calls it standalone.
    pub fn label_remove(&self, id: &str, label: &str) {
        self.seam(Op::LabelRemove, &[id, label]);
    }

    /// `sending destroy-branch`'s own entry point: the trait only has the combined `send`
    /// (witness recheck + full reap), so a standalone destroy-branch (for bash callers that
    /// are not the sweep itself — `held.sh`, and lib.sh's `spira_destroy_branch` shim) goes
    /// through this instead.
    #[allow(clippy::too_many_arguments)]
    pub fn destroy_branch(&self, id: &str, br: &str, repo: &Path, why: &str, caller: &str, base: Option<&str>) -> Result<(), reap::DestroyBranchErr> {
        reap::destroy_branch(&self.run(), &self.reaplog_path(), id, br, repo, why, caller, base, self)
    }

    /// `sending salvage`'s own entry point.
    #[allow(clippy::result_unit_err)]
    pub fn salvage(&self, id: &str, w: &Path) -> Result<Option<PathBuf>, ()> {
        reap::salvage(&self.run(), &self.reaplog_path(), id, w)
    }

    fn seam(&self, op: Op, vals: &[&str]) -> Answer {
        let home = self.home.to_string_lossy().into_owned();
        let mut all = vec![home.as_str(), self.status.as_str()];
        all.extend_from_slice(vals);
        // `-s sending.seam.sh`: bash still reads the script from stdin; the extra word is only
        // so lib.sh's spira_caller (which names the `*.sh` programs up the process chain in
        // every reap-log line) can name this one — a bare `bash` under a binary names nothing.
        let child = Command::new("bash")
            .args(["-s", "sending.seam.sh"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            // law-a-binary-resolves-the-config-it-reads (sp-kgzql): this binary's own
            // release's bin/+spira/ on the CHILD's PATH, never only inherited.
            .envs(spira_config::release_env::child_path_env_for_process())
            .spawn();
        let Ok(mut child) = child else { return Answer { rc: 127, text: String::new() } };
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(&seam::stdin_bytes(op, &all));
        }
        let Ok(out) = child.wait_with_output() else { return Answer { rc: 127, text: String::new() } };
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let (logs, answer) = seam::split(&stdout);
        if !logs.is_empty() {
            print!("{logs}");
            if !logs.ends_with('\n') {
                println!();
            }
        }
        Answer { rc: out.status.code().unwrap_or(1), text: answer.to_string() }
    }
}

fn p(x: &Path) -> String {
    x.to_string_lossy().into_owned()
}

impl World for Real {
    fn base(&self, root: &Path) -> Option<Base> {
        // spira_config::repos (sp-o88bx, "wave 4.12") in-process, instead of the
        // spira_landref/spira_landrefs/ref_remote Op::Base bash seam.
        let reg = self.repo_registry();
        let root_s = p(root);
        let (landref, local) = spira_config::repos::landrefs(&reg, &root_s)?;
        let landrefs = match &local {
            Some(l) => vec![landref.clone(), l.clone()],
            None => vec![landref.clone()],
        };
        let remote = spira_config::repos::ref_remote(&landref, Some(&root_s));
        Some(Base { landref, landrefs, remote })
    }
    fn prefetch(&self) {
        if !self.status.is_empty() {
            return;
        }
        let a = self.seam(Op::Beads, &[]);
        let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(a.text.trim()) else { return };
        let map: BTreeMap<String, Value> = rows.into_iter().filter_map(|r| Some((r.get("id")?.as_str()?.to_string(), r))).collect();
        if !map.is_empty() {
            *self.beads.borrow_mut() = Some(map);
        }
    }
    fn witness(&self, id: &str) -> Option<String> {
        if let Some(m) = self.beads.borrow().as_ref() {
            let status = m.get(id).and_then(|b| b.get("status")).and_then(Value::as_str).unwrap_or_default().to_string();
            return reap::holder_witnesses(&self.run(), id, &Cached(status));
        }
        reap::holder_witnesses(&self.run(), id, self)
    }
    fn bead(&self, id: &str) -> Option<Value> {
        if let Some(m) = self.beads.borrow().as_ref() {
            return m.get(id).cloned();
        }
        let a = self.seam(Op::Bead, &[id]);
        let v: Value = serde_json::from_str(a.text.trim()).ok()?;
        match v {
            Value::Array(mut xs) if !xs.is_empty() => Some(xs.swap_remove(0)),
            Value::Object(_) => Some(v),
            _ => None,
        }
    }
    /// send_branch: the mid-send recheck, then the verified deletion — in one call so
    /// nothing can claim the bead between the two, exactly as the retired bash seam ran
    /// both in one process.
    fn send(&self, id: &str, br: &str, repo: &Path, why: &str, caller: &str) -> Sent {
        if let Some(held) = reap::holder_witnesses(&self.run(), id, self) {
            return Sent::Held(held);
        }
        // The base and its remote, re-derived here exactly as lib.sh's own
        // spira_reap_landed_branch re-derived them rather than trusting a value the sweep
        // read earlier in the pass.
        let base_info = self.base(repo);
        let base_ref = base_info.as_ref().map(|b| b.landref.as_str());
        let remote = base_info.as_ref().and_then(|b| b.remote.as_deref());
        let (run, reaplog_path) = (self.run(), self.reaplog_path());
        match reap::reap_landed_branch(&run, &reaplog_path, id, br, repo, why, caller, base_ref, remote, self, &|m| self.log(m), &|i, l| self.label_remove(i, l)) {
            reap::ReapOutcome::Done => Sent::Done,
            reap::ReapOutcome::Queued => Sent::Queued,
            reap::ReapOutcome::Failed(m) => Sent::Failed(m),
        }
    }
    fn close_on_land(&self, id: &str, sha: &str) {
        self.seam(Op::CloseOnLand, &[id, sha]);
    }
    fn destroy_worktree(&self, id: &str, w: &Path, repo: &Path, why: &str) -> bool {
        reap::destroy_worktree(&self.run(), &self.reaplog_path(), id, w, repo, why, self)
    }
    fn prune(&self, repo: &Path) {
        reap::prune_worktrees(&self.reaplog_path(), repo);
    }
    fn label_add(&self, id: &str, label: &str) {
        self.seam(Op::LabelAdd, &[id, label]);
    }
    fn content_on_base(&self, id: &str, proof: &str) {
        let _ = Command::new("spira-lc")
            .args(["content-on-base", id, proof, "sending"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    fn pr_merged_tip(&self, repo: &Path, br: &str) -> Option<String> {
        let o = Command::new("timeout")
            .arg(self.setting("gh_timeout", "120"))
            .arg(self.setting("gh", "gh"))
            .args(["pr", "view", br, "--json", "state,headRefOid", "-q", r#"select(.state=="MERGED") | .headRefOid"#])
            .current_dir(repo)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    }
    fn enforce(&self) -> bool {
        self.enforce
    }
    fn emit(&self, line: &str) {
        println!("{line}");
    }
    fn log(&self, msg: &str) {
        println!("{} spira: {msg}", reap::utc_now());
    }
    fn reaplog(&self) -> String {
        self.setting("reaplog", "the reap log")
    }
    fn worktrees(&self) -> PathBuf {
        self.run().join("worktree")
    }
}

/// A status read from the prefetched listing; non-empty rows prove the database answered.
struct Cached(String);

impl reap::BdProbe for Cached {
    fn probe(&self, _id: &str) -> (bool, String) {
        (true, self.0.clone())
    }
}

impl reap::BdProbe for Real {
    /// `spira_db_reachable` + `spira_bead_status`, still bash (families A/B are not ported
    /// yet) — the one bd question `reap::holder_witnesses` cannot answer itself.
    fn probe(&self, id: &str) -> (bool, String) {
        let a = self.seam(Op::Status, &[id]);
        if a.rc != 0 {
            return (false, String::new());
        }
        let f: Vec<&str> = a.text.split(FIELD).collect();
        let reachable = f.first().copied() == Some("1");
        let status = f.get(1).map(|s| s.to_string()).unwrap_or_default();
        (reachable, status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(dir: &Path, args: &[&str]) {
        let o = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }

    /// `base()` (sp-o88bx, "wave 4.12") resolves in-process through `spira_config::repos`
    /// rather than the retired `Op::Base` seam — this exercises the real wiring (a real
    /// checkout with no remote, so rung 4 answers its own current branch) rather than
    /// re-proving `spira_config::repos`' own rungs, which have their own unit tests.
    #[test]
    fn base_resolves_through_spira_config_repos_with_no_remote() {
        let dir = testkit::TempDir::new("sending-real-base");
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-q", "-b", "trunk"]);
        std::fs::write(repo.join("f"), "x").unwrap();
        run_git(&repo, &["add", "f"]);
        run_git(&repo, &["commit", "-q", "-m", "x"]);
        let r = Real::minimal(dir.path().to_path_buf(), None);
        let base = r.base(&repo).expect("a repo with no remote resolves through rung 4");
        assert_eq!(base.landref, "trunk");
        assert_eq!(base.landrefs, vec!["trunk".to_string()]);
        assert_eq!(base.remote, None);
    }

    #[test]
    fn base_is_none_for_a_path_that_is_not_a_git_checkout() {
        let dir = testkit::TempDir::new("sending-real-base-not-a-repo");
        let r = Real::minimal(dir.path().to_path_buf(), None);
        assert_eq!(r.base(dir.path()), None);
    }
}

/// `spira.run` resolved in-process (env override, then `spira.toml`); the process refuses,
/// named, when it cannot — an empty run directory would put the reap log at the filesystem
/// root.
pub fn run_dir() -> PathBuf {
    spira_config::resolve::run_dir_for_process().unwrap_or_else(|e| {
        eprintln!("sending: {e}");
        std::process::exit(1)
    })
}

/// `$SPIRA_REAPLOG`, else `reap.log` under [`run_dir`].
pub fn reaplog_path() -> PathBuf {
    match std::env::var("SPIRA_REAPLOG") {
        Ok(rl) if !rl.is_empty() => PathBuf::from(rl),
        _ => run_dir().join(REAPLOG_NAME),
    }
}

const REAPLOG_NAME: &str = "reap.log";
