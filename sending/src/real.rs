//! The real world: the destruction chokepoint in-process (`reap.rs`, sp-9envm); what has
//! not moved to Rust yet (context/base/bead — families U/W/A/B) through the lib.sh seam;
//! `gh` for the one forge question; `spira-lc` by bare name on the launcher PATH.

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
        let mut repos = Vec::new();
        for rec in a.text.split('\0').filter(|s| !s.is_empty()) {
            let Some((k, v)) = rec.split_once('=') else { continue };
            if k == "repo" {
                let f: Vec<&str> = v.split(FIELD).collect();
                if f.len() != 4 {
                    return Err(format!("malformed repository record {v:?}"));
                }
                repos.push(Repo { name: f[0].into(), root: (f[1] == "1").then(|| PathBuf::from(f[2])), queued: f[3] == "1" });
            } else {
                r.settings.insert(k.into(), v.into());
            }
        }
        if repos.is_empty() {
            return Err("spira_repos names no repository — nothing could be judged".into());
        }
        r.submitted_label = r.setting("submitted", "spira-submitted");
        Ok((r, repos))
    }

    /// No context seam call at all — just `home`/`status`, for a caller that only needs
    /// the Base/Status/Bead seams (each self-contained) and never the repository registry.
    pub fn minimal(home: PathBuf, status: Option<String>) -> Real {
        Real { home, status: status.unwrap_or_default(), settings: BTreeMap::new(), submitted_label: String::new(), enforce: spira_config::lifecycle_enforce(None) }
    }

    fn setting(&self, k: &str, default: &str) -> String {
        self.settings.get(k).filter(|v| !v.is_empty()).cloned().unwrap_or_else(|| default.to_string())
    }

    /// `$SPIRA_RUN`, read straight from the environment — not through the context seam's
    /// own `run=` echo of the same variable, so this works whether or not that seam ran.
    pub fn run(&self) -> PathBuf {
        PathBuf::from(std::env::var("SPIRA_RUN").unwrap_or_default())
    }

    /// `${SPIRA_REAPLOG:-$SPIRA_RUN/reap.log}` — the chokepoint's own log, as a real path
    /// (distinct from the `World::reaplog` trait method, which is display text for a "see
    /// …" message and keeps its existing placeholder default unchanged).
    pub fn reaplog_path(&self) -> PathBuf {
        match std::env::var("SPIRA_REAPLOG") {
            Ok(rl) if !rl.is_empty() => PathBuf::from(rl),
            _ => self.run().join("reap.log"),
        }
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
        let a = self.seam(Op::Base, &[&p(root)]);
        if a.rc != 0 {
            return None;
        }
        let f: Vec<&str> = a.text.split(FIELD).collect();
        let landref = f.first().filter(|s| !s.is_empty())?.to_string();
        let landrefs = f.get(1).map(|s| s.split_whitespace().map(String::from).collect()).unwrap_or_default();
        let remote = f.get(2).filter(|s| !s.is_empty()).map(|s| s.to_string());
        Some(Base { landref, landrefs, remote })
    }
    fn witness(&self, id: &str) -> Option<String> {
        reap::holder_witnesses(&self.run(), id, self)
    }
    fn bead(&self, id: &str) -> Option<Value> {
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
