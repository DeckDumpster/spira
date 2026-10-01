//! The rest of the audit worker — CHECK 6b (the Sending), 7c (unclaimable ready beads),
//! 7d (branch collisions) — and the full pass's last word, CHECK 8 (judgement).

use crate::cfg::Repo;
use crate::detect::ParkOutcome;
use crate::host::{Io, Spec};
use crate::pass::Sentinel;
use crate::store::Snapshot;

/// How many open plan beads judgement is shown. reflect.sh reads each one with its own
/// `bd show`, and the backlog is the whole open plan (hundreds), not one epic's handful of
/// children (sp-k6m1m) — so it gets a bounded sample, and the STARVED line carries the total.
pub const REFLECT_IDS: usize = 25;

/// Judgement's pure predicate (lib.sh `check8_should_judge`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Judge {
    Yes,
    Cooldown,
    No,
}

pub fn should_judge(
    plan_ready: Option<usize>,
    plan_inprog: usize,
    n_open: usize,
    progressed: u32,
    last: i64,
    now: i64,
    every: i64,
) -> Judge {
    // A plan_ready this pass could not read is not a starved plan (G3).
    let Some(plan_ready) = plan_ready else {
        return Judge::No;
    };
    if plan_ready > 0 {
        return Judge::No;
    }
    if plan_inprog == 0 && n_open > 0 && progressed == 0 {
        return if now - last < every {
            Judge::Cooldown
        } else {
            Judge::Yes
        };
    }
    Judge::No
}

/// The Sending's skip test: every stamped repo still at its sha, and every swept repo
/// stamped. `current` resolves a repo's landref sha (None: cannot resolve → walk).
pub fn sending_skip(
    stamp: &str,
    swept: &[&Repo],
    all: &[Repo],
    current: &dyn Fn(&Repo) -> Option<String>,
) -> bool {
    for line in stamp.lines() {
        let Some((name, sha)) = line.split_once('=') else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let Some(r) = all.iter().find(|r| r.name == name) else {
            return false;
        };
        if r.root.is_none() || r.base().is_none() {
            return false;
        }
        match current(r) {
            Some(cur) if cur == sha => {}
            _ => return false,
        }
    }
    swept.iter().all(|r| {
        stamp
            .lines()
            .any(|l| l.starts_with(&format!("{}=", r.name)))
    })
}

