//! The pr-mode pass (`landing-pass --pass`, spira-landing-pass.timer): for every pr-mode
//! repository, for every spira/* branch whose bead is done — rebase, confine, force-push,
//! open or refresh the pull request, observe merged/closed — through pr-pass-branch.sh, one
//! branch at a time. The pull request's own CI is the gate; gate.sh is never called here.
//!
//! Behaviour is this crate's pr pass as it was, with the repository rows and settings now
//! coming from the one resolver (DESIGN.md §8 D1–D2).

use crate::model::{BeadRow, LandMode, RepoRow, Settings};
use crate::ports::{Beads, Git, Procs};
use crate::report::Reporter;
use crate::util::{command, run_capture, unix_now};
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

pub trait PrTools {
    /// pr-pass-branch.sh <repo> <br> <id> <base> <name> <tip> → its exit status.
    fn branch_helper(&self, repo: &Path, br: &str, id: &str, base: &str, name: &str, tip: &str) -> i32;
    /// Record pr mode's `merged` exit by content proof when the delivery row is PR_OPEN.
    /// Ok covers the quiet cases (delivered, no row, a row not in PR_OPEN); Err is a machine
    /// that could not be asked or refused the event. Called only with the switch ON.
    fn deliver_by_content(&self, id: &str, merge_sha: &str) -> Result<(), String>;
}

pub struct PrPass<'a> {
    pub s: &'a Settings,
    pub repos: &'a [RepoRow],
    pub beads: &'a dyn Beads,
    pub git: &'a dyn Git,
    pub procs: &'a dyn Procs,
    pub tools: &'a dyn PrTools,
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
            "landing-pass: {id}: LIFECYCLE: lifecycle_enforce is on and the Delivered event did not happen ({why}) — the delivery row stays PR_OPEN"
        );
        self.loud.borrow_mut().push(line.clone());
        eprintln!("{line}");
    }

    /// Returns (branches seen, branches acted on).
    pub fn run(&self) -> (u64, u64) {
        let start = unix_now();
        let pr: Vec<&RepoRow> = self.repos.iter().filter(|r| r.mode == LandMode::Pr).collect();
        if pr.is_empty() {
            self.log("landing-pass: no pr-mode repositories in the repository map");
            return (0, 0);
        }
        let (mut seen, mut acted) = (0u64, 0u64);
        for r in pr {
            let (s, a) = self.repo(r);
            seen += s;
            acted += a;
        }
        self.log(&format!(
            "landing-pass: complete — {seen} branches seen, {acted} acted, {}s",
            unix_now().saturating_sub(start)
        ));
        (seen, acted)
    }

    fn repo(&self, r: &RepoRow) -> (u64, u64) {
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
            self.git.fetch(&r.path, rem);
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
            if bead.status != "closed" {
                self.log(&format!("landing-pass {name}: {id} not landed — its bead is {}", bead.status));
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
                self.log(&format!("landing-pass {name}: {id} is superseded — leaving it for the Sending to reap"));
                continue;
            }
            if self.git.content_landed(&r.path, br, &base_fq) {
                self.log(&format!("landing-pass {name}: {base} already contains every change on {br} — nothing to land"));
                write_content_mark(&self.s.landstate(), id, tip);
                // OFF (production): the CONTENT record and nothing else — spira-lc is never run
                // (f031f6dee). ON: the Delivered event, best-effort additive; a machine that
                // cannot be asked or refuses is a loud LIFECYCLE: line and the pass goes on.
                if self.s.lifecycle_enforce {
                    if let Some(sha) = self.git.rev_parse(&r.path, &base_fq) {
                        if let Err(why) = self.tools.deliver_by_content(id, &sha) {
                            self.lifecycle_loud(id, &why);
                        }
                    }
                }
                continue;
            }
            if self.procs.holder_alive(id) {
                self.log(&format!("landing-pass {name}: a live aeon still holds {br} — deferring"));
                continue;
            }
            // 0 opened/refreshed; 7 merged, delivered; 8 closed unmerged, returned.
            if matches!(self.tools.branch_helper(&r.path, br, id, &base, name, tip), 0 | 7 | 8) {
                acted += 1;
            }
        }
        (refs.len() as u64, acted)
    }
}

