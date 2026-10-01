//! CHECK 7's inputs, and the --summon-only fast path. The summon loop itself (lanes, pool,
//! fleet ceiling, express grant, elastic reservations) — lib.sh's `ck7_summon_pass`/
//! `_ck7_summon_body`/`summon_fayth`/`world_gate`/`summon_argv` — is ported in-process here
//! (wave 4.27, family G, sp-gzmd2). `summon.lock` is the one thing still shared with `aeon
//! --escape` (G5; escape.sh before it, retired sp-zpaq0) across the process boundary — a
//! real OS flock, so two binaries contending for it still serialize correctly.

use crate::cfg::Fayth;
use crate::cfg::Lifecycle;
use crate::host::{Io, Spec};
use crate::model::{Bead, LcRow};
use crate::pass::{self, Sentinel};
use crate::store::{self, has_all, has_none};

/// ready-bucket.py: a bead counts for a fayth iff its labels ⊇ FAYTH_LABELS, are disjoint
/// from FAYTH_EXCLUDE_LABELS and from the shared queue-wait/submitted exclusion, and — when
/// it carries `fayth:<name>` — this fayth is among the named preferences. Fayths with no
/// labels are not rows (bulk_ready_by_fayth skips them).
pub fn bucket(ready: &[Bead], fayths: &[Fayth], shared_exclude: &[String]) -> Vec<(String, usize)> {
    let parts: Vec<&Fayth> = fayths.iter().filter(|f| !f.labels.is_empty()).collect();
    let mut counts: Vec<(String, usize)> = parts.iter().map(|f| (f.name.clone(), 0)).collect();
    for b in ready {
        if !has_none(b, shared_exclude) {
            continue;
        }
        let pref: Vec<&str> = b
            .labels
            .iter()
            .filter_map(|l| l.strip_prefix("fayth:"))
            .collect();
        for (i, f) in parts.iter().enumerate() {
            if !pref.is_empty() && !pref.contains(&f.name.as_str()) {
                continue;
            }
            if has_all(b, &f.labels) && has_none(b, &f.exclude) {
                counts[i].1 += 1;
            }
        }
    }
    counts
}

/// lifecycle_enforce ON: the ready beads a claim could actually take. A bead the lifecycle
/// machine holds on anything but `wait` (a poison, ask or operator hold) is refused by
/// spira-claim's own claim rule (rank.rs `claimable`: `Held`), so CHECK 7 must not count it
/// as ready — else every pass summons an aeon for a poisoned bead that no claim can take.
/// A bead with no lifecycle row is left in (unmigrated; the claim itself decides).
pub fn unheld(ready: &[Bead], rows: &[LcRow]) -> Vec<Bead> {
    let held: std::collections::HashSet<&str> = rows
        .iter()
        .filter(|r| r.holds.iter().any(|h| h != "wait"))
        .map(|r| r.bead_id.as_str())
        .collect();
    ready.iter().filter(|b| !held.contains(b.id.as_str())).cloned().collect()
}

pub fn render_cache(counts: &[(String, usize)]) -> String {
    counts.iter().fold(String::new(), |mut s, (f, n)| {
        use std::fmt::Write;
        let _ = writeln!(s, "{f} {n}");
        s
    })
}

impl<'a> Sentinel<'a> {
    fn shared_exclude(&self) -> Vec<String> {
        [&self.cfg.queue_wait, &self.cfg.submitted]
            .into_iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect()
    }

    /// ready_cache_populate: never hand back an empty-but-existing file, because fayth_ready
    /// trusts the cache unconditionally once it exists.
    ///
    /// ON: held beads are not ready (see [`unheld`]); a machine that cannot answer counts
    /// nothing ready this pass — it is authoritative, and no claim can succeed without it.
    pub fn export_ready_cache(&self, ready: &[Bead]) {
        let ready: Vec<Bead> = match self.lc {
            Lifecycle::Off => ready.to_vec(),
            Lifecycle::On => match self.lc_rows() {
                Some(rows) => unheld(ready, &rows),
                None => Vec::new(),
            },
        };
        let counts = bucket(&ready, &self.ctx.fayths, &self.shared_exclude());
        if counts.is_empty() && !self.ctx.fayths.is_empty() {
            return;
        }
        if let Some(p) = self.temp_file("ready-cache", &render_cache(&counts)) {
            self.h.set_env("SPIRA_READY_CACHE", &p.to_string_lossy());
        }
    }

    /// --summon-only: the gate, the live count, ONE bd ready, then CHECK 7.
    pub fn summon_only(&self) -> i32 {
        // S1 — the world (halt/drain), checked once here as a fast exit ahead of every
        // other cost `summon_fayth` would otherwise pay per fayth; each `summon_fayth`
        // call below checks it again on its own (wave 4.27 — `world_gate` is in-process
        // now, so asking twice costs nothing like the bash seam call it replaced did).
        if !self.world_gate("fleet", "summon-only") {
            return 0;
        }
        // Capacity (family K, wave 4.26): a pure read, in-process — sentinel never probes
        // or mutates the pause file, so aeon stays the probe's one owner
        // (wave4-decomposition.md (c)3).
        match aeon::capacity::pause_state(&self.cfg.capacity_pause) {
            aeon::capacity::PauseState::Open => {}
            aeon::capacity::PauseState::Paused { until, .. } => {
                let left = until - self.h.now();
                self.log(&format!("summon-only: account out of capacity for another {left}s — not summoning"));
                return 0;
            }
            aeon::capacity::PauseState::Unknown => {
                self.log("summon-only: the capacity pause file could not be read — not summoning (failing closed)");
                return 0;
            }
        }
        let live = self.live_total();
        self.log(&format!(
            "summon-only: live={live} fayths=[{}]",
            self.cfg.fayths_str
        ));
        if let Ok((_, ready)) = store::read_json(&self.bd(), self.h, &store::ready_args(&self.cfg))
        {
            self.export_ready_cache(&ready);
        }
        self.ck7_summon_pass();
        crate::temps::cleanup();
        self.h.unset_env("SPIRA_READY_CACHE");
        self.log(&format!(
            "summon-only pass complete — {} action(s)",
            self.acted.get()
        ));
        0
    }
}

