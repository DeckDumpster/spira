//! CHECK 8 — judgement, the full pass's last word.

use crate::host::{Io, Spec};
use crate::pass::Sentinel;

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

impl<'a> Sentinel<'a> {
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
}
