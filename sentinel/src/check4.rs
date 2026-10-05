//! CHECK 4 — the poison valve (audit worker), its closed-but-unlanded supplement, and the
//! stale-poison clear. Counting and the decision itself are spira-claim's (`counts`,
//! `decide`); this module reads the dedup stamps, acts on the tokens and writes the asks.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::host::{Io, Spec};
use crate::lifecycle::{hold_event, unhold_event};
use crate::model::Bead;
use crate::pass::Sentinel;
use crate::render::{bead_context, causes};
use crate::seams;
use crate::store::Snapshot;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub attempts: u32,
    pub requeues: u32,
    pub reclaims: u32,
}

/// `spira-claim counts` output: "<id>\t<att>\t<req>\t<rcl>" per line.
pub fn parse_counts(s: &str) -> HashMap<String, Counts> {
    s.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 4 || f[0].is_empty() {
                return None;
            }
            let n = |i: usize| f[i].trim().parse().unwrap_or(0);
            Some((
                f[0].to_string(),
                Counts {
                    attempts: n(1),
                    requeues: n(2),
                    reclaims: n(3),
                },
            ))
        })
        .collect()
}

/// The dedup stamps, as lib.sh reads them.
pub struct Stamps<'a> {
    pub poison_asked: &'a Path,
    pub requeue_asked: &'a Path,
    pub reclaim_asked: &'a Path,
    pub poison_lifted: &'a Path,
}

fn nonempty(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false)
}

impl<'a> Stamps<'a> {
    /// `requeue_asked` / `reclaim_asked`: the bead's file is non-empty.
    pub fn requeue(&self, id: &str) -> bool {
        nonempty(&self.requeue_asked.join(id))
    }
    pub fn reclaim(&self, id: &str) -> bool {
        nonempty(&self.reclaim_asked.join(id))
    }
    /// `poison_asked <id> <n>`: some line of the file is exactly n.
    pub fn poison(&self, id: &str, n: u32) -> bool {
        std::fs::read_to_string(self.poison_asked.join(id))
            .map(|s| s.lines().any(|l| l == n.to_string()))
            .unwrap_or(false)
    }
    /// `poison_lifted <id> <n>`: the last line is a count >= n.
    pub fn lifted(&self, id: &str, n: u32) -> bool {
        std::fs::read_to_string(self.poison_lifted.join(id))
            .ok()
            .and_then(|s| s.lines().last().map(str::to_string))
            .and_then(|l| l.trim().parse::<i64>().ok())
            .map(|last| last >= n as i64)
            .unwrap_or(false)
    }
    pub fn stamp(&self, id: &str, n: u32) -> String {
        let b = |x: bool| if x { "1" } else { "0" };
        format!(
            "{}:{}:{}:{}",
            b(self.requeue(id)),
            b(self.reclaim(id)),
            b(self.poison(id, n)),
            b(self.lifted(id, n))
        )
    }
}

/// `*_asked_mark <dir> <id> <n>`: append n.
pub fn mark(dir: &Path, id: &str, n: u32) {
    let _ = std::fs::create_dir_all(dir);
    crate::pass::append_line(&dir.join(id), &n.to_string());
}

pub const POISON_NOTE: &str = "Triaged by the groomer, not a human: it reads the charged sessions' final results and either credits the harness-caused attempts and lifts the poison (groomer unpoison) or splits/re-scopes the work. Any live holder keeps its claim and releases on its own exit path; no persona can claim it again while the ";

pub const REQUEUE_DEFAULT: &str = "close the bead if its work has already landed under a different id or is no longer needed; file a harness-defect bead if the deliverable was not a commit; otherwise label it needs-rebase so an aeon can resolve the conflict";
pub const RECLAIM_DEFAULT: &str = "close the bead if the work is no longer relevant; move it to a healthier lane if this box consistently kills workers; otherwise check infrastructure and re-queue when the box is stable";
pub const REQUEUE_TAIL: &str = "Every additional requeue costs one full aeon session and its context budget; no progress is made toward landing this work.";
pub const RECLAIM_TAIL: &str = "The workers above were killed by the infrastructure before they could act. This is a fact about the box, not the work.";
pub const FROM: &str = "Sentinel <sentinel@spira>";

