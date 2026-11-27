//! The pr-mode pass (`landing-pass --pass`, spira-landing-pass.timer): for every pr-mode
//! repository, for every spira/* branch whose bead is done — rebase, confine, force-push,
//! open or refresh the pull request, observe merged/closed — one branch at a time, through
//! [`crate::pr_branch`] (sp-t4y60; used to shell out to pr-pass-branch.sh, now in-process).
//! The pull request's own CI is the gate; gate.sh is never called here.
//!
//! Behaviour is this crate's pr pass as it was, with the repository rows and settings now
//! coming from the one resolver (DESIGN.md §8 D1–D2).

use crate::model::{BeadRow, LandMode, RepoRow, Settings};
use crate::ports::{Beads, Git, Lib, Procs, Tools};
use crate::pr_branch;
use crate::records::{Against, Files, Graphs};
use crate::report::Reporter;
use crate::util::unix_now;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

pub trait PrTools {
    /// Record pr mode's `merged` exit by content proof when the delivery row is PR_OPEN.
    /// Ok covers the quiet cases (delivered, no row, a row not in PR_OPEN); Err is a machine
    /// that could not be asked or refused the event. Called only with the switch ON.
    fn deliver_by_content(&self, repo: &std::path::Path, id: &str, merge_sha: &str) -> Result<(), String>;
}

pub struct PrPass<'a> {
    pub s: &'a Settings,
    pub repos: &'a [RepoRow],
    pub beads: &'a dyn Beads,
    pub git: &'a dyn Git,
    pub lib: &'a dyn Lib,
    pub land_tools: &'a dyn Tools,
    pub procs: &'a dyn Procs,
    pub tools: &'a dyn PrTools,
    pub files: &'a Files,
    pub out: &'a Reporter,
    /// The LIFECYCLE: lines this pass printed on stderr (kept for tests).
    pub loud: std::cell::RefCell<Vec<String>>,
}

impl<'a> PrPass<'a> {
    fn log(&self, m: &str) {
        self.out.log(m);
    }

    fn lifecycle_loud(&self, id: &str, why: &str) {
        let line = format!(
            "landing-pass: {id}: LIFECYCLE: the Delivered event did not happen ({why}) — the delivery row stays PR_OPEN"
        );
        self.loud.borrow_mut().push(line.clone());
        eprintln!("{line}");
    }

    /// Returns (branches seen, branches acted on).
    pub fn run(&self) -> (u64, u64) {
        if !self.s.batcher_forge_enable {
            self.log("landing-pass: SPIRA_BATCHER_FORGE_ENABLE=0 — the operator lands by hand; the pr walk is held");
            return (0, 0);
        }
        let start = unix_now();
        let pr: Vec<&RepoRow> = self.repos.iter().filter(|r| r.mode == LandMode::Pr).collect();
        if pr.is_empty() {
            self.log("landing-pass: no pr-mode repositories in the repository map");
            return (0, 0);
        }
        let (mut seen, mut acted) = (0u64, 0u64);
        let mut fetched = HashSet::new();
        for r in pr {
            let graphs = Graphs::default();
            let (s, a) = self.repo(r, &graphs, &mut fetched);
            graphs.write(self.files, &r.name);
            seen += s;
            acted += a;
        }
        self.log(&format!(
            "landing-pass: complete — {seen} branches seen, {acted} acted, {}s",
            unix_now().saturating_sub(start)
        ));
        (seen, acted)
    }