/// `summon_fayth`'s one answer per call: whether it summoned, and the ready count this
/// call learned or reused — the caller's own cache for its NEXT call (lib.sh's
/// `SUMMON_FAYTH_CACHED_READY`, a shell variable surviving across calls in one process;
/// here just a value the fill loop threads through itself). `None` means the ready query
/// itself could not be answered this call — never cached as a zero.
pub struct Attempt {
    pub summoned: bool,
    pub ready: Option<i64>,
}

/// `fayth_ready <fayth>`'s three outcomes (spira-claim `fayth-ready`'s own rc contract,
/// sp-3ntca): a real count (zero included), no such fayth in the chamber, or the query
/// itself failed (bd unreachable, say) — the last two must refuse the summon WITHOUT
/// caching a zero, or a transient failure reads as "nothing ready" for the rest of the pass.
enum ReadyAnswer {
    Count(i64),
    NoFayth,
    Failed(String),
}

/// check7_pool_decision <throttled> <free> <express-ready> -> the task pool CHECK 7 grants
/// this pass (lib.sh:474). An express grant sets the pool to EXACTLY 1, never whatever
/// `free` already held (sp-zcvh1's own fix).
pub fn check7_pool_decision(throttled: bool, free: i64, express_ready: bool) -> i64 {
    if throttled {
        i64::from(express_ready)
    } else {
        free
    }
}

/// lane_rotate <last> <lanes> -> this pass's lane evaluation order (lib.sh:489): `last`
/// (and everything before it) rotates to the end, so the lane that won last pass draws
/// last this pass and the collective cap cannot be monopolised by whichever lane happens
/// to sort first. `last` absent from `lanes` (or empty) leaves the order unchanged.
pub fn lane_rotate(last: &str, lanes: &[String]) -> Vec<String> {
    if last.is_empty() || lanes.is_empty() {
        return lanes.to_vec();
    }
    match lanes.iter().position(|l| l == last) {
        Some(i) => lanes[i + 1..].iter().chain(lanes[..=i].iter()).cloned().collect(),
        None => lanes.to_vec(),
    }
}

/// ck7_pool <max-aeons> <task-live> -> the task pool's starting size, floored at 0
/// (lib.sh:517). `None` when no pool is configured — every persona's own concurrency cap
/// applies unchanged, exactly as before this pool existed.
pub fn ck7_pool(max_aeons: Option<i64>, task_live: i64) -> Option<i64> {
    max_aeons.map(|m| (m - task_live).max(0))
}

/// ck7_throttled <stamp-exists> <override> -> true when the admission throttle stamp holds
/// the task pool at 0 this pass (lib.sh:526). `SPIRA_QUEUE_THROTTLE_OVERRIDE=off` pins the
/// pass unthrottled regardless of the stamp.
pub fn ck7_throttled(stamp_exists: bool, override_: &str) -> bool {
    stamp_exists && override_ != "off"
}

/// ck7_fill_cap <fill> <pool> <max-live-aeons> -> true ("stop") once the pool (if bounded)
/// is spent or `fill` has reached the per-persona-per-pass cap (lib.sh:536, default 4 —
/// the caller resolves `SPIRA_MAX_LIVE_AEONS` with that default before calling this).
pub fn ck7_fill_cap(fill: i64, pool: Option<i64>, max_live_aeons: i64) -> bool {
    if let Some(p) = pool {
        if p <= 0 {
            return true;
        }
    }
    fill >= max_live_aeons
}

/// `command -v <name>` strictly: `None` when nothing executable is found, never a bare-name
/// fallback — the one caller (`summon_fayth`'s own `aeon` resolution) must refuse rather
/// than hand systemd-run a name it will never find either.
fn which(name: &str, path: &str) -> Option<String> {
    let p = pass::on_path(name, path);
    p.contains('/').then_some(p)
}

impl<'a> Sentinel<'a> {
    /// world_gate <fayth> <log-prefix> -> true if summons are permitted, false if halted or
    /// draining (lib.sh:1102). HALTED is indefinite, checked first; DRAINING expires — a
    /// stamp with no `expires` line of its own expires at its own mtime + `drain_ttl`, so a
    /// drain nobody resumed cannot wedge the loop forever. Lifting an expired drain is
    /// LOUD, never silent: a quiet lift would hide the forgotten resume, which is the
    /// defect worth seeing.
    pub fn world_gate(&self, f: &str, prefix: &str) -> bool {
        if self.cfg.run.join("world.halted").is_file() {
            self.log(&format!("{prefix} {f}: halted — not summoning (world.sh start to lift)"));
            return false;
        }
        let dstamp = self.cfg.run.join("world.draining");
        let Ok(meta) = std::fs::metadata(&dstamp) else {
            return true;
        };
        let text = std::fs::read_to_string(&dstamp).unwrap_or_default();
        let declared = text
            .lines()
            .find_map(|l| l.strip_prefix("expires "))
            .and_then(|v| v.trim().parse::<i64>().ok());
        let exp = declared.unwrap_or_else(|| {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            mtime + self.cfg.drain_ttl
        });
        if self.h.now() >= exp {
            let _ = std::fs::remove_file(&dstamp);
            self.log(&format!(
                "{prefix} {f}: DRAIN EXPIRED — lifting a drain nobody resumed (deadline {}, TTL {}s). Whoever drained did not resume; summons are live again.",
                crate::host::utc(exp),
                self.cfg.drain_ttl
            ));
            true
        } else {
            self.log(&format!("{prefix} {f}: draining — not summoning (world.sh resume to lift)"));
            false
        }
    }