impl<'a> Sentinel<'a> {
    fn landref_sha(&self, r: &Repo) -> Option<String> {
        let (root, base) = (r.root.as_ref()?, r.base()?);
        let o = self.git(root, &["rev-parse", base]);
        o.ok()
            .then(|| o.stdout.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// CHECK 6b — the Sending, skipped when no swept repo's base has moved.
    pub fn check6b(&self) {
        let stamp_p = self.cfg.run.join("sending.base");
        let swept: Vec<&Repo> = self
            .ctx
            .repos
            .iter()
            .filter(|r| !r.queued && r.root.is_some() && r.base().is_some())
            .collect();
        let skip = match std::fs::read_to_string(&stamp_p) {
            Ok(stamp) => sending_skip(&stamp, &swept, &self.ctx.repos, &|r| self.landref_sha(r)),
            Err(_) => false,
        };
        if skip {
            self.log("sending: base unchanged — skipped");
            return;
        }
        let o = self.h.run(Spec::args_owned(
            self.cfg.sending_bin.clone(),
            vec!["--skip-queue".into()],
        ));
        let text = format!("{}{}", o.stdout, o.stderr);
        if !text.is_empty() {
            self.h.print(&text);
        }
        for l in text.lines().filter(|l| l.starts_with("SENT")) {
            let f: Vec<&str> = l.split_whitespace().collect();
            if let (Some(id), Some(repo), Some(br)) = (f.get(1), f.get(2), f.get(3)) {
                self.act(&format!("sent {repo} {br} {id}"));
            }
        }
        if text.lines().any(|l| l.starts_with("FAILED")) {
            self.log("sending reported a branch it could not delete");
        }
        let body: String = swept
            .iter()
            .filter_map(|r| self.landref_sha(r).map(|s| format!("{}={s}\n", r.name)))
            .collect();
        let _ = std::fs::write(&stamp_p, body);
    }

    /// CHECK 7c — ready beads no persona can claim: name them, file one incident each.
    /// Native now (wave 4.28, sp-fbqsv): the bash seams S7/S8 are retired.
    pub fn check7c(&self, snap: &Snapshot) {
        let snap_ready_raw = (!snap.ready_raw.is_empty()).then(|| snap.ready_raw.as_str());
        let out = self.detect_unclaimable_ready(snap_ready_raw);
        let out = out.trim_end_matches('\n');
        if out.is_empty() {
            return;
        }
        self.h.print(out);
        let n = out.lines().filter(|l| l.starts_with("UNCLAIMABLE")).count();
        self.log(&format!("CHECK7c: {n} ready bead(s) no persona can claim — fix each by adding or removing the label named above"));
        self.act(&format!("surfaced {n} unclaimable ready bead(s)"));
        self.file_unclaimable_incidents(out);
    }

    /// CHECK 7d — a bead's recorded branch checked out in another bead's worktree. Native
    /// now (wave 4.28, sp-fbqsv): the bash seams S9/S10 are retired.
    pub fn check7d(&self) {
        let collisions = self.detect_branch_collisions();
        if collisions.is_empty() {
            return;
        }
        let text: Vec<String> = collisions.iter().map(crate::detect::Collision::line).collect();
        self.h.print(&text.join("\n"));
        let n_col = collisions.len() as i64;
        let outcomes = self.park_branch_collisions(&collisions);
        let park_lines: Vec<String> = outcomes.iter().map(ParkOutcome::line).collect();
        if !park_lines.is_empty() {
            self.h.print(&park_lines.join("\n"));
        }
        let freed = outcomes.iter().filter(|o| matches!(o, ParkOutcome::Freed { .. })).count() as i64;
        let unl = outcomes.iter().filter(|o| matches!(o, ParkOutcome::Unlabeled { .. })).count() as i64;
        let parked = n_col - freed - unl;
        if freed > 0 {
            self.log(&format!("CHECK7d: freed {freed} stale squatting worktree(s) whose owning bead is closed and clean"));
            self.act(&format!("freed {freed} branch-collision worktree(s)"));
        }
        if unl > 0 {
            self.log(&format!("CHECK7d: cut {unl} bead(s) off an inherited branch: label naming another bead's canonical branch"));
            self.act(&format!(
                "unlabeled {unl} inherited branch-collision bead(s)"
            ));
        }
        if parked > 0 {
            self.log(&format!(
                "CHECK7d: {parked} bead(s) whose recorded branch is held by another bead's worktree — parking with {}",
                self.cfg.ask
            ));
            self.act(&format!("parked {parked} branch-collision bead(s)"));
        }
    }

    /// CHECK 8 — judgement, rate limited; only when the plan is starved and nothing moved.
    pub fn check8(
        &self,
        plan_ready: Option<usize>,
        plan_inprog: usize,
        n_open: usize,
        open_plan: &[String],
    ) {
        let now = self.h.now();
        let cd = self.cfg.run.join("inference.cooldown");
        let last = std::fs::read_to_string(&cd)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        match should_judge(
            plan_ready,
            plan_inprog,
            n_open,
            self.progressed.get(),
            last,
            now,
            self.cfg.inference_every,
        ) {
            Judge::Cooldown => {
                self.log(&format!(
                    "starved, but inference is in cooldown ({}s left)",
                    self.cfg.inference_every - now + last
                ));
            }
            Judge::Yes => {
                let _ = std::fs::write(&cd, format!("{now}\n"));
                self.log(&format!(
                    "STARVED — {n_open} open, 0 ready, 0 running. Dropping to inference."
                ));
                let log = self.cfg.run.join("reflect.log");
                self.h.run(
                    Spec::args_owned(
                        self.script("reflect.sh").to_string_lossy().into_owned(),
                        vec![open_plan[..open_plan.len().min(REFLECT_IDS)].join("\n")],
                    )
                    .out(Io::Append(log.clone()))
                    .err(Io::Append(log)),
                );
                self.act("invoked reflection");
            }
            Judge::No => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn judgement_table() {
        // (plan_ready, plan_inprog, n_open, progressed, last, now, every) -> verdict
        assert_eq!(
            should_judge(Some(1), 0, 5, 0, 0, 10_000, 3600),
            Judge::No,
            "ready plan work means the DAG can move"
        );
        assert_eq!(should_judge(Some(0), 0, 5, 0, 0, 10_000, 3600), Judge::Yes);
        assert_eq!(
            should_judge(Some(0), 0, 5, 0, 9_000, 10_000, 3600),
            Judge::Cooldown
        );
        assert_eq!(
            should_judge(Some(0), 0, 5, 1, 0, 10_000, 3600),
            Judge::No,
            "progressed mutes judgement"
        );
        assert_eq!(
            should_judge(Some(0), 1, 5, 0, 0, 10_000, 3600),
            Judge::No,
            "something is running"
        );
        assert_eq!(
            should_judge(Some(0), 0, 0, 0, 0, 10_000, 3600),
            Judge::No,
            "nothing open"
        );
        assert_eq!(
            should_judge(None, 0, 5, 0, 0, 10_000, 3600),
            Judge::No,
            "unknown is not starved"
        );
    }

    fn repo(n: &str, q: bool) -> Repo {
        Repo {
            name: n.into(),
            root: Some(format!("/r/{n}")),
            landrefs: vec!["origin/main".into()],
            queued: q,
        }
    }

    #[test]
    fn sending_skip_needs_every_swept_repo_unchanged() {
        let all = vec![repo("a", false), repo("b", false), repo("q", true)];
        let swept: Vec<&Repo> = all.iter().filter(|r| !r.queued).collect();
        let cur = |r: &Repo| Some(format!("sha-{}", r.name));
        assert!(sending_skip("a=sha-a\nb=sha-b\n", &swept, &all, &cur));
        assert!(
            !sending_skip("a=sha-a\nb=old\n", &swept, &all, &cur),
            "a base moved"
        );
        assert!(
            !sending_skip("a=sha-a\n", &swept, &all, &cur),
            "b was never stamped"
        );
        assert!(
            !sending_skip("a=sha-a\nb=sha-b\nzz=1\n", &swept, &all, &cur),
            "an unresolvable stamped repo walks"
        );
        let none = |_: &Repo| None;
        assert!(!sending_skip("a=sha-a\nb=sha-b\n", &swept, &all, &none));
    }
}