    fn repo(&self, r: &RepoRow, graphs: &Graphs, fetched: &mut HashSet<PathBuf>) -> (u64, u64) {
        let name = &r.name;
        if r.path.as_os_str().is_empty() || !r.path.join(".git").exists() {
            self.log(&format!("landing-pass {name}: {} is not a git checkout — skipped", r.path.display()));
            return (0, 0);
        }
        let Some(base) = r.landref.clone() else {
            self.log(&format!(
                "landing-pass {name}: cannot resolve base ref — skipped. Give it a `base` in the repository map."
            ));
            return (0, 0);
        };
        if let Some(rem) = &r.base_remote {
            if fetched.insert(r.path.clone()) {
                self.git.fetch(&r.path, rem);
            }
        }
        let refs = self.git.spira_refs(&r.path);
        if refs.is_empty() {
            return (0, 0);
        }
        let ids: Vec<String> = refs.iter().map(|(b, _)| b.trim_start_matches("spira/").to_string()).collect();
        let beads: HashMap<String, BeadRow> =
            self.beads.show(&ids).map(|v| v.into_iter().map(|b| (b.id.clone(), b)).collect()).unwrap_or_default();
        let mut sorted = refs.clone();
        sorted.sort_by(|(a, _), (b, _)| {
            let ka = beads.get(a.trim_start_matches("spira/")).map(|x| (x.priority, x.closed_at.clone()));
            let kb = beads.get(b.trim_start_matches("spira/")).map(|x| (x.priority, x.closed_at.clone()));
            let ka = ka.unwrap_or((9999, "9999-99-99".into()));
            let kb = kb.unwrap_or((9999, "9999-99-99".into()));
            ka.cmp(&kb)
        });
        let base_fq = r.base_fq.clone().unwrap_or_else(|| base.clone());
        let mut acted = 0;
        for (br, tip) in &sorted {
            let id = br.trim_start_matches("spira/");
            let Some(bead) = beads.get(id) else {
                self.log(&format!("landing-pass {name}: {id} not in bead db — skipped"));
                continue;
            };
            if !bead.handed_on() && self.land_tools.forge_pr_state(&r.path, br).as_deref() == Some("merged") {
                let sha = self.git.rev_parse(&r.path, &base_fq).unwrap_or_else(|| base.clone());
                if self.lib.deliver_pr_merged(&r.path, id, br, &sha) {
                    self.log(&format!("landing-pass {name}: {id} is {} but {br}'s pull request is merged — recorded LANDED", bead.state));
                    acted += 1;
                    continue;
                }
            }
            if !bead.handed_on() {
                self.log(&format!("landing-pass {name}: {id} not landed — its bead is {}", bead.state));
                continue;
            }
            let bead_path = self.repos.iter().find(|x| x.name == bead.repo).map(|x| x.path.as_path());
            if bead_path != Some(r.path.as_path()) {
                self.log(&format!(
                    "landing-pass {name}: {id} is in {name} but the bead names repo:{} — not landing it here",
                    bead.repo
                ));
                continue;
            }
            if bead.superseded {
                self.log(&format!("landing-pass {name}: {id} is superseded — leaving it for the Hel to reap"));
                continue;
            }
            if self.files.content_settled(br, tip) {
                continue;
            }
            let at = Against { files: self.files, git: self.git, name, path: &r.path, base_fq: &base_fq };
            if graphs.content_on_base(at, br, tip) {
                self.log(&format!("landing-pass {name}: {base} already contains every change on {br} — nothing to land"));
                // The Delivered event is this mode's close: the branch's PR carries the evidence.
                // Best-effort additive; a machine that cannot be asked or
                // refuses is a loud LIFECYCLE: line, and the next pass tries again.
                let delivered = match self.git.rev_parse(&r.path, &base_fq) {
                    Some(sha) => match self.tools.deliver_by_content(&r.path, id, &sha) {
                        Ok(_) => true,
                        Err(why) => {
                            self.lifecycle_loud(id, &why);
                            false
                        }
                    },
                    None => false,
                };
                if delivered {
                    self.files.settle_content(br, tip);
                }
                continue;
            }
            if self.procs.holder_alive(id) {
                self.log(&format!("landing-pass {name}: a live dwarf still holds {br} — deferring"));
                continue;
            }
            // 0 opened/refreshed; 7 merged, delivered; 8 closed unmerged, returned.
            let ctx = pr_branch::Ctx {
                lib: self.lib,
                git: self.git,
                beads: self.beads,
                tools: self.land_tools,
                files: self.files,
                pr_refresh_max: self.s.pr_refresh_max,
                log: &|m: &str| self.log(&format!("landing-pass {name}: {m}")),
            };
            let rc = pr_branch::run(&ctx, &r.path, br, id, &base, name, tip, &base_fq, r);
            if matches!(rc, 0 | 7 | 8) {
                acted += 1;
            }
        }
        (refs.len() as u64, acted)
    }
}

