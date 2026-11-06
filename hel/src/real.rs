//! The real world: the destruction chokepoint in-process (`reap.rs`, sp-9envm); base
//! (family W) in-process too (sp-o88bx, "wave 4.12", `spira_config::repos`); what has not
//! moved too: the bead reads, the landed close and the label removal the lib.sh seam used to
//! answer are the store's own verbs over [`LcClient`], in-process. `gh` is the one child, for
//! the one forge question.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;
use spira_config::lc_call::LcClient;

use crate::ports::{Base, Repo, Sent, World};
use crate::reap;

/// The settings the retired lib.sh context seam echoed: `reaplog` (display text for "see …")
/// and `gh_timeout` (`${GH_TIMEOUT:-120}`), read from the same environment lib.sh saw.
pub fn context_settings(env: &dyn Fn(&str) -> Option<String>) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("reaplog".to_string(), env("SPIRA_REAPLOG").unwrap_or_default());
    m.insert("gh_timeout".to_string(), env("GH_TIMEOUT").filter(|v| !v.is_empty()).unwrap_or_else(|| "120".into()));
    m
}

/// lib.sh `json_only`: the answer from its first `[` or `{` line on, parsed.
pub fn json_only(text: &str) -> Option<Value> {
    let start = text.lines().position(|l| matches!(l.trim_start().chars().next(), Some('[' | '{')))?;
    let body: Vec<&str> = text.lines().skip(start).collect();
    serde_json::from_str(body.join("\n").trim()).ok()
}

pub struct Real {
    home: PathBuf,
    run: PathBuf,
    status: String,
    settings: BTreeMap<String, String>,
    pub submitted_label: String,
    /// `SPIRA_GH`, a registered config key — empty for a `Real::minimal` instance (the
    /// chokepoint subcommands never call `pr_merged_tip`, so it is never resolved for them).
    gh: String,
    beads: RefCell<Option<BTreeMap<String, Value>>>,
    /// The sweep's one `spira-lc list`, keyed by bead id: the claim witness for every bead
    /// the pass judges (sp-mve9i).
    claims: RefCell<Option<std::collections::HashMap<String, spira_config::lc_state::Row>>>,
    /// The store client; `None` resolves the configured one per call.
    lc: Option<LcClient>,
}

impl Real {
    /// Read the context once. Err: lib.sh could not be loaded, or it names no repository.
    /// Used by the full sweep, which needs the repository list; the chokepoint subcommands
    /// (destroy-worktree/destroy-branch/witness/reap-landed-branch) use `minimal` instead —
    /// they are handed a repo PATH directly, as the retired bash functions were, and must
    /// not be made to depend on the repo-name registry resolving at all.
    pub fn new(home: PathBuf, status: Option<String>) -> Result<(Real, Vec<Repo>), String> {
        let mut r = Real::minimal(home, run_dir()?, status);
        // The two settings the retired context seam echoed from lib.sh's environment.
        r.settings = context_settings(&|k| std::env::var(k).ok());
        // spira_repos/repo_root/repo_land_queued (family U) in-process (sp-k6lku, "wave
        // 4.13"), not this seam's own loop — Registry::all() always puts the home repo
        // first, so (unlike the retired bash loop) this can no longer come back empty.
        let repos = r.repos();
        // SPIRA_SUBMITTED_LABEL/SPIRA_GH are registered config keys (spira/conf.d) — the one
        // source of config, read here rather than through the lib.sh context seam's own
        // (now-removed) `${VAR:-default}` echoes.
        r.submitted_label = spira_config::process::cfg("SPIRA_SUBMITTED_LABEL")?;
        r.gh = spira_config::process::cfg("SPIRA_GH")?;
        Ok((r, repos))
    }

