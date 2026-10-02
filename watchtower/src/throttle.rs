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
use crate::landstate;
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

/// Depth: CERTIFIED landstate records whose branch still exists AND whose tip is not yet an
/// ancestor of the land ref. Stale records (branch gone, or tip already merged) do not
/// count — an id with no live branch, or one already landed, is not queue depth.
pub fn compute_depth(landstate_dir: &Path, repo: Option<&str>, land_ref: Option<&str>) -> i64 {
    if !landstate_dir.is_dir() {
        return 0;
    }
    let mut depth = 0i64;
    for rec in landstate::read_dir(landstate_dir) {
        if rec.status != "CERTIFIED" {
            continue;
        }
        let refname = format!("refs/heads/spira/{}", rec.id);
        if !git::ref_exists(repo, &refname) {
            continue;
        }
        if let Some(lref) = land_ref {
            if !rec.tip.is_empty() && rec.tip != "none" && git::is_ancestor(repo, &rec.tip, lref) {
                continue;
            }
        }
        depth += 1;
    }
    depth
}

/// Minutes since the most recent LANDED record, or `None` ("?" — no LANDED record could be
/// read, treated as drain unknown = drain zero, never as "nothing to drain").
pub fn minutes_since_last_landed(landstate_dir: &Path, now: i64) -> Option<i64> {
    landstate::last_landed(landstate_dir).map(|(_, at)| (now - at) / 60)
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
pub fn run(now: i64, landstate_dir: &Path, cfg: &Cfg, ctx: &Ctx) {
    if ctx.override_off {
        let _ = std::fs::remove_file(ctx.stamp);
        log("watchtower: throttle-check — override=off, admission not throttled");
        return;
    }

    let depth = compute_depth(landstate_dir, ctx.repo, ctx.land_ref);
    let since_land = minutes_since_last_landed(landstate_dir, now);
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
                incident::file(inc, &f);
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
                    incident::file(ctx.incident_sh, &f);
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
                incident::file(ctx.incident_sh, &f);
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

    fn init_repo(dir: &Path) -> String {
        std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .arg(dir)
            .status()
            .unwrap();
        for (k, v) in [("user.email", "t@t"), ("user.name", "t")] {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["config", k, v])
                .status()
                .unwrap();
        }
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["commit", "-q", "--allow-empty", "-m", "init"])
            .status()
            .unwrap();
        String::from_utf8(
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string()
    }

    fn branch(dir: &Path, name: &str) {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["branch", name])
            .status()
            .unwrap();
    }

    #[test]
    fn depth_counts_a_live_unmerged_certified_branch() {
        let d = testkit::TempDir::new("wt-throttle-depth");
        let repo = d.join("git");
        std::fs::create_dir_all(&repo).unwrap();
        let base = init_repo(&repo);
        branch(&repo, "spira/sp-live");
        let ls = d.join("landstate");
        std::fs::create_dir_all(&ls).unwrap();
        std::fs::write(ls.join("sp-live"), format!("CERTIFIED {} 1700000000 x\n", base)).unwrap();
        let depth = compute_depth(&ls, Some(repo.to_str().unwrap()), Some("main"));
        // tip == base == HEAD of main, so is_ancestor is true -> excluded (already merged).
        assert_eq!(depth, 0);
    }

    #[test]
    fn depth_excludes_a_certified_record_with_no_live_branch() {
        let d = testkit::TempDir::new("wt-throttle-depth2");
        let repo = d.join("git");
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        let ls = d.join("landstate");
        std::fs::create_dir_all(&ls).unwrap();
        std::fs::write(ls.join("sp-gone"), "CERTIFIED deadbeef 1700000000 x\n").unwrap();
        let depth = compute_depth(&ls, Some(repo.to_str().unwrap()), Some("main"));
        assert_eq!(depth, 0);
    }

    #[test]
    fn depth_counts_a_certified_branch_whose_tip_is_not_yet_on_the_land_ref() {
        let d = testkit::TempDir::new("wt-throttle-depth3");
        let repo = d.join("git");
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        branch(&repo, "spira/sp-ahead");
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["checkout", "-q", "spira/sp-ahead"])
            .status()
            .unwrap();
        std::fs::write(repo.join("f.txt"), "x").unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["add", "f.txt"])
            .status()
            .unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["commit", "-q", "-m", "work"])
            .status()
            .unwrap();
        let ahead_tip = String::from_utf8(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["rev-parse", "spira/sp-ahead"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        let ls = d.join("landstate");
        std::fs::create_dir_all(&ls).unwrap();
        std::fs::write(
            ls.join("sp-ahead"),
            format!("CERTIFIED {} 1700000000 x\n", ahead_tip),
        )
        .unwrap();
        let depth = compute_depth(&ls, Some(repo.to_str().unwrap()), Some("main"));
        assert_eq!(depth, 1);
    }

    #[test]
    fn minutes_since_last_landed_is_none_with_no_landed_record() {
        let d = testkit::TempDir::new("wt-throttle-drain");
        let ls = d.join("landstate");
        std::fs::create_dir_all(&ls).unwrap();
        std::fs::write(ls.join("sp-a"), "CERTIFIED deadbeef 1700000000 x\n").unwrap();
        assert_eq!(minutes_since_last_landed(&ls, 1_700_003_600), None);
    }

    #[test]
    fn minutes_since_last_landed_picks_the_newest_landed_record() {
        let d = testkit::TempDir::new("wt-throttle-drain2");
        let ls = d.join("landstate");
        std::fs::create_dir_all(&ls).unwrap();
        std::fs::write(ls.join("sp-a"), "LANDED deadbeef 1700000000 x\n").unwrap();
        std::fs::write(ls.join("sp-b"), "LANDED deadbeef 1700003000 x\n").unwrap();
        let now = 1_700_003_600;
        assert_eq!(minutes_since_last_landed(&ls, now), Some((now - 1_700_003_000) / 60));
    }
}
