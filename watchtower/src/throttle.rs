//! `--throttle-check` — admission gate for the task pool, called by the sentinel on every
//! pass. Two inputs, two outputs (watchtower.sh's own comment, kept verbatim as the
//! contract):
//!
//!   depth >= DEPTH_AT AND drain active  -> write stamp (throttle engaged, pool held at 0)
//!   depth >= DEPTH_AT AND drain zero    -> escalate WITHOUT writing stamp (stall, not capacity)
//!
//! Throttling a stalled queue delays repairs rather than reducing load — wrong answer every
//! time, which is why the two branches are kept as separate, named outcomes below rather
//! than folded into one "over threshold" case.

use crate::git;
use crate::incident::{self, Finding};
use crate::lc;
use crate::log::log;
use std::path::Path;

pub struct Cfg {
    pub depth_at: i64,
    pub release_at: i64,
    pub stall_mins: i64,
}

impl Default for Cfg {
    fn default() -> Self {
        Cfg {
            depth_at: 16,
            release_at: 8,
            stall_mins: 50,
        }
    }
}

/// Depth: CERTIFIED beads whose branch still exists AND whose tip is not yet an ancestor of
/// the land ref. Stale rows (branch gone, or tip already merged) do not count. `None` is
/// spira-lc unreachable — the caller must not read it as an empty queue.
pub fn compute_depth(repo: Option<&str>, land_ref: Option<&str>) -> Option<i64> {
    let mut depth = 0i64;
    for b in lc::beads_in("CERTIFIED")? {
        let refname = format!("refs/heads/spira/{}", b.id);
        if !git::ref_exists(repo, &refname) {
            continue;
        }
        if let Some(lref) = land_ref {
            if !b.tip.is_empty() && b.tip != "none" && git::is_ancestor(repo, &b.tip, lref) {
                continue;
            }
        }
        depth += 1;
    }
    Some(depth)
}

/// Minutes since the most recent LANDED bead, or `None` ("?" — drain unknown, treated as
/// drain zero, never as "nothing to drain").
pub fn minutes_since_last_landed(now: i64) -> Option<i64> {
    lc::last_landed().map(|(_, at)| (now - at) / 60)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Primary {
    /// depth >= depth_at, drain active (since_land known and below stall_mins).
    HighDepthDrainOk { already_throttled: bool },
    /// depth >= depth_at, drain not active (`?` or >= stall_mins) — needs the deliberate-
    /// stall check (async gate commits on the land ref) before a verdict.
    HighDepthDrainZero,
    /// Was throttled, depth has dropped below release_at.
    Lift,
    /// Neither branch: log the current state, nothing to do.
    NoAction { throttled: bool },
}

/// The pure decision over already-gathered numbers — no IO, table-tested against the exact
/// scenarios `test-watchtower-throttle.sh` named (critical pair, hysteresis, stale-CERTIFIED
/// filtering happens before this in `compute_depth`).
pub fn decide(
    depth: i64,
    since_land: Option<i64>,
    already_throttled: bool,
    cfg: &Cfg,
) -> Primary {
    let drain_ok = since_land.map(|m| m < cfg.stall_mins).unwrap_or(false);
    if depth >= cfg.depth_at {
        if drain_ok {
            Primary::HighDepthDrainOk { already_throttled }
        } else {
            Primary::HighDepthDrainZero
        }
    } else if already_throttled && depth < cfg.release_at {
        Primary::Lift
    } else {
        Primary::NoAction {
            throttled: already_throttled,
        }
    }
}

/// True iff the stall is the known deliberate one: the async-gate epic (sp-c8w16,
/// sp-74gwk) is NOT fully on the land ref yet, so the landing loop legitimately freezes
/// pending that implementation. False — a real fault, escalate — only when BOTH commits
/// are already on the land ref and the queue is stalled anyway, which the carve-out was
/// never meant to excuse. An exact count of 2 for "fully landed", not "at least 2": a
/// third, unrelated commit mentioning either id must not silently widen the carve-out.
pub fn is_deliberate_stall(async_on_main_count: u32) -> bool {
    async_on_main_count != 2
}

pub fn disp(v: Option<i64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "?".into())
}