    /// No context seam call at all — just `home`/`status`, for a caller that only needs
    /// the Base/Bead seams (each self-contained) and never the repository registry.
    pub fn minimal(home: PathBuf, run: PathBuf, status: Option<String>) -> Real {
        Real {
            home,
            run,
            status: status.unwrap_or_default(),
            settings: BTreeMap::new(),
            submitted_label: String::new(),
            gh: String::new(),
            beads: RefCell::new(None),
            claims: RefCell::new(None),
            lc: None,
        }
    }

    /// The forge program and its per-call timeout in seconds, for `pr_merged_tip`, from a
    /// caller that pinned them (the scheduler's `hel.sweep`).
    pub fn with_gh(mut self, gh: &str, timeout_secs: &str) -> Real {
        self.gh = gh.to_string();
        self.settings.insert("gh_timeout".to_string(), timeout_secs.to_string());
        self
    }

    /// Every store call through `c` instead of the configured client (a test's scripted one).
    pub fn with_lc(mut self, c: LcClient) -> Real {
        self.lc = Some(c);
        self
    }

    /// The store, in-process over the lifecycle socket.
    fn lc(&self) -> Result<LcClient, String> {
        match &self.lc {
            Some(c) => Ok(c.clone()),
            None => LcClient::configured(),
        }
    }