    /// `sentinel --world-gate <fayth> <prefix>`: lib.sh's `world_gate` shim target, for
    /// `aeon --escape`'s own seam (`_aeon_world_gate`) and any bash caller that still
    /// sources lib.sh directly.
    pub fn world_gate_cmd(&self, f: &str, prefix: &str) -> i32 {
        i32::from(!self.world_gate(f, prefix))
    }

    /// A live unit/pidfile count for one name — the mechanism `live_total` already uses
    /// for the fleet ceiling, factored out so `aeon_count`/`lanes_live_total` share it
    /// rather than keeping a third copy (lib.sh family E's own Rust port, `strand::probe`,
    /// does the same thing for its own caller; this crate's copy stays `Host`-driven so
    /// its own fixtures can fake it exactly as `live_total`'s already do).
    fn unit_count(&self, name: &str, exclude: Option<&str>) -> i64 {
        if self.cfg.summon == "systemd-run" {
            let prefix = format!("spira-aeon-{name}-");
            let o = self.h.run(
                Spec::args_owned(
                    self.cfg.systemctl.clone(),
                    vec![
                        "--user".into(),
                        "list-units".into(),
                        format!("{prefix}*"),
                        "--no-legend".into(),
                    ],
                )
                .err(Io::Null),
            );
            // Filtered again here, client-side, exactly as `live_total` already does: a
            // real systemctl's own glob already narrows this, but a fixture that answers
            // every glob with the same fixed unit list (several already do) must not
            // over-count — "spira-aeon-opsx-3" must never count toward "ops".
            return o
                .stdout
                .lines()
                .filter_map(|l| l.split_whitespace().next())
                .filter(|u| u.starts_with(&prefix))
                .filter(|u| exclude.map_or(true, |ex| *u != ex))
                .count() as i64;
        }
        pass::pid_count(&self.cfg.run, name) as i64
    }

    /// aeon_count <fayth> [exclude-unit] -> live aeons of one persona (lib.sh:238, already
    /// a shim onto `strand aeon-count`; this is the same question asked in-process by the
    /// one caller that needs it inside this pass, `fayth_free`).
    fn aeon_count(&self, fayth: &str, exclude: Option<&str>) -> i64 {
        self.unit_count(fayth, exclude)
    }

    /// aeons_live_lanes, restricted to the lane fayths already known to the caller (so it
    /// is asked once per lane, in the SAME roster the CK7 loop itself computed) — `FAYTH_NAME`
    /// is resolved per lane fayth, exactly as the bash original, so a lane free to declare a
    /// different unit/pidfile name than its chamber filename is still counted correctly.
    fn lanes_live_total(&self, lanes: &[String]) -> i64 {
        lanes
            .iter()
            .map(|f| {
                let name = spira_config::chamber::fayth_get(&self.cfg.home, f, "FAYTH_NAME", f);
                self.unit_count(&name, None)
            })
            .sum()
    }

    /// fayth_free <fayth> [pool-remaining] [exclude-unit] -> how many more of this persona
    /// may be summoned right now (lib.sh:1003). THIS STAYS HOST-DRIVEN RATHER THAN A ONE-LINE
    /// SHIM for the same reason lib.sh's own copy stays bash: callers need to drive
    /// `aeon_count`'s answer directly in a fixture without a real process table.
    fn fayth_free(&self, fayth: &str, pool: Option<i64>, exclude: Option<&str>) -> i64 {
        let home = &self.cfg.home;
        let mut max: i64 = spira_config::chamber::fayth_get(home, fayth, "FAYTH_MAX_CONCURRENT", "1")
            .trim()
            .parse()
            .unwrap_or(1);
        let elastic = spira_config::chamber::fayth_get(home, fayth, "FAYTH_ELASTIC", "0") == "1";
        let is_remainder = elastic && pool.is_some();
        if is_remainder {
            max = pool.unwrap();
        }
        let have = self.aeon_count(fayth, exclude);
        let mut free = if is_remainder { max } else { (max - have).max(0) };
        if let Some(p) = pool {
            if p < free {
                free = p;
            }
        }
        free
    }

    /// `spira_task_fayths`/`spira_lane_fayths` (spira-config's own chamber functions),
    /// applied to THIS pass's already-resolved roster (`ctx.fayth_names()`, which the
    /// probe built via `spira_fayths()` — already `SPIRA_FAYTHS`-correct and in priority
    /// order) rather than re-deriving the roster a second time: the same three predicates
    /// (`FAYTH_SUMMON=auto`, not a party member, `FAYTH_LANE` set or not) applied per name
    /// in roster order produce the identical split, with no need to thread `SPIRA_FAYTHS`
    /// itself from bash into this process a second way.
    fn task_and_lane_fayths(&self) -> (Vec<String>, Vec<String>) {
        let home = &self.cfg.home;
        let mut task = Vec::new();
        let mut lane = Vec::new();
        for f in self.ctx.fayth_names() {
            if spira_config::chamber::fayth_get(home, &f, "FAYTH_SUMMON", "auto") != "auto" {
                continue;
            }
            if spira_config::chamber::fayth_get(home, &f, "FAYTH_ROLE", "task") == "party" {
                continue;
            }
            if spira_config::chamber::fayth_get(home, &f, "FAYTH_LANE", "").is_empty() {
                task.push(f);
            } else {
                lane.push(f);
            }
        }
        (task, lane)
    }