/// The CONTENT record ("<state> <tip> <at>"), written directly as this pass always has —
/// the pr pass runs every 90 s and never sourced lib.sh for it.
fn write_content_mark(dir: &Path, id: &str, tip: &str) {
    let _ = crate::util::atomic_write(&dir.join(id), &format!("CONTENT {tip} {}", unix_now()));
}

pub struct RealPrTools<'a> {
    pub s: &'a Settings,
    pub out: &'a Reporter,
}

impl<'a> PrTools for RealPrTools<'a> {
    fn branch_helper(&self, repo: &Path, br: &str, id: &str, base: &str, name: &str, tip: &str) -> i32 {
        // A bare name (the default) is the launcher-PATH program, run directly; a configured
        // SPIRA_PR_PASS_BRANCH_SH path is run with bash.
        let mut c = if self.s.pr_pass_branch_sh.components().count() == 1 {
            command(&self.s.pr_pass_branch_sh)
        } else {
            let mut c = command("bash");
            c.arg(&self.s.pr_pass_branch_sh);
            c
        };
        c.arg(repo).arg(br).arg(id).arg(base).arg(name).arg(tip);
        c.env("SPIRA_HOME", &self.s.home).env("SPIRA_RUN", &self.s.run).env("SPIRA_DB", &self.s.db);
        c.env("SPIRA_ID_PREFIX", &self.s.id_prefix).stdin(Stdio::null());
        // The switch as this pass resolved it is already pinned into the environment the
        // helper inherits (lifecycle::pin_for_children): OFF, SPIRA_LIFECYCLE_ENFORCE=0 and its
        // lc-delivery.sh calls never reach spira-lc.
        // The helper's own lines go straight to this pass's stdout (the unit's log).
        c.stdout(Stdio::inherit()).stderr(Stdio::inherit());
        match c.status() {
            Ok(st) => st.code().unwrap_or(1),
            Err(_) => 1,
        }
    }

    fn deliver_by_content(&self, id: &str, merge_sha: &str) -> Result<(), String> {
        let lc = self.s.lc_bin.as_ref().ok_or("no spira-lc program")?;
        let mut c = command(lc);
        c.arg("show").arg(id).stdin(Stdio::null());
        let (rc, so, se) = run_capture(c);
        if rc != 0 {
            let e = String::from_utf8_lossy(&se);
            return Err(format!("show exited {rc}: {}", e.lines().last().unwrap_or("").trim()));
        }
        let v: serde_json::Value = serde_json::from_slice(&so).map_err(|e| format!("show: unparsed reply: {e}"))?;
        // No delivery row, or one not in PR_OPEN: the ordinary, quiet case.
        let Some((state, version)) = delivery_of(v.to_string().as_bytes()) else { return Ok(()) };
        if state != "PR_OPEN" {
            return Ok(());
        }
        let kind = serde_json::json!({"Delivered": {"merge_sha": merge_sha, "proof": "merge-tree"}}).to_string();
        let mut e = command(lc);
        e.args(["event", "delivery", id, "--expect", "PR_OPEN", "--version", &version, "--actor", "landing-pass", "--kind", &kind]);
        e.stdin(Stdio::null());
        let (rc, _, se) = run_capture(e);
        if rc != 0 {
            let err = String::from_utf8_lossy(&se);
            return Err(format!("event exited {rc}: {}", err.lines().last().unwrap_or("").trim()));
        }
        eprintln!("landing-pass: {id}: delivery PR_OPEN -> delivered by content proof");
        Ok(())
    }
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