    /// One bead-content read (`spira-lc content <argv> --json`), parsed; `None` when the store
    /// did not answer with JSON.
    fn content_json(&self, argv: &[&str]) -> Option<Value> {
        let mut a = vec!["content"];
        a.extend_from_slice(argv);
        a.push("--json");
        let out = self.lc().ok()?.answer(&a).ok()?;
        json_only(&out)
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

    /// Every registered repository, the home repo first, from the registry in-process.
    pub fn repos(&self) -> Vec<Repo> {
        let reg = self.repo_registry();
        reg.all()
            .into_iter()
            .map(|name| {
                let root = reg.root(&name).map(PathBuf::from);
                let forge_queued = reg.land_forge_queued(&name);
                Repo { name, root, forge_queued }
            })
            .collect()
    }

    /// `SPIRA_RUN`, through [`run_dir`] — not through the context seam, so this works
    /// whether or not that seam ran.
    pub fn run(&self) -> PathBuf {
        self.run.clone()
    }

    /// `${SPIRA_REAPLOG:-$SPIRA_RUN/reap.log}` — the chokepoint's own log, as a real path
    /// (distinct from the `World::reaplog` trait method, which is display text for a "see
    /// …" message and keeps its existing placeholder default unchanged).
    pub fn reaplog_path(&self) -> PathBuf {
        reaplog_path_in(&self.run)
    }

    /// `bdq label remove <id> <label>`, needed by
    /// `reap::reap_landed_branch` (law-branch-affinity-is-recorded) but not part of the
    /// `World` trait since nothing else in this crate calls it standalone.
    pub fn label_remove(&self, id: &str, label: &str) {
        if let Ok(c) = self.lc() {
            let _ = c.call(&["content", "update", id, "--remove-label", label]);
        }
    }

    /// `hel destroy-branch`'s own entry point: the trait only has the combined `send`
    /// (witness recheck + full reap), so a standalone destroy-branch (for bash callers that
    /// are not the sweep itself — `held.sh`, and lib.sh's `spira_destroy_branch` shim) goes
    /// through this instead.
    #[allow(clippy::too_many_arguments)]
    pub fn destroy_branch(&self, id: &str, br: &str, repo: &Path, why: &str, caller: &str, base: Option<&str>) -> Result<(), reap::DestroyBranchErr> {
        reap::destroy_branch(&self.run(), &self.reaplog_path(), id, br, repo, why, caller, base, self)
    }

    /// `hel salvage`'s own entry point.
    #[allow(clippy::result_unit_err)]
    pub fn salvage(&self, id: &str, w: &Path) -> Result<Option<PathBuf>, ()> {
        reap::salvage(&self.run(), &self.reaplog_path(), id, w)
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
        let Some(Value::Array(rows)) = self.content_json(&["list", "--all", "--limit", "0"]) else { return };
        let map: BTreeMap<String, Value> = rows.into_iter().filter_map(|r| Some((r.get("id")?.as_str()?.to_string(), r))).collect();
        if !map.is_empty() {
            *self.beads.borrow_mut() = Some(map);
        }
        if let Ok(rows) = self.lc().and_then(|c| spira_config::lc_state::list_on(&c)) {
            *self.claims.borrow_mut() = Some(spira_config::lc_state::index(rows));
        }
    }
    fn witness(&self, id: &str) -> Option<String> {
        if let Some(m) = self.claims.borrow().as_ref() {
            return reap::holder_witnesses(&self.run(), id, &Cached(m.get(id).cloned()));
        }
        reap::holder_witnesses(&self.run(), id, self)
    }
    fn bead(&self, id: &str) -> Option<Value> {
        if let Some(m) = self.beads.borrow().as_ref() {
            return m.get(id).cloned();
        }
        let v = self.content_json(&["show", id])?;
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
        if let Ok(c) = self.lc() {
            let _ = c.call(&["close-on-land", id, sha]);
        }
    }
    fn destroy_worktree(&self, id: &str, w: &Path, repo: &Path, why: &str) -> bool {
        reap::destroy_worktree(&self.run(), &self.reaplog_path(), id, w, repo, why, self)
    }
    fn prune(&self, repo: &Path) {
        reap::prune_worktrees(&self.reaplog_path(), repo);
    }
    fn lc_past_builder(&self, id: &str) -> bool {
        let row = match self.claims.borrow().as_ref() {
            Some(m) => m.get(id).cloned(),
            None => self.lc().and_then(|c| spira_config::lc_state::row_on(&c, id)).ok().flatten(),
        };
        row.is_some_and(|r| r.past_builder())
    }
    fn lc_terminal(&self, id: &str) -> bool {
        let row = match self.claims.borrow().as_ref() {
            Some(m) => m.get(id).cloned(),
            None => self.lc().and_then(|c| spira_config::lc_state::row_on(&c, id)).ok().flatten(),
        };
        row.is_some_and(|r| r.terminal())
    }
    fn lc_landed(&self, id: &str) -> bool {
        // Bounded: a hung record answers "not landed".
        self.lc().ok().and_then(|c| c.within(Duration::from_secs(5)).answer(&["state", id]).ok()).is_some_and(|s| s.trim() == "LANDED")
    }
    fn content_on_base(&self, id: &str, proof: &str) {
        if let Ok(c) = self.lc() {
            let _ = c.call(&["content-on-base", id, proof, "hel"]);
        }
    }
    fn pr_merged_tip(&self, repo: &Path, br: &str) -> Option<String> {
        let o = Command::new("timeout")
            .arg(self.setting("gh_timeout", "5"))
            .arg(&self.gh)
            .args(["pr", "view", br, "--json", "state,headRefOid", "-q", r#"select(.state=="MERGED") | .headRefOid"#])
            .current_dir(repo)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
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

/// A row from the prefetched `spira-lc list`: the listing answered, so a bead missing from it
/// has no row.
struct Cached(Option<spira_config::lc_state::Row>);

impl reap::ClaimProbe for Cached {
    fn probe(&self, _id: &str) -> Result<Option<spira_config::lc_state::Row>, String> {
        Ok(self.0.clone())
    }
}

impl reap::ClaimProbe for Real {
    /// `spira-lc show <id>`: the bead's lifecycle row, the one claim question
    /// `reap::holder_witnesses` cannot answer itself.
    fn probe(&self, id: &str) -> Result<Option<spira_config::lc_state::Row>, String> {
        spira_config::lc_state::row_on(&self.lc()?, id)
    }
}

const REAPLOG_NAME: &str = "reap.log";

/// `$SPIRA_REAPLOG`, else `reap.log` under `run`.
pub fn reaplog_path_in(run: &Path) -> PathBuf {
    match spira_config::process::cfg("SPIRA_REAPLOG") {
        Ok(rl) if !rl.is_empty() => PathBuf::from(rl),
        _ => run.join(REAPLOG_NAME),
    }
}

/// `SPIRA_RUN`, a registered config key (spira/conf.d) — the one source of config, through
/// `spira_config::process::cfg`; an error, named, when it cannot resolve. An empty run
/// directory would put the reap log at the filesystem root.
pub fn run_dir() -> Result<PathBuf, String> {
    spira_config::process::cfg("SPIRA_RUN").map(PathBuf::from).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_is_none_for_a_path_that_is_not_a_git_checkout() {
        let dir = testkit::TempDir::new("hel-real-base-not-a-repo");
        let r = Real::minimal(dir.path().to_path_buf(), dir.join("run"), None);
        assert_eq!(r.base(dir.path()), None);
    }

    /// The store as a scripted socket: every argv is recorded and answered by `reply`.
    struct Store {
        seen: std::sync::Mutex<Vec<String>>,
        reply: fn(&[String]) -> (i32, String),
    }

    impl spira_config::lc_call::Transport for Store {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: Duration) -> Result<spira_config::lc_call::Reply, String> {
            self.seen.lock().unwrap().push(argv.join(" "));
            let (code, out) = (self.reply)(argv);
            Ok(spira_config::lc_call::Reply { code, out, err: String::new() })
        }
    }

    fn over(reply: fn(&[String]) -> (i32, String)) -> (std::sync::Arc<Store>, Real, testkit::TempDir) {
        let dir = testkit::TempDir::new("hel-real-store");
        let store = std::sync::Arc::new(Store { seen: Default::default(), reply });
        let r = Real::minimal(dir.path().to_path_buf(), dir.join("run"), None).with_lc(LcClient::over(store.clone(), true));
        (store, r, dir)
    }

    #[test]
    fn the_bead_record_is_the_stores_content_verb_not_a_lib_sh_seam() {
        let (store, r, _d) = over(|a| match a.join(" ").as_str() {
            "content show sp-a --json" => (0, "warning: noise\n[{\"id\":\"sp-a\",\"status\":\"open\"}]\n".into()),
            _ => (1, String::new()),
        });
        assert_eq!(r.bead("sp-a").and_then(|b| b["status"].as_str().map(str::to_string)).as_deref(), Some("open"));
        assert_eq!(*store.seen.lock().unwrap(), vec!["content show sp-a --json"]);
    }

    #[test]
    fn the_writes_and_the_landed_read_go_to_the_store_in_process() {
        let (store, r, _d) = over(|a| match a[0].as_str() {
            "state" => (0, "LANDED\n".into()),
            _ => (0, String::new()),
        });
        r.label_remove("sp-a", "branch:x");
        r.close_on_land("sp-a", "abc");
        r.content_on_base("sp-a", "proof");
        assert!(r.lc_landed("sp-a"));
        assert_eq!(
            *store.seen.lock().unwrap(),
            vec!["content update sp-a --remove-label branch:x", "close-on-land sp-a abc", "content-on-base sp-a proof hel", "state sp-a"]
        );
    }

    #[test]
    fn a_store_that_does_not_say_landed_is_not_landed() {
        let (_s, r, _d) = over(|_| (2, "cannot tell".into()));
        assert!(!r.lc_landed("sp-a"));
        assert_eq!(r.bead("sp-a"), None);
    }

    #[test]
    fn the_context_settings_are_the_environment_lib_sh_saw() {
        let m = context_settings(&|k| (k == "SPIRA_REAPLOG").then(|| "/r/reap.log".to_string()));
        assert_eq!(m["reaplog"], "/r/reap.log");
        assert_eq!(m["gh_timeout"], "120");
    }
}