    /// fayth_ready <fayth> -> spira-claim's own answer, in-process via the binary on PATH
    /// (`_spira_claim fayth-ready`, lib.sh:454 — kept external: spira-claim's own lib.rs
    /// deliberately exposes no more than `READY_ARGS_BASE` for another crate to link).
    fn fayth_ready(&self, f: &str) -> ReadyAnswer {
        let bin = self.cfg.claim_bin.clone();
        let o = self.h.run(Spec::args_owned(bin, vec!["fayth-ready".into(), f.to_string()]));
        match o.rc {
            0 => ReadyAnswer::Count(o.stdout.trim().parse().unwrap_or(0)),
            2 => ReadyAnswer::NoFayth,
            _ => ReadyAnswer::Failed(o.stderr.trim().to_string()),
        }
    }

    /// express_ready_in_task_pool <task-fayths> <express-label> -> true when some task
    /// fayth has a bead ready under its OWN partition plus the express label (lib.sh's
    /// `express_ready_in_task_pool`, fbddd3e2b). Composes with `FAYTH_LABELS` rather than
    /// bypassing them — exactly `ready-count "<FAYTH_LABELS>,<express-label>"
    /// "<fayth-exclude f FAYTH_EXCLUDE_LABELS>"`, per task fayth, stopping at the first hit.
    ///
    /// DELETED OUTRIGHT, NOT MISSING A CALLER: wave 4.25 (sp-obhv6) deleted this function
    /// as "no live callers" while `_ck7_summon_body` (lib.sh) still called it by name —
    /// the call failed silently every throttled pass from then on (an undefined bash
    /// function is "command not found", never a bug visible in a diff), so "express ready"
    /// read false forever and the bypass never fired again. Wave 4.27 (sp-gzmd2) ported
    /// `_ck7_summon_body` faithfully, which means it ported that silent `false` too — this
    /// restores the actual predicate the bash intended, not the broken state it had drifted
    /// into (sp-yh7yx).
    ///
    /// A fayth with no `FAYTH_LABELS` is skipped, same as the bash guard
    /// (`[ -n "${FAYTH_LABELS:-}" ] || exit 1`) — never counted as ready.
    fn express_ready_in_task_pool(&self, task_fayths: &[String], express_label: &str) -> bool {
        let home = &self.cfg.home;
        let bin = self.cfg.claim_bin.clone();
        for f in task_fayths {
            let labels = spira_config::chamber::fayth_get(home, f, "FAYTH_LABELS", "");
            if labels.is_empty() {
                continue;
            }
            let own = spira_config::chamber::fayth_get(home, f, "FAYTH_EXCLUDE_LABELS", "");
            let ex = self
                .h
                .run(Spec::args_owned(bin.clone(), vec!["fayth-exclude".into(), f.clone(), own]));
            let exclude = ex.stdout.trim().to_string();
            let combined = format!("{labels},{express_label}");
            let rc = self
                .h
                .run(Spec::args_owned(bin.clone(), vec!["ready-count".into(), combined, exclude]));
            let n: i64 = rc.stdout.trim().parse().unwrap_or(0);
            if n > 0 {
                return true;
            }
        }
        false
    }

    /// summon_refill_argv -> the ExecStopPost property that refills this slot the instant
    /// the aeon it is attached to exits (lib.sh:1141; sp-0y2av). The resolved path, not the
    /// bare name: systemd validates an ExecStopPost command line itself and refuses a bare
    /// "systemd-run" as "not an absolute path".
    fn summon_refill_argv(&self) -> String {
        let path = self.cfg.raw("PATH");
        let summon_bin = pass::on_path(&self.cfg.summon, path);
        let sentinel_bin = pass::on_path("sentinel", path);
        format!("--property=ExecStopPost={summon_bin} --user --collect --quiet {sentinel_bin} --summon-only")
    }

    /// summon_argv <fayth> -> the systemd-run property/setenv flags shared by every summon
    /// path (lib.sh:1150: `summon_fayth`, `aeon --escape`), one argv token per element.
    pub fn summon_argv(&self, f: &str) -> Vec<String> {
        let timeout = spira_config::chamber::fayth_get(&self.cfg.home, f, "FAYTH_TIMEOUT_SECONDS", "3600");
        let release = self.cfg.raw("SPIRA_RELEASE");
        vec![
            format!("--property=TimeoutStartSec={timeout}"),
            self.summon_refill_argv(),
            format!("--setenv=SPIRA_RELEASE={release}"),
            format!("--setenv=PATH={release}/bin:{release}/spira:/usr/local/bin:/usr/bin:/bin"),
            format!("--setenv=HOME={}", self.cfg.raw("HOME")),
        ]
    }

    /// `sentinel --summon-argv <fayth>`: lib.sh's `summon_argv` shim target, for `aeon
    /// --escape`'s own seam (`_aeon_summon_argv`).
    pub fn summon_argv_cmd(&self, f: &str) -> i32 {
        for line in self.summon_argv(f) {
            self.h.print(&line);
        }
        0
    }