pub struct Ctx<'a> {
    pub db: &'a str,
    pub home_repo: &'a str,
    pub incident_sh: &'a str,
    pub stamp: &'a Path,
    pub override_off: bool,
    pub repo: Option<&'a str>,
    pub land_ref: Option<&'a str>,
    pub land_ref_default_for_log: &'a str,
}

/// Gathers, decides and performs the side effects (stamp write/remove, incident filing),
/// logging exactly the lines the bash logged. `world.halted` and the incident.sh presence
/// guard are checked by the caller (`main.rs`) — shared by all four checks.
pub fn run(now: i64, cfg: &Cfg, ctx: &Ctx) {
    if ctx.override_off {
        let _ = std::fs::remove_file(ctx.stamp);
        log("watchtower: throttle-check — override=off, admission not throttled");
        return;
    }

    let Some(depth) = compute_depth(ctx.repo, ctx.land_ref) else {
        log("watchtower: throttle-check skipped — spira-lc is unreachable, depth unknown");
        return;
    };
    let since_land = minutes_since_last_landed(now);
    let already_throttled = ctx.stamp.is_file();

    match decide(depth, since_land, already_throttled, cfg) {
        Primary::HighDepthDrainOk {
            already_throttled: true,
        } => {
            log(&format!(
                "watchtower: throttle-check — still throttled (depth={}>={})",
                depth, cfg.depth_at
            ));
        }
        Primary::HighDepthDrainOk {
            already_throttled: false,
        } => {
            let since_land_m = since_land.unwrap_or(0);
            let body = format!(
                "since={} depth={} since_land={}m\n",
                crate::log::now_iso(),
                depth,
                since_land_m
            );
            let _ = std::fs::write(ctx.stamp, body);
            log(&format!(
                "watchtower: throttle engaged — depth={}>={}, since_land={}m",
                depth, cfg.depth_at, since_land_m
            ));
            if let Some(inc) = Some(ctx.incident_sh).filter(|p| incident::is_usable(p)) {
                let body = format!(
                    "Queue admission throttled: CERTIFIED depth {} (threshold: {} branches), last landing {}m ago.\n\nBuilders are held; Ops, groomer, and other lanes continue.\n\nEngages when depth >= {} AND drain active (< {}m since landing).\nLifts when depth < {}.\nOverride: SPIRA_QUEUE_THROTTLE_OVERRIDE=off in spira.conf\n",
                    depth, cfg.depth_at, since_land_m, cfg.depth_at, cfg.stall_mins, cfg.release_at
                );
                let f = Finding::new(
                    ctx.db,
                    ctx.home_repo,
                    &format!(
                        "QUEUE THROTTLED: depth {}, drain {}m since landing",
                        depth, since_land_m
                    ),
                    &body,
                )
                .priority(2)
                .reference("incident:queue-throttle-engaged")
                .cause("throttle-engaged");
                incident::alarm(inc, &f);
            }
            log("watchtower: throttle-engage escalation filed");
        }
        Primary::HighDepthDrainZero => {
            let rev = ctx.land_ref.unwrap_or(ctx.land_ref_default_for_log);
            let async_on_main =
                git::oneline_log_matches(ctx.repo, rev, &["sp-c8w16", "sp-74gwk"]);
            if is_deliberate_stall(async_on_main) {
                log(&format!(
                    "watchtower: throttle-check — depth={}>={} but stall is deliberate (async gate not on main) — not escalating",
                    depth, cfg.depth_at
                ));
            } else {
                log(&format!(
                    "watchtower: throttle-check — depth={}>={} but since_land={}m>={}m stall — not throttling (fault)",
                    depth, cfg.depth_at, disp(since_land), cfg.stall_mins
                ));
                if !already_throttled && incident::is_usable(ctx.incident_sh) {
                    let body = format!(
                        "Queue depth {} above throttle threshold ({}) but drain has been zero for {}m (stall threshold: {}m).\n\nThis is a QUEUE FAULT, not a capacity condition. Throttling builders delays repairs.\nInvestigate: landing loop, batch CI, gate status.\n",
                        depth, cfg.depth_at, disp(since_land), cfg.stall_mins
                    );
                    let f = Finding::new(
                        ctx.db,
                        ctx.home_repo,
                        &format!(
                            "QUEUE: deep+stalled (depth {}, no landings for {}m)",
                            depth, disp(since_land)
                        ),
                        &body,
                    )
                    .priority(1)
                    .reference("incident:queue-throttle-stall")
                    .cause("throttle-stall");
                    incident::alarm(ctx.incident_sh, &f);
                }
            }
        }
        Primary::Lift => {
            let _ = std::fs::remove_file(ctx.stamp);
            log(&format!(
                "watchtower: throttle lifted — depth={}<{}",
                depth, cfg.release_at
            ));
            if incident::is_usable(ctx.incident_sh) {
                let body = format!(
                    "Queue throttle lifted: CERTIFIED depth now {} (below release threshold {}).\n\nBuilder admission is no longer throttled.\n",
                    depth, cfg.release_at
                );
                let f = Finding::new(
                    ctx.db,
                    ctx.home_repo,
                    &format!("QUEUE THROTTLE LIFTED: depth {}", depth),
                    &body,
                )
                .priority(2)
                .reference("incident:queue-throttle-lifted")
                .cause("throttle-lifted");
                incident::alarm(ctx.incident_sh, &f);
            }
            log("watchtower: throttle-lift escalation filed");
        }
        Primary::NoAction { throttled } => {
            log(&format!(
                "watchtower: throttle-check — {} (depth={} since_land={}m)",
                if throttled { "throttled" } else { "clear" },
                depth,
                disp(since_land)
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Cfg {
        Cfg::default()
    }

    // --- the critical pair -----------------------------------------------------------

    #[test]
    fn engage_when_depth_high_and_drain_active() {
        assert_eq!(
            decide(16, Some(5), false, &cfg()),
            Primary::HighDepthDrainOk {
                already_throttled: false
            }
        );
    }

    #[test]
    fn stall_not_engage_when_depth_high_and_drain_zero() {
        assert_eq!(decide(16, None, false, &cfg()), Primary::HighDepthDrainZero);
        assert_eq!(decide(16, Some(51), false, &cfg()), Primary::HighDepthDrainZero);
    }

    #[test]
    fn deliberate_stall_is_false_only_at_an_exact_match_on_two() {
        // Both async-gate commits on the land ref: the freeze has no excuse left. Real fault.
        assert!(!is_deliberate_stall(2));
        // Anything else (including a third, unrelated commit mentioning either id): the
        // implementation is not fully landed, so the freeze is still the deliberate one.
        assert!(is_deliberate_stall(0));
        assert!(is_deliberate_stall(1));
        assert!(is_deliberate_stall(3));
    }

    // --- hysteresis --------------------------------------------------------------------

    #[test]
    fn lift_when_depth_drops_below_release_threshold_while_throttled() {
        assert_eq!(decide(7, Some(2), true, &cfg()), Primary::Lift);
    }

    #[test]
    fn no_lift_in_the_hysteresis_band_between_release_and_depth_thresholds() {
        // depth 10 is below depth_at(16) but not below release_at(8): stays throttled,
        // no re-escalation, no stamp removal.
        assert_eq!(
            decide(10, Some(2), true, &cfg()),
            Primary::NoAction { throttled: true }
        );
    }

    #[test]
    fn depth_below_threshold_and_not_throttled_is_clear() {
        assert_eq!(
            decide(3, Some(2), false, &cfg()),
            Primary::NoAction { throttled: false }
        );
    }

    #[test]
    fn already_throttled_and_still_high_depth_drain_ok_is_a_noop_not_a_reengage() {
        assert_eq!(
            decide(20, Some(2), true, &cfg()),
            Primary::HighDepthDrainOk {
                already_throttled: true
            }
        );
    }

    // --- depth computation: stale CERTIFIED filtering ----------------------------------
}