pub struct RealPrTools<'a> {
    pub s: &'a Settings,
    pub out: &'a Reporter,
    /// A pinned store client (tests); `None` resolves one from `s.lc_bin`.
    pub lc: Option<spira_store::lc_call::LcClient>,
}

impl<'a> PrTools for RealPrTools<'a> {
    fn deliver_by_content(&self, repo: &std::path::Path, id: &str, merge_sha: &str) -> Result<(), String> {
        use spira_store::lifecycle_row::{deliver_on, Delivered};
        let lc = crate::lifecycle::client(self.lc.as_ref(), self.s.lc_bin.as_deref())?;
        let d = merged_delivery(repo, &format!("spira/{id}"), merge_sha);
        if deliver_on(&lc, id, &d, d.actor())? == Delivered::Recorded {
            eprintln!("landing-pass: {id}: delivered by content proof");
        }
        Ok(())
    }
}

/// The merged pull request's exit, with its proof read from `repo`.
pub fn merged_delivery(repo: &std::path::Path, branch: &str, merge_sha: &str) -> spira_store::lifecycle_row::Delivery {
    let git = hel::git::Git(repo);
    spira_store::lifecycle_row::Delivery::PrMerged { merge_sha: merge_sha.to_string(), content_on_base: git.content_on_base(branch, merge_sha), in_force: git.merged_proof(branch, merge_sha) }
}

/// `spira-lc show`'s delivery row → (state, version); version may be a string or a number.
pub fn delivery_of(json: &[u8]) -> Option<(String, String)> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    let dv = v.get("delivery")?;
    if dv.is_null() {
        return None;
    }
    let state = dv.get("state")?.as_str()?.to_string();
    let version = match dv.get("version")? {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => return None,
    };
    Some((state, version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_store::lc_call::{LcClient, Reply, Transport};
    use std::sync::{Arc, Mutex};

    struct Store(Mutex<Vec<String>>, &'static str);

    impl Transport for Store {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: std::time::Duration) -> Result<Reply, String> {
            self.0.lock().unwrap().push(argv.join(" "));
            let (code, out) = match argv[0].as_str() {
                "deliver" => (2, "usage: spira-lc show <bead-id> | …".to_string()),
                "show" => (0, self.1.to_string()),
                _ => (0, String::new()),
            };
            Ok(Reply { code, out, err: String::new(), read_ended: false })
        }
    }

    fn deliver(row: &'static str) -> Vec<String> {
        let store = Arc::new(Store(Mutex::new(Vec::new()), row));
        let t = testkit::TempDir::new("pr-deliver");
        let s = Settings::for_run(t.path().to_path_buf());
        let out = Reporter::capture(None);
        let tools = RealPrTools { s: &s, out: &out, lc: Some(LcClient::over(store.clone(), true)) };
        tools.deliver_by_content(t.path(), "sp-a", "abc").unwrap();
        let seen = store.0.lock().unwrap().clone();
        seen
    }

    #[test_id(ti4r3ma)]
    #[test]
    fn a_pr_open_delivery_is_delivered_by_an_event_never_the_deliver_verb() {
        let seen = deliver(r#"{"bead":{"state":"SUBMITTED","version":4},"delivery":{"state":"PR_OPEN","version":6}}"#);
        assert!(seen.iter().all(|l| !l.starts_with("deliver")), "{seen:?}");
        assert!(seen.iter().any(|l| l.starts_with("event delivery sp-a --expect PR_OPEN --version 6 --actor pr-pass-branch")), "{seen:?}");
    }

    #[test_id(tb9zii5)]
    #[test]
    fn a_row_with_no_delivery_and_no_proof_in_force_sends_no_event() {
        let seen = deliver(r#"{"bead":{"state":"SUBMITTED","version":4},"delivery":null}"#);
        assert_eq!(seen, vec!["show sp-a"]);
    }
}