    /// summon_fayth <fayth> [pool-remaining] [require-label] [reuse-ready] -> lib.sh:1160,
    /// the whole refusal ladder: world gate, the account's own capacity, the fleet ceiling
    /// (above every per-persona cap and the pool — SPIRA_MAX_LIVE_AEONS unset means no
    /// ceiling, today's behaviour exactly), the elastic last-slot and inverted lane-last-
    /// slot reservations, this persona's own readiness and concurrency cap, then the
    /// summon itself. **Safety-critical** (wave4-decomposition.md (c)8) — every refusal
    /// keeps its own log line, because the pass's stdout IS the sentinel log.
    pub fn summon_fayth(&self, f: &str, pool: Option<i64>, require_label: &str, reuse_ready: Option<i64>) -> Attempt {
        if !self.world_gate(f, "CHECK7") {
            return Attempt { summoned: false, ready: reuse_ready };
        }
        // THE ACCOUNT BEFORE THE QUEUE, and a pure in-process read — never a probe (wave
        // 4.26 moved the one probe owner to `aeon`; sentinel reading "cannot tell" as paused
        // is the fail-closed fix that same wave made, kept exactly by this port).
        match aeon::capacity::pause_state(&self.cfg.capacity_pause) {
            aeon::capacity::PauseState::Open => {}
            aeon::capacity::PauseState::Paused { until, .. } => {
                let left = until - self.h.now();
                self.log(&format!("CHECK7 {f}: the account is out of capacity for another {left}s — not summoning"));
                return Attempt { summoned: false, ready: reuse_ready };
            }
            aeon::capacity::PauseState::Unknown => {
                self.log(&format!("CHECK7 {f}: the account is out of capacity for another ?s — not summoning"));
                return Attempt { summoned: false, ready: reuse_ready };
            }
        }
        if let Some(max_live) = self.cfg.max_live_aeons {
            let live_all = self.live_total() as i64;
            if live_all >= max_live {
                self.log(&format!("CHECK7 {f}: {live_all}/{max_live} aeon(s) live across the whole fleet — not summoning"));
                return Attempt { summoned: false, ready: reuse_ready };
            }
            let home = &self.cfg.home;
            if spira_config::chamber::fayth_get(home, f, "FAYTH_ELASTIC", "0") == "1" {
                let slots_free = max_live - live_all;
                if slots_free == 1 {
                    let (task_fayths, _) = self.task_and_lane_fayths();
                    for nef in &task_fayths {
                        if spira_config::chamber::fayth_get(home, nef, "FAYTH_ELASTIC", "0") == "1" {
                            continue;
                        }
                        if let ReadyAnswer::Count(n) = self.fayth_ready(nef) {
                            if n > 0 {
                                self.log(&format!("CHECK7 {f}: 1 fleet slot remaining, held back — {nef} has {n} ready bead(s)"));
                                return Attempt { summoned: false, ready: reuse_ready };
                            }
                        }
                    }
                }
            }
            if let Some(lanes_max) = self.cfg.lanes_max_live {
                let is_lane = !spira_config::chamber::fayth_get(home, f, "FAYTH_LANE", "").is_empty();
                if is_lane {
                    let (_, lane_fayths) = self.task_and_lane_fayths();
                    let live_lanes = self.lanes_live_total(&lane_fayths);
                    if live_lanes >= lanes_max {
                        self.log(&format!("CHECK7 {f}: {live_lanes}/{lanes_max} lane slot(s) in use — not summoning"));
                        return Attempt { summoned: false, ready: reuse_ready };
                    }
                } else {
                    let inv_slots_free = max_live - live_all;
                    if inv_slots_free == 1 {
                        let (_, lane_fayths) = self.task_and_lane_fayths();
                        let live_lanes = self.lanes_live_total(&lane_fayths);
                        if live_lanes < lanes_max {
                            for lf in &lane_fayths {
                                if let ReadyAnswer::Count(lf_r) = self.fayth_ready(lf) {
                                    if lf_r > 0 {
                                        let lf_free = self.fayth_free(lf, None, None);
                                        if lf_free > 0 {
                                            self.log(&format!("CHECK7 {f}: 1 fleet slot remaining, held back — {lf} has {lf_r} ready lane work"));
                                            return Attempt { summoned: false, ready: reuse_ready };
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        let ready = match reuse_ready {
            Some(r) => r,
            None => match self.fayth_ready(f) {
                ReadyAnswer::NoFayth => {
                    self.log(&format!("CHECK7 {f}: no fayth in the chamber — skipped"));
                    return Attempt { summoned: false, ready: None };
                }
                ReadyAnswer::Failed(msg) => {
                    let m = if msg.is_empty() { "bd gave no reason".to_string() } else { msg };
                    self.log(&format!("CHECK7 {f}: ready query failed: {m} — skipped, not counted as zero ready"));
                    return Attempt { summoned: false, ready: None };
                }
                ReadyAnswer::Count(n) => n,
            },
        };
        if ready == 0 {
            self.log(&format!("CHECK7 {f}: nothing ready in its partition"));
            return Attempt { summoned: false, ready: Some(0) };
        }
        let free = self.fayth_free(f, pool, None);
        if free == 0 {
            self.log(&format!("CHECK7 {f}: {ready} ready, at concurrency cap"));
            return Attempt { summoned: false, ready: Some(ready) };
        }
        let restricted = if require_label.is_empty() {
            String::new()
        } else {
            format!(", restricted to '{require_label}'")
        };
        self.log(&format!("CHECK7 {f}: {ready} ready, {free} free — summoning{restricted}"));
        // systemd-run is handed the PATH-resolved aeon: a transient unit has no launcher PATH.
        let Some(aeon_bin) = which("aeon", self.cfg.raw("PATH")) else {
            self.log(&format!("CHECK7 {f}: aeon not found on PATH — not summoning"));
            return Attempt { summoned: false, ready: Some(ready) };
        };
        let mut args: Vec<String> = vec![
            "--user".into(),
            "--collect".into(),
            "--quiet".into(),
            format!("--unit=spira-aeon-{f}-{}", self.h.now()),
        ];
        args.extend(self.summon_argv(f));
        if !require_label.is_empty() {
            args.push(format!("--setenv=SPIRA_REQUIRE_LABEL={require_label}"));
        }
        args.push(aeon_bin);
        args.push("--home".into());
        args.push(self.cfg.home.to_string_lossy().into_owned());
        args.push(f.to_string());
        let o = self.h.run(
            Spec::args_owned(self.cfg.summon.clone(), args)
                .out(Io::Inherit)
                .err(Io::Null),
        );
        if o.ok() {
            Attempt { summoned: true, ready: Some((ready - 1).max(0)) }
        } else {
            Attempt { summoned: false, ready: Some(ready) }
        }
    }

    /// `sentinel --summon <fayth> [pool] [require-label]`: lib.sh's `summon_fayth` shim
    /// target — czar-pass's own summon call (no lib.sh sourcing at all now) and any bash
    /// caller that still sources lib.sh directly. The fourth, `reuse-ready`, argument is
    /// dropped: its only caller was `_ck7_summon_body`'s own fill loop, fully ported below,
    /// and no external caller ever passed it (grepped the whole tree).
    pub fn summon_cmd(&self, f: &str, pool: Option<i64>, require_label: &str) -> i32 {
        i32::from(!self.summon_fayth(f, pool, require_label, None).summoned)
    }

    /// _ck7_summon_body -> CHECK 7's lane-then-pool summon loop, unlocked (lib.sh:1314).
    /// Never reachable on its own any more: the one bash caller that used to call it bare,
    /// to prove the race `ck7_summon_pass`'s flock prevents, called a shell function that
    /// could stub `aeon_count`/`capacity_paused`/`world_gate` by name — a technique a
    /// compiled binary cannot offer. The race is now structurally impossible instead:
    /// there is no entry point to this loop that does not hold `summon.lock` first
    /// (`ck7_summon_pass`, below); the lock itself is proven by `summon::tests::
    /// the_lock_actually_serializes_two_contenders`.
    fn ck7_summon_body(&self) {
        let (task_fayths, lane_fayths) = self.task_and_lane_fayths();
        // task_live is asked only when a pool is configured — exactly `[ -n
        // "${SPIRA_MAX_AEONS:-}" ]`'s own guard, never a cost paid on a host with no pool.
        let mut pool: Option<i64> = None;
        if let Some(max_aeons) = self.cfg.max_aeons {
            let task_live: i64 = task_fayths.iter().map(|f| self.aeon_count(f, None)).sum();
            let p = ck7_pool(Some(max_aeons), task_live).unwrap();
            self.log(&format!(
                "CHECK7 pool: {max_aeons} slot(s), {task_live} live, {p} free — order: {}",
                task_fayths.join(" ")
            ));
            pool = Some(p);
        }
        if !lane_fayths.is_empty() {
            let declared = self.ctx.get("SPIRA_LANES").filter(|s| !s.is_empty()).unwrap_or("none");
            self.log(&format!("CHECK7 lanes ({declared} declared): {}", lane_fayths.join(" ")));
        }
        let last = std::fs::read_to_string(&self.cfg.lane_round_robin).unwrap_or_default();
        let rotated_lanes = lane_rotate(last.trim(), &lane_fayths);
        let start = self.h.now();
        let budget = self.cfg.pass_budget_secs;
        for f in &rotated_lanes {
            if self.h.now() - start >= budget {
                self.log(&format!("CHECK7 {f}: not evaluated (pass budget exhausted)"));
                continue;
            }
            if self.summon_fayth(f, None, "", None).summoned {
                self.act(&format!("summoned a {f} lane aeon"));
                let _ = std::fs::write(&self.cfg.lane_round_robin, f);
            }
        }
        // ADMISSION THROTTLE: the stamp holds the task pool at 0 unless the override pins
        // it clear, UNLESS an express bead is ready in the task pool (sp-yh7yx restores
        // this: wave 4.25/sp-obhv6 deleted `express_ready_in_task_pool` while this loop's
        // bash original still called it, so the bypass silently never fired from then on,
        // and wave 4.27/sp-gzmd2 ported that broken state faithfully). An express grant
        // sets the pool to EXACTLY 1 (`check7_pool_decision`) and restricts the summon to
        // the express label — every OTHER gate in `summon_fayth` (world halted/draining,
        // account capacity, the fleet ceiling) still runs on that one slot unchanged.
        let mut express_label = String::new();
        if ck7_throttled(self.cfg.throttle_stamp.is_file(), &self.cfg.queue_throttle_override) {
            let express_ready = self.express_ready_in_task_pool(&task_fayths, &self.cfg.express_label);
            pool = Some(check7_pool_decision(true, pool.unwrap_or(0), express_ready));
            let head = std::fs::read_to_string(&self.cfg.throttle_stamp)
                .ok()
                .and_then(|s| s.lines().next().map(str::to_string))
                .unwrap_or_default();
            if express_ready {
                express_label = self.cfg.express_label.clone();
                self.log(&format!(
                    "CHECK7 pool: throttle active — express bead ready, granting pool={} (restricted to '{express_label}')",
                    pool.unwrap_or(0)
                ));
            } else {
                self.log(&format!(
                    "CHECK7 pool: throttle active ({head}) — task pool held at {}",
                    pool.unwrap_or(0)
                ));
            }
        }
        for f in &task_fayths {
            if self.h.now() - start >= budget {
                self.log(&format!("CHECK7 {f}: not evaluated (pass budget exhausted)"));
                continue;
            }
            let mut fill: i64 = 0;
            let mut reuse: Option<i64> = None;
            loop {
                let attempt = self.summon_fayth(f, pool, &express_label, reuse);
                reuse = attempt.ready;
                if !attempt.summoned {
                    break;
                }
                self.act(&format!("summoned a {f} aeon"));
                if let Some(p) = pool {
                    pool = Some((p - 1).max(0));
                }
                fill += 1;
                if ck7_fill_cap(fill, pool, self.cfg.max_live_aeons.unwrap_or(4)) {
                    break;
                }
                if self.h.now() - start >= budget {
                    break;
                }
            }
        }
    }

    /// Opens (creating if needed) and flock-waits on `summon.lock`, up to
    /// `SPIRA_SUMMON_LOCK_WAIT` seconds (default 30) — `flock -w N`'s own semantics,
    /// polled rather than signal-driven since libc's `flock(2)` has no built-in timeout.
    /// `None` on a timeout or an unopenable lockfile; the guard itself IS the hold, same
    /// as `archivist::lock`/`queue::lock`'s own flock guards — released the moment this
    /// process drops it, however it exits.
    fn acquire_summon_lock(&self) -> Option<std::fs::File> {
        use std::os::unix::io::AsRawFd;
        let _ = std::fs::create_dir_all(&self.cfg.run);
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&self.cfg.summon_lock)
            .ok()?;
        let deadline = self.h.now() + self.cfg.summon_lock_wait as i64;
        loop {
            // SAFETY: flock on a descriptor this process owns exclusively.
            if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Some(f);
            }
            if self.h.now() >= deadline {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    /// ck7_summon_pass -> ck7_summon_body, serialized against every other caller of this
    /// function (and of `aeon --escape`) by one flock on `$SPIRA_RUN/summon.lock`
    /// (lib.sh:1406). ONE LOCK, TWO ENTRY POINTS: the full pass's own CHECK 7 and
    /// `--summon-only` both reach the fleet through this one function, so a slot a fast
    /// pass just filled cannot be filled again by a full pass that read "free" a moment
    /// earlier (sp-0y2av). A FIXED GUARD, NOT A SUBSHELL: unlike the bash original's
    /// `( flock ...; _ck7_summon_body )`, nothing here runs in a copy of this process's
    /// state, so `self.acted`'s counter survives exactly as every other in-process call
    /// already does.
    pub fn ck7_summon_pass(&self) -> i32 {
        let Some(_lock) = self.acquire_summon_lock() else {
            self.log("CHECK7: another summon pass holds summon.lock — skipping this pass");
            return 1;
        };
        self.ck7_summon_body();
        0
    }

    /// named_unit_stop <systemd --user unit glob> -> stop every live unit matching it, BY
    /// NAME (lib.sh:1429). No match is not a failure — the run may already be finished. A
    /// stop that fails is.
    pub fn named_unit_stop(&self, glob: &str) -> (Vec<String>, i32) {
        let o = self.h.run(
            Spec::args_owned(
                self.cfg.systemctl.clone(),
                vec!["--user".into(), "list-units".into(), glob.to_string(), "--all".into(), "--no-legend".into()],
            )
            .err(Io::Null),
        );
        let units: Vec<String> = o
            .stdout
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .map(str::to_string)
            .collect();
        let mut lines = Vec::new();
        let mut rc = 0;
        for u in &units {
            let so = self.h.run(
                Spec::args_owned(self.cfg.systemctl.clone(), vec!["--user".into(), "stop".into(), u.clone()])
                    .err(Io::Null),
            );
            if so.ok() {
                lines.push(format!("stopped {u}"));
            } else {
                self.h.log_err(&format!("could not stop {u}"));
                rc = 1;
            }
        }
        if units.is_empty() {
            lines.push(format!("no unit matches {glob}"));
        }
        (lines, rc)
    }

    /// `sentinel --named-unit-stop <glob>`: lib.sh's `named_unit_stop` shim target, for
    /// `acceptance-local.sh`'s own round-stop step.
    pub fn named_unit_stop_cmd(&self, glob: &str) -> i32 {
        let (lines, rc) = self.named_unit_stop(glob);
        for l in &lines {
            self.h.print(l);
        }
        rc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::parse_beads;

    fn f(n: &str, l: &str, e: &str) -> Fayth {
        Fayth {
            name: n.into(),
            labels: crate::cfg::csv(l),
            exclude: crate::cfg::csv(e),
        }
    }

    #[test]
    fn bucket_is_ready_bucket_py() {
        let ready = parse_beads(
            r#"[{"id":"1","labels":["spira","plan"]},
                {"id":"2","labels":["spira","plan","fayth:ops"]},
                {"id":"3","labels":["spira","incident"]},
                {"id":"4","labels":["spira","plan","spira-queue-waiting"]},
                {"id":"5","labels":["spira","plan","qa-proposed"]}]"#,
        )
        .unwrap();
        let fs = vec![
            f("builder", "spira,plan", "qa-proposed"),
            f("ops", "spira,incident", ""),
            f("spike", "spira", ""),
            f("concierge", "", ""),
        ];
        let c = bucket(
            &ready,
            &fs,
            &["spira-queue-waiting".into(), "spira-submitted".into()],
        );
        assert_eq!(
            c,
            vec![
                ("builder".into(), 1),
                ("ops".into(), 1),
                ("spike".into(), 3)
            ]
        );
        assert_eq!(render_cache(&c), "builder 1\nops 1\nspike 3\n");
    }

    fn row(id: &str, holds: &[&str]) -> LcRow {
        LcRow {
            bead_id: id.into(),
            state: "READY".into(),
            holds: holds.iter().map(|h| h.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn unheld_drops_poison_ask_and_operator_holds_but_keeps_wait_and_rowless() {
        let ready = parse_beads(
            r#"[{"id":"p","labels":["spira","plan"]},
                {"id":"a","labels":["spira","plan"]},
                {"id":"o","labels":["spira","plan"]},
                {"id":"w","labels":["spira","plan"]},
                {"id":"free","labels":["spira","plan"]},
                {"id":"norow","labels":["spira","plan"]}]"#,
        )
        .unwrap();
        let rows = vec![
            row("p", &["poison"]),
            row("a", &["ask"]),
            row("o", &["operator", "wait"]),
            row("w", &["wait"]),
            row("free", &[]),
        ];
        let ids: Vec<String> = unheld(&ready, &rows).into_iter().map(|b| b.id).collect();
        assert_eq!(ids, vec!["w", "free", "norow"]);
        let c = bucket(&unheld(&ready, &rows), &[f("builder", "spira,plan", "")], &[]);
        assert_eq!(c, vec![("builder".into(), 3)]);
    }

    // ---- check7_pool_decision, lane_rotate, ck7_pool, ck7_throttled, ck7_fill_cap -------
    // (lib.sh G1-G3; retired outright with `_ck7_summon_body` — these were each their own
    // `libcall`-direct row in test-watchtower-throttle.sh/test-express-lane.sh/
    // test-summon-fayth.sh; every case below is that same row, ported.)

    #[test]
    fn check7_pool_decision_matches_the_throttle_leak_fix() {
        // throttled + 0 express beads -> 0 (the ordinary hold).
        assert_eq!(check7_pool_decision(true, 5, false), 0);
        // throttled + 1 express bead ready + 5 free -> 1, NOT 5 (sp-zcvh1's own fix: the
        // old inline form returned free here, which is the leak itself).
        assert_eq!(check7_pool_decision(true, 5, true), 1);
        // unthrottled -> free, unchanged either way.
        assert_eq!(check7_pool_decision(false, 5, false), 5);
        assert_eq!(check7_pool_decision(false, 5, true), 5);
    }

    #[test]
    fn lane_rotate_moves_last_and_everything_before_it_to_the_end() {
        let v = |s: &str| -> Vec<String> { s.split_whitespace().map(str::to_string).collect() };
        assert_eq!(lane_rotate("laner", &v("groomer laner")), v("groomer laner"));
        assert_eq!(lane_rotate("", &v("laner groomer")), v("laner groomer"), "no prior last — unchanged");
        assert_eq!(lane_rotate("nosuchlane", &v("laner groomer")), v("laner groomer"), "last not among lanes — unchanged");
        assert_eq!(lane_rotate("laner", &v("laner")), v("laner"), "single lane — unchanged regardless of last");
        assert_eq!(lane_rotate("laner", &[]), Vec::<String>::new(), "no lanes at all — empty");
    }

    #[test]
    fn ck7_pool_subtracts_task_live_and_floors_at_zero() {
        assert_eq!(ck7_pool(Some(4), 1), Some(3));
        assert_eq!(ck7_pool(Some(4), 4), Some(0), "task-live at or above max floors at 0");
        assert_eq!(ck7_pool(None, 0), None, "no pool configured -> no ceiling, today's behaviour unchanged");
    }

    #[test]
    fn ck7_throttled_is_the_stamp_unless_the_override_pins_it_clear() {
        assert!(ck7_throttled(true, ""));
        assert!(!ck7_throttled(false, ""));
        assert!(!ck7_throttled(true, "off"), "override=off pins the pass unthrottled regardless of the stamp");
    }

    #[test]
    fn ck7_fill_cap_stops_on_either_the_pool_or_the_per_persona_cap() {
        assert!(!ck7_fill_cap(1, None, 2), "below the cap, no pool -> continue");
        assert!(ck7_fill_cap(2, None, 2), "at the cap, no pool -> stop");
        assert!(ck7_fill_cap(0, Some(0), 2), "pool exhausted before the cap -> stop");
        assert!(ck7_fill_cap(4, None, 4), "unset SPIRA_MAX_LIVE_AEONS defaults to 4 (caller's job)");
    }

    #[test]
    fn which_refuses_a_bare_name_not_on_path() {
        let d = testkit::TempDir::new("sentinel-which");
        testkit::write_exe(&d.join("aeon"), "#!/bin/sh\n");
        assert_eq!(which("aeon", &d.to_string_lossy()), Some(d.join("aeon").to_string_lossy().into_owned()));
        assert_eq!(which("no-such-binary-anywhere", &d.to_string_lossy()), None);
    }

    /// The exact primitive `acquire_summon_lock` uses (`libc::flock(LOCK_EX)` on
    /// `summon.lock`), proven against two REAL OS threads rather than a hand simulation —
    /// bash's own version of this proof (test-summon-fast-path.sh, before wave 4.27)
    /// stubbed `aeon_count`/`capacity_paused`/`world_gate`/`fayth_ready` by name to show
    /// the unlocked body races and the locked wrapper does not; a compiled binary offers
    /// no such override, so the race it demonstrated is structurally impossible now
    /// instead (there is no entry point to `ck7_summon_body` left that does not take this
    /// same lock first). What is left to prove is the lock itself: two real contenders,
    /// racing to enter, never interleave — the first to enter is also the first to exit.
    #[test]
    fn the_lock_actually_serializes_two_real_contenders() {
        use std::os::unix::io::AsRawFd;
        use std::sync::{Arc, Barrier, Mutex};
        let d = testkit::TempDir::new("sentinel-summon-lock");
        let lockfile = d.join("summon.lock");
        let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|id| {
                let lockfile = lockfile.clone();
                let order = order.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let f = std::fs::OpenOptions::new().create(true).write(true).open(&lockfile).unwrap();
                    barrier.wait(); // both threads reach the lock attempt together
                    // SAFETY: flock on a descriptor this thread owns exclusively.
                    assert_eq!(unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) }, 0);
                    order.lock().unwrap().push(format!("{id}-enter"));
                    std::thread::sleep(std::time::Duration::from_millis(30));
                    order.lock().unwrap().push(format!("{id}-exit"));
                    // dropping `f` releases the lock.
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let order = order.lock().unwrap().clone();
        assert_eq!(order.len(), 4);
        let first_enter = order[0].split('-').next().unwrap();
        let first_exit = order.iter().find(|l| l.ends_with("-exit")).unwrap().split('-').next().unwrap();
        assert_eq!(first_enter, first_exit, "the first to enter must also be the first to exit — never interleaved: {order:?}");
    }
}