/// The mail body every CHECK 4 ask shares.
pub fn ask_body(subject: &str, default: &str, middle: &str, tail: &str) -> String {
    format!("## Question\n{subject}\n\n## Default\n{default}\n\n{middle}\n\n{tail}\n")
}

impl<'a> Sentinel<'a> {
    fn stamps(&self) -> Stamps<'_> {
        Stamps {
            poison_asked: &self.cfg.poison_asked,
            requeue_asked: &self.cfg.requeue_asked,
            reclaim_asked: &self.cfg.reclaim_asked,
            poison_lifted: &self.cfg.poison_lifted,
        }
    }

    /// `spira-claim counts` over the ids (on stdin). Err(rc) = cannot tell.
    fn claim_counts(&self, ids: &[&str]) -> Result<HashMap<String, Counts>, i32> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let bin = self.cfg.claim_bin.clone();
        let o = self.h.run(
            Spec::args_owned(bin, vec!["counts".into()])
                .stdin(format!("{}\n", ids.join("\n")))
                .err(Io::Inherit),
        );
        if o.ok() {
            Ok(parse_counts(&o.stdout))
        } else {
            Err(o.rc)
        }
    }

    /// `spira-claim decide` → the token set; None when the call failed (decide nothing).
    fn claim_decide(
        &self,
        n: u32,
        rq: u32,
        rc: u32,
        labels: &str,
        stamp: &str,
        poisoned: Option<bool>,
    ) -> Option<HashSet<String>> {
        let bin = self.cfg.claim_bin.clone();
        let mut a = vec![
            "decide".to_string(),
            "--poison-at".into(),
            self.cfg.poison_at.to_string(),
            "--requeue-at".into(),
            self.cfg.requeue_at.to_string(),
            "--reclaim-at".into(),
            self.cfg.reclaim_at.to_string(),
            "--".into(),
            n.to_string(),
            rq.to_string(),
            rc.to_string(),
            labels.to_string(),
            stamp.to_string(),
        ];
        if let Some(p) = poisoned {
            a.push(if p { "1".into() } else { "0".into() });
        }
        let o = self.h.run(Spec::args_owned(bin, a).err(Io::Inherit));
        o.ok()
            .then(|| o.stdout.split_whitespace().map(str::to_string).collect())
    }

    fn context_of(&self, snap: &Snapshot, id: &str) -> String {
        match snap.get(id) {
            Some(b) => bead_context(b, self.h.now()),
            None => format!("(could not read {id})"),
        }
    }

    fn repo_of(&self, labels: &str) -> String {
        labels
            .split(',')
            .find_map(|l| l.strip_prefix("repo:"))
            .filter(|r| !r.is_empty())
            .unwrap_or(&self.cfg.home_repo)
            .to_string()
    }

    fn repo_root(&self, name: &str) -> Option<String> {
        self.ctx.repo(name).and_then(|r| r.root.clone())
    }

    fn repo_base(&self, name: &str) -> Option<String> {
        self.ctx
            .repo(name)
            .and_then(|r| r.base().map(str::to_string))
    }

    /// One lifecycle read serves the main loop and the stale clear. A hold this pass applies
    /// is on a bead at or over the threshold, which the clear would never release anyway.
    pub fn check4(&self, snap: &Snapshot) {
        if self.lc == crate::cfg::Lifecycle::Off {
            self.log("CHECK4 lifecycle_enforce is off — no lifecycle machine to read, no poison decision");
            return;
        }
        let poisoned = self.poisoned_set();
        self.check4_main(snap, poisoned.as_ref());
        self.check4_closed(snap);
        if let Some(p) = &poisoned {
            self.check4_stale_clear(snap, p);
        }
    }

    /// Which beads are poisoned: the lifecycle poison hold. None when the machine cannot
    /// answer (decide nothing).
    fn poisoned_set(&self) -> Option<HashSet<String>> {
        self.lc_rows().map(|rows| {
            rows.into_iter()
                .filter(|r| r.holds.iter().any(|h| h == "poison"))
                .map(|r| r.bead_id)
                .collect()
        })
    }

    fn poison_write(&self, id: &str, n: u32) {
        let ev = hold_event(
            "poison",
            &format!("poisoned after {n} in_progress transition(s) without landing"),
        )
        .unwrap_or_default();
        self.lc_apply(id, &ev);
    }

    fn poison_lift(&self, id: &str) {
        self.lc_apply(id, &unhold_event("poison").unwrap_or_default());
    }

    fn check4_main(&self, snap: &Snapshot, poisoned: Option<&HashSet<String>>) {
        let parts = &self.ctx.partitions;
        if parts.is_empty() {
            self.h.log_err("WARN no persona in the chamber declares a partition — no bead is dispatchable, and none is being examined");
        }
        let disp = snap.dispatchable(parts);
        self.log(&format!(
            "CHECK4 examining {} dispatchable bead(s), poison={} requeue={} reclaim={}",
            disp.len(),
            self.cfg.poison_at,
            self.cfg.requeue_at,
            self.cfg.reclaim_at
        ));
        let ids: Vec<&str> = disp.iter().map(|(i, _)| i.as_str()).collect();
        let counts = match self.claim_counts(&ids) {
            Ok(c) => c,
            Err(rc) => {
                self.log(&format!("CHECK4 bulk attempts query failed (rc={rc}) — making no poison/requeue/reclaim decision this pass"));
                return;
            }
        };
        let Some(poisoned) = poisoned else {
            self.log("CHECK4 lifecycle read failed — making no poison/requeue/reclaim decision this pass");
            return;
        };
        let st = self.stamps();
        for (id, labels) in &disp {
            let c = counts.get(id).copied().unwrap_or_default();
            let n = c.attempts;
            let stamp = st.stamp(id, n);
            let Some(tok) = self.claim_decide(
                n,
                c.requeues,
                c.reclaims,
                labels,
                &stamp,
                Some(poisoned.contains(id)),
            ) else {
                continue;
            };

            if tok.contains("requeue-mail") {
                let causes = causes(labels, "requeue");
                let subj = format!(
                    "Spira bead {id} — completed and requeued {} times, never landed ({causes}) — the harness cannot land it",
                    c.requeues
                );
                let ev = format!(
                    "{}\n\nREQUEUES  {} (cap {}) — causes: {causes}\nATTEMPTS  {n} — distinct from requeues; a requeue is not a failed attempt and was not charged",
                    self.context_of(snap, id),
                    c.requeues,
                    self.cfg.requeue_at
                );
                if self.mail(
                    FROM,
                    &subj,
                    REQUEUE_DEFAULT,
                    &ask_body(&subj, REQUEUE_DEFAULT, &ev, REQUEUE_TAIL),
                    id,
                    true,
                ) {
                    mark(&self.cfg.requeue_asked, id, c.requeues);
                } else {
                    self.log(&format!(
                        "CHECK4 {id}: requeue escalation path refused the ask — retries next pass"
                    ));
                }
            }

            if tok.contains("reclaim-mail") {
                let causes = causes(labels, "reclaim");
                let subj = format!(
                    "Spira bead {id} — {} aeons died holding it, work never judged ({causes}) — the box cannot run it",
                    c.reclaims
                );
                let ev = format!(
                    "{}\n\nRECLAIMS  {} (cap {}) — causes: {causes}\nATTEMPTS  {n} — distinct from reclaims; no attempt was ever charged",
                    self.context_of(snap, id),
                    c.reclaims,
                    self.cfg.reclaim_at
                );
                if self.mail(
                    FROM,
                    &subj,
                    RECLAIM_DEFAULT,
                    &ask_body(&subj, RECLAIM_DEFAULT, &ev, RECLAIM_TAIL),
                    id,
                    true,
                ) {
                    mark(&self.cfg.reclaim_asked, id, c.reclaims);
                } else {
                    self.log(&format!(
                        "CHECK4 {id}: reclaim escalation path refused the ask — retries next pass"
                    ));
                }
            }

            if tok.contains("poison") || tok.contains("ask") {
                // Re-read before the write (fresh.rs): the snapshot is a snapshot, and the
                // landing pass or an aeon may have moved this bead since it was taken.
                let was = snap.get(id).map(|b| b.status.clone()).unwrap_or_default();
                let live = self.reread(&[id.as_str()]);
                if let Some(b) = live.as_ref().and_then(|m| m.get(id.as_str())) {
                    if b.status == "closed" && was != "closed" {
                        self.log(&format!("CHECK4 {id}: {n} attempts, but it closed while this pass ran — not poisoned, not asked"));
                        continue;
                    }
                }
                let Some(shown) = self.still("CHECK4", id, &was, live.as_ref()).cloned() else {
                    continue;
                };
                let shown = Some(shown);
                if tok.contains("poison") {
                    self.poison_write(id, n);
                    let note = format!(
                        "Poisoned after {n} in_progress transition(s) without landing. {POISON_NOTE}hold stands.",
                    );
                    self.bd()
                        .quiet(self.h, &["note", id, "--stdin"], Some(&note));
                    self.progress(&format!("poisoned {id} after {n} attempts"));
                    self.event(
                        "bead.poisoned",
                        id,
                        &format!("poisoned {id} after {n} attempts"),
                        "the groomer triages it, not a human",
                    );
                }
                if tok.contains("ask") {
                    self.poison_ask(id, labels, n, shown.as_ref());
                }
            }
        }
    }

    /// The poison ask: the bead first, then the failure (DESIGN.md §4, CHECK 4 step 5).
    fn poison_ask(&self, id: &str, labels: &str, n: u32, shown: Option<&Bead>) {
        let mut ev = match shown {
            Some(b) => bead_context(b, self.h.now()),
            None => "(could not read the bead — say so rather than pretend)".to_string(),
        };
        let r_name = self.repo_of(labels);
        let r_path = self.repo_root(&r_name);
        let mut branch_info = "none — nothing was committed".to_string();
        if let Some(p) = &r_path {
            if self
                .git(
                    p,
                    &[
                        "show-ref",
                        "--verify",
                        "-q",
                        &format!("refs/heads/spira/{id}"),
                    ],
                )
                .ok()
            {
                let base = self.repo_base(&r_name).unwrap_or_default();
                let range = if base.is_empty() {
                    format!("spira/{id}")
                } else {
                    format!("{base}..spira/{id}")
                };
                let o = self.git(p, &["rev-list", "--count", &range]);
                let nc = if o.ok() {
                    o.stdout.trim().to_string()
                } else {
                    "?".into()
                };
                let ahead = if base.is_empty() {
                    String::new()
                } else {
                    format!(" ahead of {base}")
                };
                if nc == "0" || nc == "?" {
                    branch_info = format!("spira/{id} exists, no commits{ahead}");
                } else {
                    let ds = self.git(p, &["diff", "--stat", &range]).stdout;
                    let last = ds.lines().last().unwrap_or("").to_string();
                    branch_info = if last.is_empty() {
                        format!("spira/{id} — {nc} commit(s)")
                    } else {
                        format!("spira/{id} — {nc} commit(s); {last}")
                    };
                }
            }
        }
        // trace_tail (wave 4.34, sp-27d3d): ported to aeon::trace, called in-process —
        // no more bash seam (S6) for it.
        let log = self.cfg.run.join(format!("{id}.log"));
        let trace_mark = self.ctx.vars.get("SPIRA_TRACE_MARK").cloned().unwrap_or_else(|| "=== spira attempt".to_string());
        let tail = aeon::trace::trace_tail(&log, &trace_mark, 25);
        ev.push_str(&format!(
            "\n\nREPO      {r_name}{}\nATTEMPTS  {n} (poison threshold {}) — each in_progress transition from the events trail\nBRANCH    {branch_info}\n\n--- last session log (tail) ---\n{}",
            r_path.as_ref().map(|p| format!(" ({p})")).unwrap_or_default(),
            self.cfg.poison_at,
            tail.trim_end_matches('\n')
        ));
        let subj = format!("Spira bead {id} — {n} in_progress transition(s) without landing ({n} attempts) — change the approach or drop it?");
        let dflt = format!("if the work is correct, re-label or split the bead and run spira-claim unpoison --bead {id} --cause <why>; if it is not worth doing, close it");
        let body = format!(
            "## Question\n{subj}\n\n## Default\n{dflt}\n\nnothing downstream of it can proceed, and no aeon will take it again while it is poisoned\n\n{ev}\n"
        );
        if self.mail(FROM, &subj, &dflt, &body, id, false) {
            mark(&self.cfg.poison_asked, id, n);
        } else {
            self.log(&format!("CHECK4 {id}: the escalation path refused the ask — it stands, and the next pass retries it"));
        }
    }

    /// lib.sh `spira_event`, through its seam (S5).
    pub fn event(&self, kind: &str, target: &str, title: &str, detail: &str) {
        let mut b = Vec::new();
        for f in [kind, target, title, detail] {
            b.extend_from_slice(f.as_bytes());
            b.push(0);
        }
        self.seam("event", seams::EVENT, Some(b), Io::Null, Io::Null, false);
    }

    /// lib.sh `landed <id> <repo>`: a base subject that lands it, never a mention.
    pub fn landed(&self, id: &str, repo: &str, refs: &[String]) -> bool {
        if refs.is_empty() {
            return false;
        }
        let mut a = vec!["log", "--format=%s", "--grep", id, "-F"];
        a.extend(refs.iter().map(String::as_str));
        let o = self.git(repo, &a);
        o.stdout.lines().any(|s| {
            s == format!("spira: land {id}")
                || s.starts_with(&format!("spira: land {id} "))
                || s.starts_with(&format!("{id}:"))
        })
    }

    /// The lifecycle machine records the bead LANDED.
    pub fn lc_landed(&self, id: &str) -> bool {
        self.lc_rows()
            .is_some_and(|rows| rows.iter().any(|r| r.bead_id == id && r.state == "LANDED"))
    }

    /// The requeue cap for closed-but-unlanded beads: dispatchable_open drops them.
    fn check4_closed(&self, snap: &Snapshot) {
        let closed = snap.closed_branched(&self.ctx.partitions);
        if closed.is_empty() {
            return;
        }
        let ids: Vec<&str> = closed.iter().map(|(i, _)| i.as_str()).collect();
        let counts = match self.claim_counts(&ids) {
            Ok(c) => c,
            Err(rc) => {
                self.log(&format!("CHECK4-closed bulk attempts query failed (rc={rc}) — making no requeue decision this pass"));
                return;
            }
        };
        let st = self.stamps();
        for (id, labels) in &closed {
            let rq = counts.get(id).map(|c| c.requeues).unwrap_or(0);
            let stamp = format!("{}:1:1", if st.requeue(id) { 1 } else { 0 });
            let Some(tok) = self.claim_decide(0, rq, 0, labels, &stamp, None) else {
                continue;
            };
            if !tok.contains("requeue-mail") {
                continue;
            }
            let r_name = self.repo_of(labels);
            let r_path = self.repo_root(&r_name);
            if let Some(p) = &r_path {
                let refs = self
                    .ctx
                    .repo(&r_name)
                    .map(|r| r.landrefs.clone())
                    .unwrap_or_default();
                if self.landed(id, p, &refs) {
                    self.log(&format!(
                        "CHECK4-closed {id}: reopens={rq} but already landed — no escalation"
                    ));
                    continue;
                }
            }
            if self.lc_landed(id) {
                self.log(&format!("CHECK4-closed {id}: lifecycle records LANDED — no escalation"));
                continue;
            }
            let causes = causes(labels, "requeue");
            let subj = format!("Spira bead {id} — completed and requeued {rq} times, never landed ({causes}) — the harness cannot land it");
            let ev = format!(
                "{}\n\nREQUEUES  {rq} (cap {}) — causes: {causes}\nSTATUS    closed (not landed in {})",
                self.context_of(snap, id),
                self.cfg.requeue_at,
                if r_name.is_empty() { "unknown" } else { &r_name }
            );
            if self.mail(
                FROM,
                &subj,
                REQUEUE_DEFAULT,
                &ask_body(&subj, REQUEUE_DEFAULT, &ev, REQUEUE_TAIL),
                    id,
                true,
            ) {
                mark(&self.cfg.requeue_asked, id, rq);
            } else {
                self.log(&format!("CHECK4-closed {id}: requeue escalation path refused the ask — retries next pass"));
            }
        }
    }

    /// A poison hold whose count has fallen below the threshold is released.
    /// A poison whose count has fallen below the threshold is lifted — found independently
    /// of the dispatchable set, which excludes a poisoned bead (sp-9szt).
    fn check4_stale_clear(&self, snap: &Snapshot, poisoned: &HashSet<String>) {
        let mut held: Vec<&str> = poisoned
            .iter()
            .map(String::as_str)
            .filter(|id| match snap.get(id) {
                Some(b) => b.status != "closed" && !matches!(b.typ(), "epic" | "event"),
                None => true,
            })
            .collect();
        held.sort_unstable();
        if held.is_empty() {
            return;
        }
        let counts = self.claim_counts(&held);
        for id in held {
            let n = match &counts {
                Ok(c) => c.get(id).map(|c| c.attempts).unwrap_or(0),
                Err(_) => {
                    self.log(&format!("CHECK4 {id}: attempts query failed — stale-poison-clear makes no decision this pass"));
                    continue;
                }
            };
            let Some(tok) = self.claim_decide(n, 0, 0, "", "1:1:1", Some(true)) else {
                continue;
            };
            if !tok.contains("clear") {
                continue;
            }
            self.poison_lift(id);
            self.progress(&format!(
                "CHECK4 {id}: stale poison cleared — {n} attempt(s), below threshold {}",
                self.cfg.poison_at
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_parse_and_skip_junk() {
        let c = parse_counts("a\t3\t1\t0\nb\t0\t5\t2\njunk\n");
        assert_eq!(
            c["a"],
            Counts {
                attempts: 3,
                requeues: 1,
                reclaims: 0
            }
        );
        assert_eq!(c["b"].reclaims, 2);
        assert!(!c.contains_key("junk"));
    }

    #[test]
    fn stamps_read_like_lib_sh() {
        let d = testkit::TempDir::new("sentinel-stamps");
        let (pa, rq, rc, pl) = (d.join("pa"), d.join("rq"), d.join("rc"), d.join("pl"));
        for x in [&pa, &rq, &rc, &pl] {
            std::fs::create_dir_all(x).unwrap();
        }
        let st = Stamps {
            poison_asked: &pa,
            requeue_asked: &rq,
            reclaim_asked: &rc,
            poison_lifted: &pl,
        };
        assert_eq!(st.stamp("x", 3), "0:0:0:0");
        mark(&pa, "x", 3);
        mark(&rq, "x", 5);
        std::fs::write(pl.join("x"), "2\n4\n").unwrap();
        assert!(st.poison("x", 3) && !st.poison("x", 4));
        assert!(st.lifted("x", 4) && !st.lifted("x", 5));
        assert_eq!(st.stamp("x", 3), "1:0:1:1");
        std::fs::write(rc.join("y"), "").unwrap();
        assert!(!st.reclaim("y"), "an empty file is not an ask (-s)");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ask_body_shape() {
        assert_eq!(
            ask_body("S", "D", "EV", "TAIL"),
            "## Question\nS\n\n## Default\nD\n\nEV\n\nTAIL\n"
        );
    }
}
