//! The two entry points that touch the world: gathering a classification from the live
//! store (or a saved TSV), `report`, and `check` with its mechanical fixes and escalations.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::Path;

use inbox_rules::Sink;

use crate::classify::{Facts, Partition, Store};
use crate::config::Config;
use crate::model::{AgedRow, Disposition, PartRow, ReportJson, ReportStrand, Row};
use crate::probe;
use crate::state::{self, Mark};
use crate::timefmt;

pub fn log(msg: &str) {
    println!("{} spira: {}", timefmt::utc_stamp(timefmt::now()), msg);
}
pub fn warn(msg: &str) {
    eprintln!("{} spira: {}", timefmt::utc_stamp(timefmt::now()), msg);
}

/// What a run classified, and which partitions it looked at.
pub struct Classified {
    pub rows: Vec<PartRow>,
    pub watching: Vec<String>,
    pub live: Vec<String>,
}

/// Rows from a saved classifier TSV (`--from <f|->`), attributed to SPIRA_LABELS or `-`.
pub fn from_tsv(cfg: &Config, text: &str) -> Classified {
    let part = cfg.labels.clone().unwrap_or_else(|| "-".into());
    let rows = text
        .lines()
        .filter_map(Row::from_tsv)
        .map(|row| PartRow { part: part.clone(), row })
        .collect();
    Classified { rows, watching: vec![part], live: Vec::new() }
}

fn world_state(run_dir: &std::path::Path) -> Option<&'static str> {
    [("world.halted", "halted"), ("world.draining", "draining")]
        .into_iter()
        .find(|(stamp, _)| run_dir.join(stamp).exists())
        .map(|(_, state)| state)
}

/// The beads with a gate job queued, claimed by a worker, or filed and awaiting the pass.
/// None when the host has no gate-worker queue: its SUBMITTED beads are gated inline.
fn gate_queued(run_dir: &Path) -> Option<std::collections::HashSet<String>> {
    let root = run_dir.join("gate-worker");
    if !root.is_dir() {
        return None;
    }
    let mut dirs = vec![root.join("inbox"), root.join("done")];
    if let Ok(rd) = fs::read_dir(root.join("claimed")) {
        dirs.extend(rd.flatten().map(|e| e.path()));
    }
    let mut beads = std::collections::HashSet::new();
    for d in dirs {
        let Ok(rd) = fs::read_dir(d) else { continue };
        for e in rd.flatten() {
            let job: Option<serde_json::Value> = fs::read_to_string(e.path()).ok().and_then(|t| serde_json::from_str(&t).ok());
            let job = job.as_ref();
            let done = job.and_then(|j| j.get("job"));
            if let Some(b) = done.or(job).and_then(|j| j.get("bead")).and_then(|b| b.as_str()) {
                beads.insert(b.to_string());
            }
        }
    }
    Some(beads)
}

/// The landing pass is deliberately holding this bead's gate back until a time it wrote down.
fn backing_off(run_dir: &Path, bead_id: &str, now: i64) -> bool {
    let Ok(t) = fs::read_to_string(run_dir.join("gate-backoff").join(format!("spira-{bead_id}"))) else { return false };
    t.lines().nth(1).and_then(|n| n.parse::<i64>().ok()).is_some_and(|until| until > now)
}

/// Beads of a repo that lands by pull request: nothing gates them, the landing task opens their pull request.
fn pr_mode_beads(reg: &spira_config::repos::Registry, store: &Store) -> std::collections::HashSet<String> {
    store.beads.iter().filter(|b| b.labels.iter().find_map(|l| l.strip_prefix("repo:")).is_some_and(|r| reg.land(r) == "pr")).map(|b| b.id.clone()).collect()
}

/// SUBMITTED beads nothing will gate: no job queued, no backoff, no hold, and not a pull-request repo's.
fn ungated_submitted(run_dir: &Path, rows: &[spira_store::lc_state::Row], pr_mode: &std::collections::HashSet<String>, now: i64) -> std::collections::HashSet<String> {
    let Some(queued) = gate_queued(run_dir) else { return Default::default() };
    rows.iter()
        .filter(|r| r.state == "SUBMITTED" && !queued.contains(&r.bead_id))
        .filter(|r| r.holds.is_empty() || expired_wait_alone(r, now))
        .filter(|r| !pr_mode.contains(&r.bead_id))
        .filter(|r| !backing_off(run_dir, &r.bead_id, now))
        .map(|r| r.bead_id.clone())
        .collect()
}

fn expired_wait_alone(r: &spira_store::lc_state::Row, now: i64) -> bool {
    r.holds.iter().all(|h| h == "wait") && r.reason.as_deref().is_some_and(|reason| spira_store::lc_state::wait_expired(reason, now))
}

pub const TASK_SILENT_S: u64 = 120;

/// A row when `waiting` work is parked on `task` and it has not observed for `TASK_SILENT_S`.
/// A host with no stamp at all has never run the task, which is silence too.
fn task_silent(run_dir: &Path, task: &str, waiting: usize, what: &str) -> Option<PartRow> {
    task_silent_past(run_dir, task, TASK_SILENT_S, waiting, what)
}

/// The scheduler's own tick, `watch.transitions`, which runs whether or not work waits: silent
/// for `SCHEDULER_MISSED_TICKS` of its period means the scheduler is not ticking at all.
pub const SCHEDULER_TASK: &str = "watch.transitions";
pub const SCHEDULER_MISSED_TICKS: u64 = 3;

fn scheduler_silent(run_dir: &Path, period_s: u64) -> Option<PartRow> {
    task_silent_past(run_dir, SCHEDULER_TASK, period_s * SCHEDULER_MISSED_TICKS, 1, "lifecycle wait(s) unwatched")
}

fn task_silent_past(run_dir: &Path, task: &str, silent_s: u64, waiting: usize, what: &str) -> Option<PartRow> {
    if waiting == 0 {
        return None;
    }
    let quiet = spira_config::ticked::quiet_secs(run_dir, task);
    if quiet.is_some_and(|s| s <= silent_s) {
        return None;
    }
    let since = quiet.map_or("never".to_string(), |s| format!("{}m ago", s / 60));
    let part = task.replace('.', "-");
    Some(PartRow {
        part: part.clone(),
        row: Row::new(
            &format!("{part}-silent"),
            task,
            Disposition::Escalate,
            format!("{waiting} {what} and {task} last ticked {since}"),
            format!("check the runtime: its {task} task is not ticking"),
        ),
    })
}

/// A partition's claimable ids from its labels and excluded labels; `Err` is a refused read.
pub type ReadyReader<'a> = dyn Fn(&str, &[String]) -> Result<HashSet<String>, String> + 'a;

/// Classify every partition against the live store. Err means an input could not be read,
/// and the caller must neither report "nothing stranded" nor rewrite the episode state (R7).
pub fn classify_live(cfg: &Config, ready_of: &ReadyReader) -> Result<Classified, String> {
    let specs = probe::roster(cfg, cfg.labels.as_deref())?;
    let parts: Vec<(String, Vec<String>, Vec<String>)> = specs
        .into_iter()
        .map(|s| {
            let exclude = cfg
                .exclude_labels
                .clone()
                .or_else(|| (!s.exclude.is_empty() && cfg.labels.is_none()).then(|| s.exclude.clone()))
                .unwrap_or_else(|| cfg.exclude_default());
            let mut excl: Vec<String> =
                exclude.split(',').map(str::trim).filter(|x| !x.is_empty()).map(str::to_string).collect();
            excl.extend(cfg.shared_exclude());
            (s.labels, excl, s.runes)
        })
        .collect();
    if parts.is_empty() {
        warn("WARN no persona in the runes declares a partition — no work is being watched for strands");
        return Ok(Classified { rows: Vec::new(), watching: Vec::new(), live: Vec::new() });
    }

    let store = Store::new(probe::load_store(&spira_store::lc_call::LcClient::configured()?)?);
    let now = timefmt::now();
    let log_text = cfg.sentinel_log().and_then(|p| fs::read_to_string(p).ok()).unwrap_or_default();
    let run_dir = cfg.run.clone().ok_or("SPIRA_RUN is unknown (not in the environment, no [spira].run)")?;
    let live = probe::live_units(cfg)?;
    let total_live = live.len() as u32;
    let capacity = probe::capacity(cfg, now);
    let throttle = probe::throttle(cfg);
    let uncalled = probe::scheduler_uncalled();

    let lc_rows = match spira_store::lc_state::list_live() {
        Ok(rows) => rows,
        Err(e) => {
            warn(&format!("WARN the lifecycle rows could not be read ({e}) — no bead is counted as moving by WORKING, parked or poisoned"));
            Vec::new()
        }
    };
    let ids = |f: &dyn Fn(&spira_store::lc_state::Row) -> bool| -> std::collections::HashSet<String> {
        lc_rows.iter().filter(|r| f(r)).map(|r| r.bead_id.clone()).collect()
    };
    let working = ids(&|r| r.working());
    let held_ask = ids(&|r| r.held("ask"));
    let held_poison = ids(&|r| r.held("poison"));
    let ungated = ungated_submitted(&run_dir, &lc_rows, &pr_mode_beads(&spira_config::repos::Registry::from_env(std::env::vars().collect(), &cfg.home.clone().unwrap_or_default()), &store), now);
    let stacked_on: std::collections::HashMap<String, Vec<String>> = lc_rows
        .iter()
        .filter(|r| !r.terminal() && !r.stack.is_empty())
        .map(|r| (r.bead_id.clone(), r.stack.clone()))
        .collect();
    let mut rows = Vec::new();
    let mut watching = Vec::new();
    for (labels, excl, runes) in parts {
        watching.push(labels.clone());
        let need: Vec<&str> = labels.split(',').map(str::trim).filter(|x| !x.is_empty()).collect();
        let ready = ready_of(&labels, &excl)?;
        let facts = Facts {
            now,
            live: runes.iter().map(|f| live.iter().filter(|u| runtime::units::is_dwarf_of(u, f, f)).count() as u32).sum(),
            total_live,
            max_dwarves: cfg.max_live_dwarves,
            capacity_paused: capacity.clone(),
            pool_paused: cfg.max_dwarves == Some(0),
            world: world_state(&run_dir),
            pass_truncated: probe::pass_truncated(&log_text, &runes),
            throttle: throttle.clone(),
            scheduler_reason: Some(probe::scheduler_reason(&uncalled, &runes)),
        };
        let p = Partition {
            store: &store,
            labels: need.iter().map(|s| s.to_string()).collect(),
            ready,
            working: working.clone(),
            held_ask: held_ask.clone(),
            held_poison: held_poison.clone(),
            ungated: ungated.clone(),
            stacked_on: stacked_on.clone(),
            vocab: &cfg.vocab,
            facts: &facts,
        };
        rows.extend(p.classify().into_iter().map(|row| PartRow { part: labels.clone(), row }));
    }
    let certified = lc_rows.iter().filter(|r| r.state == "CERTIFIED" && !r.terminal()).count();
    rows.extend(task_silent(&run_dir, "gate.worker", ungated.len(), "SUBMITTED bead(s) ungated"));
    rows.extend(task_silent(&run_dir, "batcher.local", certified, "CERTIFIED bead(s) waiting for a round"));
    rows.extend(scheduler_silent(&run_dir, spira_config::process::cfg_parse("SPIRA_TRANSITIONS_PERIOD")?));
    if cfg.labels.is_none() {
        let claimable: std::collections::HashSet<String> = lc_rows
            .iter()
            .filter(|r| r.claimable() && !r.held("ask") && !r.held("poison"))
            .map(|r| r.bead_id.clone())
            .collect();
        match crate::unrostered::personas(cfg) {
            Ok(ps) => rows.extend(
                crate::unrostered::rows(&claimable, &store, &ps, &cfg.shared_exclude())
                    .into_iter()
                    .map(|row| PartRow { part: crate::unrostered::PART.to_string(), row }),
            ),
            Err(e) => warn(&format!("WARN the persona runes could not be read ({e}) — unrostered READY beads are not checked")),
        }
    }
    Ok(Classified { rows, watching, live })
}

// ------------------------------------------------------------------------------------------
// report
// ------------------------------------------------------------------------------------------

pub fn render_text(live: &[String], age: i64, rows: &[AgedRow], watching: &[String]) -> String {
    let pass = if age < 0 { "never".to_string() } else { format!("{age}s ago") };
    let mut out = format!("scheduler last pass: {pass}\nlive dwarves: {}\n", live.len());
    for u in live {
        out.push_str(&format!("  {u}\n"));
    }
    out.push('\n');
    if rows.is_empty() {
        out.push_str(&format!("no stranded work in [{}]\n", watching.join(" ")));
        return out;
    }
    out.push_str(&format!(
        "{:<16} {:<20} {:<16} {:<9} {:>6}  {}\n",
        "PARTITION", "KIND", "ID", "DISPOSITION", "AGE", "DETAIL"
    ));
    for r in rows {
        out.push_str(&format!(
            "{:<16} {:<20} {:<16} {:<9} {:>5}m  {}\n",
            r.part,
            r.row.kind,
            r.row.id,
            r.row.disp.as_str(),
            r.age / 60,
            r.row.detail
        ));
        out.push_str(&format!("{:71}→ {}\n", "", r.row.action));
    }
    out
}

/// One branch's `landing.deferred` record (key=value lines, written by landing-pass).
#[derive(Debug, PartialEq, Eq)]
pub struct Deferred {
    pub branch: String,
    pub repo: String,
    pub count: u32,
    pub since: i64,
}

pub fn parse_deferred(text: &str) -> Option<Deferred> {
    let mut d = Deferred { branch: String::new(), repo: String::new(), count: 0, since: 0 };
    for l in text.lines() {
        match l.split_once('=')? {
            ("branch", v) => d.branch = v.into(),
            ("repo", v) => d.repo = v.into(),
            ("count", v) => d.count = v.parse().ok()?,
            ("since", v) => d.since = v.parse().ok()?,
            _ => {}
        }
    }
    (d.count > 0 && !d.branch.is_empty()).then_some(d)
}

/// Branches the landing pass is deferring, oldest first. A stranded BRANCH has no bead row.
pub fn deferred_branches(cfg: &Config) -> Vec<Deferred> {
    let Some(dir) = cfg.run.as_ref().map(|r| r.join("landing.deferred")) else { return Vec::new() };
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<Deferred> =
        rd.flatten().filter_map(|e| parse_deferred(&fs::read_to_string(e.path()).ok()?)).collect();
    v.sort_by(|a, b| (a.since, &a.branch).cmp(&(b.since, &b.branch)));
    v
}

pub fn render_deferred(rows: &[Deferred], now: i64) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let mut out = format!("\n{} deferred branch(es) — landing has not reached them:\n", rows.len());
    for d in rows {
        out.push_str(&format!(
            "  {:<28} {:<10} {:>3} consecutive passes, {}m since first deferral\n",
            d.branch,
            d.repo,
            d.count,
            (now - d.since).max(0) / 60
        ));
    }
    out
}

pub fn render_json(live: &[String], age: i64, rows: &[AgedRow]) -> String {
    let doc = ReportJson {
        live_dwarves: live.to_vec(),
        last_pass_seconds: age,
        strands: rows
            .iter()
            .map(|r| ReportStrand {
                partition: r.part.clone(),
                kind: r.row.kind.clone(),
                id: r.row.id.clone(),
                disposition: r.row.disp.as_str().into(),
                age_seconds: r.age,
                acted_at: r.acted,
                escalated_at: r.escalated,
                detail: r.row.detail.clone(),
                action: r.row.action.clone(),
            })
            .collect(),
    };
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".into()) + "\n"
}

pub fn report(cfg: &Config, c: &Classified, json: bool) {
    let st = cfg.state_path().map(|p| state::load(&p)).unwrap_or_default();
    let (aged, _) = state::apply(&st, &c.rows, timefmt::now());
    let age = probe::last_pass_age(cfg);
    if json {
        print!("{}", render_json(&c.live, age, &aged));
    } else {
        print!("{}", render_text(&c.live, age, &aged, &c.watching));
        print!("{}", render_deferred(&deferred_branches(cfg), timefmt::now()));
    }
}

// ------------------------------------------------------------------------------------------
// check
// ------------------------------------------------------------------------------------------

/// What `check` decided to do with one row. Pure, so the state machine is testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Skip,
    DryRun,
    Act,
    Escalate { detail: String },
}

pub fn decide(r: &AgedRow, dry: bool, strand_grace: i64) -> Step {
    if r.row.disp == Disposition::Info {
        return Step::Skip;
    }
    if dry {
        return Step::DryRun;
    }
    if r.row.disp == Disposition::Act && r.acted == 0 {
        return Step::Act;
    }
    if r.escalated != 0 {
        return Step::Skip;
    }
    if r.row.disp == Disposition::Act {
        return Step::Escalate {
            detail: format!("{} (the mechanical fix ran and did not clear it)", r.row.detail),
        };
    }
    if r.age < strand_grace && r.part != crate::unrostered::PART {
        return Step::Skip;
    }
    Step::Escalate { detail: r.row.detail.clone() }
}

pub fn check(cfg: &Config, c: Classified, dry: bool) -> i32 {
    check_into(cfg, &spira_store::lc_call::LcClient::configured(), &Sink::configured("strand"), c, dry)
}

/// [`check`] over a given store door and Concierge inbox.
pub fn check_into(cfg: &Config, door: &Result<spira_store::lc_call::LcClient, String>, inbox: &Result<Sink, String>, c: Classified, dry: bool) -> i32 {
    check_raising(cfg, door, c, dry, &mut |part, kind, id, detail, action| escalate(cfg, inbox, part, kind, id, detail, action))
}

type Raise<'a> = dyn FnMut(&str, &str, &str, &str, &str) + 'a;

fn check_raising(cfg: &Config, door: &Result<spira_store::lc_call::LcClient, String>, c: Classified, dry: bool, raise: &mut Raise) -> i32 {
    let Some(state_path) = cfg.state_path() else {
        warn("FATAL SPIRA_RUN is unknown — cannot keep episode state");
        return 2;
    };
    let lock_path = state_path.with_file_name("strands.json.lock");
    let _lock = match state::try_lock(&lock_path) {
        Ok(Some(f)) => f,
        Ok(None) => {
            log("check: another instance holds the lock — declining to avoid acting on stale state");
            return 0;
        }
        Err(e) => {
            warn(&format!("check: cannot open {}: {e}", lock_path.display()));
            return 2;
        }
    };
    let now = timefmt::now();
    let st = state::load(&state_path);
    let (aged, mut keep) = state::apply(&st, &c.rows, now);
    if !dry {
        if let Err(e) = state::save(&state_path, &keep) {
            warn(&format!("check: cannot write {}: {e}", state_path.display()));
        }
    }
    let mut handled = 0;
    for r in &aged {
        match decide(r, dry, cfg.strand_grace) {
            Step::Skip => continue,
            Step::DryRun => {
                log(&format!(
                    "would {} {} {} in [{}] ({}s, acted={} escalated={}): {}",
                    r.row.disp.as_str(),
                    r.row.kind,
                    r.row.id,
                    r.part,
                    r.age,
                    r.acted,
                    r.escalated,
                    r.row.detail
                ));
            }
            Step::Act => {
                if r.row.kind.as_str() == "stale-blocked" {
                    let _ = probe::recompute_blocked(door);
                    println!("RECOMPUTED is_blocked — {} stuck with every blocker closed", r.row.id);
                }
                mark(&state_path, &mut keep, &r.part, &r.row, Mark::Acted);
                handled += 1;
            }
            Step::Escalate { detail } => {
                if r.row.kind == "starved" {
                    let live = lane_live_now(cfg, probe::roster(cfg, Some(&r.part)));
                    if !starved_stands(live) {
                        log(&format!("check: [{}] starved cleared at raise time — no ask written", r.part));
                        continue;
                    }
                    if live.is_none() {
                        warn(&format!("check: [{}] lane liveness unreadable at raise time — escalating on the classification", r.part));
                    }
                }
                raise(&r.part, &r.row.kind, &r.row.id, &detail, &r.row.action);
                mark(&state_path, &mut keep, &r.part, &r.row, Mark::Escalated);
                println!("STRANDED {} {} — {}", r.row.kind, r.row.id, detail);
                handled += 1;
            }
        }
    }
    if handled > 0 {
        log(&format!("check: {handled} stranded item(s) handled"));
    }
    0
}

/// Live dwarves of the partition's personas, read now rather than from the classification's
/// snapshot. None when the roster cannot be read.
fn lane_live_now(cfg: &Config, specs: Result<Vec<probe::PartitionSpec>, String>) -> Option<u32> {
    let specs = specs.ok()?;
    let live = probe::live_units(cfg).ok()?;
    Some(specs.iter().flat_map(|s| s.runes.iter()).map(|f| live.iter().filter(|u| runtime::units::is_dwarf_of(u, f, f)).count() as u32).sum())
}

/// Only a lane read as live withholds the ask; a failed probe must not suppress it silently.
fn starved_stands(live: Option<u32>) -> bool {
    !matches!(live, Some(n) if n > 0)
}

/// Marks are written as they happen, so a pass that dies half-way cannot re-escalate what it
/// already escalated.
fn mark(path: &Path, st: &mut state::State, part: &str, row: &Row, what: Mark) {
    state::mark(st, part, &row.kind, &row.id, what, timefmt::now());
    if let Err(e) = state::save(path, st) {
        warn(&format!("check: cannot write {}: {e}", path.display()));
    }
}

/// lib.sh `spira_event`: one line in `$SPIRA_RUN/events.log`, with a per-key cooldown kept in
/// `$SPIRA_RUN/events/<key>` ("<last> <suppressed>"). `bead::event::emit`
/// (`bead/src/event.rs`, sp-ogu8x, wave 4.24, family Z) is the same algorithm's owning-crate
/// home now, but THIS copy is not delegated to it: `bead` depends on `strand` already (for
/// `strand::probe::dwarf_alive`), and a `strand` -> `bead` dependency the other way would be a
/// cycle Cargo refuses to build. See `bead/DESIGN.md` "event" — "strand's own duplicate" for
/// the full account and the follow-up this leaves (break the cycle with a third, lower crate
/// before actually collapsing this copy).
///
/// One behavioural fix carried here independently of that consolidation: the LOGGED
/// timestamp is the real wall clock (`wall_clock_now`), not `timefmt::now()` (which honours
/// `$SPIRA_NOW`) — matching the bash's own fresh `date -u` call, which never read
/// `$SPIRA_NOW` either. The cooldown *decision* and the cooldown file still use the
/// (possibly injected) `now` the caller passes, exactly as the bash's own `now` variable did.
pub fn spira_event(cfg: &Config, kind: &str, target: &str, title: &str, detail: &str) {
    let Some(run_dir) = cfg.run.as_ref() else { return };
    let dir = run_dir.join("events");
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let target = if target == "-" { "" } else { target };
    let key: String = format!("{kind}@{}", if target.is_empty() { "plan" } else { target })
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | '-') { c } else { '_' })
        .collect();
    let now = timefmt::now();
    let cooldown = cfg.event_cooldown;
    // Prune cooldown files well past their window — real wall-clock age, not `now`.
    let max_age = std::time::Duration::from_secs((((cooldown * 2) / 60 + 1) * 60).max(60) as u64);
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            let old = e
                .metadata()
                .ok()
                .filter(|m| m.is_file())
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|el| el > max_age);
            if old {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    let f = dir.join(&key);
    let (last, supp) = fs::read_to_string(&f)
        .ok()
        .map(|s| {
            let mut it = s.split_whitespace();
            (
                it.next().and_then(|x| x.parse::<i64>().ok()).unwrap_or(0),
                it.next().and_then(|x| x.parse::<i64>().ok()).unwrap_or(0),
            )
        })
        .unwrap_or((0, 0));
    if last > 0 && now - last < cooldown {
        let _ = fs::write(&f, format!("{} {}\n", last, supp + 1));
        return;
    }
    let mut title = title.to_string();
    if supp > 0 {
        title.push_str(&format!(" (+{supp} more since {})", timefmt::utc_hhmm(last)));
    }
    let _ = fs::write(&f, format!("{now} 0\n"));
    let mut line = format!(
        "{}\tkind: {}\ttarget: {}\t{}",
        timefmt::utc_stamp(wall_clock_now()),
        kind,
        if target.is_empty() { "-" } else { target },
        title
    );
    if !detail.is_empty() {
        line.push('\t');
        line.push_str(detail);
    }
    line.push('\n');
    if let Ok(mut fh) = fs::OpenOptions::new().create(true).append(true).open(run_dir.join("events.log")) {
        let _ = fh.write_all(line.as_bytes());
    }
}

/// The real wall clock, never `$SPIRA_NOW` — see `spira_event`'s doc comment.
fn wall_clock_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn title_for(part: &str, kind: &str, id: &str) -> String {
    if kind == "reclaim-ceiling" {
        format!("Spira: {id} keeps losing its dwarf — the host, not the bead")
    } else if !id.is_empty() && id != "-" {
        format!("Spira: {id} is stranded ({kind})")
    } else if !part.is_empty() && part != "-" {
        format!("Spira: the [{part}] queue is stranded ({kind})")
    } else {
        format!("Spira: the plan is stranded ({kind})")
    }
}

/// lib.sh bead_context: the bead itself, not just its id.
pub fn bead_context(bead: Option<&crate::model::Bead>, id: &str, now: i64) -> String {
    if id.is_empty() || id == "-" {
        return "(no single bead — this is about the plan as a whole)".into();
    }
    let Some(b) = bead else {
        return "(could not read the bead — say so rather than pretend)".into();
    };
    let age = b
        .created_at
        .as_deref()
        .and_then(timefmt::parse_rfc3339)
        .map(|t| {
            let h = (now - t) / 3600;
            if h < 48 { format!("{h}h") } else { format!("{}d", h / 24) }
        })
        .unwrap_or_else(|| "?".into());
    let mut out = format!(
        "BEAD    {}  [{}, P{}, open {}]\n",
        b.id,
        b.status.as_str(),
        b.priority.map(|p| p.to_string()).unwrap_or_else(|| "?".into()),
        age
    );
    out.push_str(&format!("TITLE   {}\n", b.title.as_deref().filter(|t| !t.is_empty()).unwrap_or("(none)")));
    let labs = if b.labels.is_empty() { "(none)".to_string() } else { b.labels.join(", ") };
    out.push_str(&format!("LABELS  {labs}\n\nWHAT THIS BEAD IS FOR\n"));
    let desc = b.description.as_deref().map(str::trim).filter(|d| !d.is_empty());
    out.push_str(desc.unwrap_or("(no description — that is itself the problem)"));
    if let Some(notes) = b.notes.as_ref().filter(|n| !n.0.is_empty()) {
        out.push_str("\n\nMOST RECENT NOTES");
        let start = notes.0.len().saturating_sub(3);
        for n in &notes.0[start..] {
            let n: String = n.trim().chars().take(400).collect();
            out.push_str(&format!("\n  - {n}"));
        }
    }
    out
}

pub fn escalation_body(title: &str, action: &str, why: &str, ctx: &str) -> String {
    format!("## Alert\n{title}\n\nAction: {action}\n\n{why}\n\n{ctx}\n")
}

/// A stranded-work alert is about the very mailboxes a dead reader leaves unread; it is an alert
/// to the operator with the bypass, without which it is rerouted to the concierge.
pub fn operator_send_args<'a>(title: &'a str, condition: &'a str) -> Vec<&'a str> {
    vec!["send", "operator", "--from", "Strand check <strand@spira>", "--subject", title, "--kind", "alert", "--condition", condition]
}

/// The `X-Spira-Condition` of a stranded-work alarm: the owner `mail tidy` asks, and the episode
/// key in `strands.json`.
pub fn condition_key(part: &str, kind: &str, id: &str) -> String {
    format!("strand:{}", state::key(part, kind, id))
}

/// `strand condition <part:kind:id>`: exit [`CONDITION_ENDED`] once the episode is gone from the
/// state, 0 while it stands, 1 when that cannot be told.
pub const CONDITION_ENDED: i32 = 10;
pub fn condition_exit(cfg: &Config, episode: &str) -> i32 {
    match cfg.state_path().and_then(|p| state::episode_stands(&p, episode)) {
        Some(true) => 0,
        Some(false) => CONDITION_ENDED,
        None => 1,
    }
}

pub const OPERATOR_CONSIDERED: (&str, &str) = ("SPIRA_MAIL_OPERATOR_CONSIDERED", "strand-check alerts the operator about stranded work whoever reads the concierge");

fn tail_lines(path: Option<&Path>, n: usize) -> String {
    match path.and_then(|p| fs::read(p).ok()) {
        Some(b) => {
            let text = String::from_utf8_lossy(&b);
            let lines: Vec<&str> = text.lines().collect();
            lines[lines.len().saturating_sub(n)..].join("\n")
        }
        None => "(sentinel log unreadable)".into(),
    }
}

fn inbox_line(title: &str, detail: &str, action: &str) -> String {
    format!("[strand] {title}: {detail} — {action}")
}

fn escalate_to_inbox(inbox: &Result<Sink, String>, title: &str, detail: &str, action: &str) {
    let sink = match inbox {
        Ok(sink) => sink,
        Err(e) => {
            warn(&format!("check: the concierge inbox cannot be written ({e}) — the unrostered row reached no one"));
            return;
        }
    };
    let line = format!("{} {}", timefmt::utc_stamp(timefmt::now()), inbox_line(title, detail, action).replace('\n', " "));
    if let Err(e) = sink.record(&line) {
        warn(&format!("check: the concierge inbox: {e}"));
    }
}

fn escalate(cfg: &Config, inbox: &Result<Sink, String>, part: &str, kind: &str, id: &str, detail: &str, action: &str) {
    let title = title_for(part, kind, id);
    if part == crate::unrostered::PART {
        escalate_to_inbox(inbox, &title, detail, action);
        return;
    }
    let bead = if id.is_empty() || id == "-" { None } else { match spira_store::lc_call::LcClient::configured() {
            Ok(lc) => probe::show(&lc, id),
            Err(e) => {
                warn(&format!("WARN the escalation for {id} carries no bead context: {e}"));
                None
            }
        } };
    let ctx = format!(
        "{}\n\nWHY THIS IS ESCALATED\n{}\n\nSENTINEL STATE\n{}",
        bead_context(bead.as_ref(), id, timefmt::now()),
        detail,
        tail_lines(cfg.sentinel_log().as_deref(), 12)
    );
    let why = if kind == "reclaim-ceiling" {
        format!("{detail} — the bead is still claimable and nothing below it is blocked")
    } else {
        format!("{detail} — nothing in the plan below it can move until this clears")
    };
    let body = escalation_body(&title, action, &why, &ctx);
    // mail, by name on the launcher's PATH (sp-gypjk).
    let condition = condition_key(part, kind, id);
    let _ = probe::run(
        "mail",
        &operator_send_args(&title, &condition),
        Some(body.as_bytes()),
        &[OPERATOR_CONSIDERED],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Row;

    #[test_id(tn4crgz)]
    #[test]
    fn an_unrostered_row_escalates_inside_the_grace_and_a_stuck_row_does_not() {
        let mut r = aged("unrostered", Disposition::Escalate, 0, 0, 0);
        assert_eq!(decide(&r, false, 900), Step::Skip, "a row of another partition waits out the grace");
        r.part = crate::unrostered::PART.into();
        assert_eq!(decide(&r, false, 900), Step::Escalate { detail: "d".into() });
        r.escalated = 5;
        assert_eq!(decide(&r, false, 900), Step::Skip, "once");
    }

    #[test_id(t576zgl)]
    #[test]
    fn the_inbox_line_names_the_row_and_its_action() {
        let l = inbox_line("Spira: sp-x is stranded (unrostered)", "no persona", "roster it");
        assert!(l.contains("sp-x") && l.contains("no persona") && l.contains("roster it"), "{l}");
    }

    fn no_door() -> Result<spira_store::lc_call::LcClient, String> {
        Err("no door in this test".into())
    }

    // covers: UC-ops-detection-remediation-21
    #[test_id(t4hdcof)]
    #[test]
    fn a_stale_blocked_act_sends_recompute_blocked_to_the_door() {
        use spira_store::lc_call::{fake::Content, LcClient};
        let run = testkit::TempDir::new("strand-check-recompute");
        let cfg = Config { run: Some(run.to_path_buf()), strand_grace: 0, ..Config::test_fixture() };
        let rows = vec![PartRow { part: "spira,plan".into(), row: Row::new("stale-blocked", "sp-x", Disposition::Act, "d".into(), "a".into()) }];
        let door = std::sync::Arc::new(Content::new(vec![]));
        let rc = check_raising(&cfg, &Ok(LcClient::over(door.clone(), true)), Classified { rows, watching: Vec::new(), live: Vec::new() }, false, &mut |_, _, _, _, _| {});
        assert_eq!(rc, 0);
        assert_eq!(door.requests(), vec![vec!["content".to_string(), "recompute-blocked".to_string()]]);
    }

    fn escalating_fixture(run: &Path, disp: Disposition) -> (Config, Classified) {
        let cfg = Config { run: Some(run.to_path_buf()), strand_grace: 0, ..Config::test_fixture() };
        let rows = vec![PartRow { part: "spira,plan".into(), row: Row::new("stuck", "sp-x", disp, "d".into(), "a".into()) }];
        (cfg, Classified { rows, watching: Vec::new(), live: Vec::new() })
    }

    // covers: UC-ops-detection-remediation-21
    #[test_id(t24lwwi)]
    #[test]
    fn a_runner_that_cannot_take_the_lock_declines_and_the_episode_is_escalated_once() {
        let run = testkit::TempDir::new("strand-check-lock");
        let (cfg, c) = escalating_fixture(&run, Disposition::Escalate);
        let mut raised = 0;
        let mut inner_rc = None;
        let rc = check_raising(&cfg, &no_door(), c, false, &mut |_, _, _, _, _| {
            raised += 1;
            // The first runner is inside its critical section, holding the lock, right now.
            let (cfg2, c2) = escalating_fixture(&run, Disposition::Escalate);
            inner_rc = Some(check_raising(&cfg2, &no_door(), c2, false, &mut |_, _, _, _, _| panic!("a declined runner raised an escalation")));
        });
        assert_eq!(rc, 0);
        assert_eq!(inner_rc, Some(0), "the second runner declines cleanly");
        assert_eq!(raised, 1);

        let (cfg3, c3) = escalating_fixture(&run, Disposition::Escalate);
        let rc = check_raising(&cfg3, &no_door(), c3, false, &mut |_, _, _, _, _| panic!("the same episode escalated again"));
        assert_eq!(rc, 0, "a later runner finds the episode already escalated");
    }

    // covers: UC-ops-detection-remediation-21
    #[test_id(tesv4t1)]
    #[test]
    fn an_info_row_never_raises_an_escalation() {
        let run = testkit::TempDir::new("strand-check-info");
        let (cfg, c) = escalating_fixture(&run, Disposition::Info);
        let rc = check_raising(&cfg, &no_door(), c, false, &mut |_, _, _, _, _| panic!("an info row reached the escalation path"));
        assert_eq!(rc, 0);
    }

    fn aged(kind: &str, disp: Disposition, age: i64, acted: i64, esc: i64) -> AgedRow {
        AgedRow {
            part: "spira,plan".into(),
            row: Row::new(kind, "sp-x", disp, "d".into(), "a".into()),
            age,
            acted,
            escalated: esc,
        }
    }

    #[test_id(twmcflu)]
    #[test]
    fn a_stranded_alert_is_an_alert_to_the_operator_with_the_bypass_and_no_question() {
        let a = operator_send_args("t", "strand:p:k:i");
        assert_eq!(a[1], "operator");
        assert!(a.windows(2).any(|w| w == ["--kind", "alert"]), "{a:?}");
        assert!(!a.contains(&"--default") && !a.contains(&"question"), "{a:?}");
        assert!(a.windows(2).any(|w| w == ["--condition", "strand:p:k:i"]), "{a:?}");
        assert_eq!(OPERATOR_CONSIDERED.0, "SPIRA_MAIL_OPERATOR_CONSIDERED");
        let b = escalation_body("T", "do x", "why", "ctx");
        assert!(b.starts_with("## Alert\n") && b.contains("do x"), "{b}");
    }

    #[test_id(txfb4en)]
    #[test]
    fn a_stranded_alarms_condition_stands_while_its_episode_is_in_the_state_and_ends_when_it_leaves() {
        let run = testkit::TempDir::new("strand-condition");
        let cfg = Config { run: Some(run.to_path_buf()), ..Config::test_fixture() };
        let key = state::key("spira,plan", "starved", "-");
        assert_eq!(condition_key("spira,plan", "starved", "-"), format!("strand:{key}"));
        assert_eq!(condition_exit(&cfg, &key), CONDITION_ENDED, "no state file holds no episode");
        let mut st = state::State::new();
        state::mark(&mut st, "spira,plan", "starved", "-", Mark::Escalated, 5);
        state::save(&cfg.state_path().unwrap(), &st).unwrap();
        assert_eq!(condition_exit(&cfg, &key), 0, "positive control: the episode stands");
        assert_eq!(condition_exit(&cfg, "spira,plan:stuck:sp-x"), CONDITION_ENDED);
        fs::write(cfg.state_path().unwrap(), "{not json").unwrap();
        assert_eq!(condition_exit(&cfg, &key), 1, "a corrupt state is not an ended episode");
    }

    #[test_id(t2rlx7r)]
    #[test]
    fn titles() {
        assert_eq!(title_for("spira,plan", "stuck", "sp-1"), "Spira: sp-1 is stranded (stuck)");
        assert_eq!(title_for("spira,plan", "starved", "-"), "Spira: the [spira,plan] queue is stranded (starved)");
        assert_eq!(title_for("-", "starved", "-"), "Spira: the plan is stranded (starved)");
        assert_eq!(title_for("p", "reclaim-ceiling", "sp-1"), "Spira: sp-1 keeps losing its dwarf — the host, not the bead");
    }

    #[test_id(t1lxolo)]
    #[test]
    fn the_raise_time_lane_count_reads_units_whatever_the_call_mode_and_none_when_unreadable() {
        let t = testkit::TempDir::new("strand-lane-live");
        let spec = || vec![probe::PartitionSpec { labels: "spira,plan".into(), exclude: String::new(), runes: vec!["builder".into()] }];
        let sc = t.join("systemctl");
        testkit::write_exe(&sc, "#!/bin/sh\ncase \"$*\" in *spira-run-sentinel*) printf 'Id=spira-run-sentinel_2dcall-builder_2d1_2d0.service\\nLoadState=loaded\\nActiveState=active\\nSubState=running\\n\\n';; esac\n");
        let cfg = Config { call: "runtime".into(), systemctl: sc.to_string_lossy().into_owned(), ..Config::test_fixture() };
        assert_eq!(lane_live_now(&cfg, Ok(spec())), Some(1));
        testkit::write_exe(&sc, "#!/bin/sh\nexit 1\n");
        assert_eq!(lane_live_now(&cfg, Ok(spec())), None);
    }

    #[test_id(twy1nfa)]
    #[test]
    fn report_shapes() {
        let rows = vec![aged("stuck", Disposition::Escalate, 180, 0, 0)];
        let t = render_text(&["spira-dwarf-builder-1.service".into()], 42, &rows, &["spira,plan".into()]);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[0], "scheduler last pass: 42s ago");
        assert_eq!(lines[1], "live dwarves: 1");
        assert_eq!(lines[2], "  spira-dwarf-builder-1.service");
        assert_eq!(lines[4], "PARTITION        KIND                 ID               DISPOSITION    AGE  DETAIL");
        assert_eq!(lines[5], "spira,plan       stuck                sp-x             escalate      3m  d");
        assert_eq!(lines[6], format!("{}→ a", " ".repeat(71)));
        let empty = render_text(&[], -1, &[], &["spira,plan".into(), "spira,incident".into()]);
        assert!(empty.ends_with("no stranded work in [spira,plan spira,incident]\n"));
        let j: serde_json::Value = serde_json::from_str(&render_json(&["u.service".into()], 42, &rows)).unwrap();
        assert_eq!(j["live_dwarves"][0], "u.service");
        assert_eq!(j["last_pass_seconds"], 42);
        assert_eq!(j["strands"][0]["partition"], "spira,plan");
        assert_eq!(j["strands"][0]["age_seconds"], 180);
        assert_eq!(j["strands"][0]["escalated_at"], 0);
    }

    // ---- the lifecycle switch (DESIGN.md §9) -------------------------------------------
}

#[cfg(test)]
mod ungated_submitted_tests {
    use super::*;

    #[test_id(tqqki7j)]
    #[test]
    fn a_held_submitted_bead_is_not_ungated() {
        let run = testkit::TempDir::new("strand-ungated");
        fs::create_dir_all(run.join("gate-worker/inbox")).unwrap();
        let row = |id: &str| spira_store::lc_state::Row { bead_id: id.into(), state: "SUBMITTED".into(), ..Default::default() };
        let held = spira_store::lc_state::Row { holds: vec!["wait".into()], ..row("sp-b") };
        let rows = [row("sp-a"), held];
        let unheld = [row("sp-a"), row("sp-b")];
        assert_eq!(ungated_submitted(&run, &unheld, &Default::default(), 1000).len(), 2, "the matcher must be able to find an ungated bead");
        assert_eq!(ungated_submitted(&run, &rows, &Default::default(), 1000), ["sp-a".to_string()].into_iter().collect());
    }

    #[test_id(tqgwrbp)]
    #[test]
    fn a_submitted_bead_whose_timed_wait_expired_is_ungated_and_any_other_hold_is_not() {
        let run = testkit::TempDir::new("strand-expired-wait");
        fs::create_dir_all(run.join("gate-worker/inbox")).unwrap();
        let row = |id: &str, holds: &[&str], reason: &str| spira_store::lc_state::Row { bead_id: id.into(), state: "SUBMITTED".into(), holds: holds.iter().map(|h| h.to_string()).collect(), reason: Some(reason.into()), ..Default::default() };
        let rows = [
            row("sp-expired", &["wait"], "snooze-until:900"),
            row("sp-live", &["wait"], "snooze-until:1100"),
            row("sp-untimed", &["wait"], "sp-blocker"),
            row("sp-ask", &["wait", "ask"], "snooze-until:900"),
        ];
        assert_eq!(ungated_submitted(&run, &rows, &Default::default(), 1000), ["sp-expired".to_string()].into_iter().collect());
    }

    #[test_id(th6awlv)]
    #[test]
    fn a_submitted_bead_of_a_pull_request_repo_is_not_ungated() {
        let run = testkit::TempDir::new("strand-pr-mode");
        fs::create_dir_all(run.join("gate-worker/inbox")).unwrap();
        let row = |id: &str| spira_store::lc_state::Row { bead_id: id.into(), state: "SUBMITTED".into(), ..Default::default() };
        let rows = [row("sp-a"), row("sp-b")];
        assert_eq!(ungated_submitted(&run, &rows, &Default::default(), 1000).len(), 2, "the matcher must be able to find an ungated bead");
        let pr_mode = ["sp-b".to_string()].into_iter().collect();
        assert_eq!(ungated_submitted(&run, &rows, &pr_mode, 1000), ["sp-a".to_string()].into_iter().collect());
    }

    #[test_id(t8bbxy2)]
    #[test]
    fn pr_mode_beads_are_the_beads_labelled_with_a_repo_that_lands_by_pull_request() {
        let reg = spira_config::repos::Registry::new(Some("prrepo|/x|pr\ngaterepo|/y|push\n"), &Default::default(), Path::new("nonexistent"));
        let bead = |id: &str, repo: &str| crate::model::Bead { id: id.into(), labels: vec![format!("repo:{repo}")], ..Default::default() };
        let store = Store::new(vec![bead("sp-a", "prrepo"), bead("sp-b", "gaterepo"), crate::model::Bead { id: "sp-c".into(), ..Default::default() }]);
        assert_eq!(pr_mode_beads(&reg, &store), ["sp-a".to_string()].into_iter().collect());
    }

    #[test_id(tzevssy)]
    #[test]
    fn a_submitted_bead_whose_gate_is_backing_off_is_not_ungated() {
        let run = testkit::TempDir::new("strand-backoff");
        fs::create_dir_all(run.join("gate-worker/inbox")).unwrap();
        fs::create_dir_all(run.join("gate-backoff")).unwrap();
        let row = |id: &str| spira_store::lc_state::Row { bead_id: id.into(), state: "SUBMITTED".into(), ..Default::default() };
        let rows = [row("sp-a"), row("sp-b")];
        let now = 1000;
        assert_eq!(ungated_submitted(&run, &rows, &Default::default(), now).len(), 2, "the matcher must be able to find an ungated bead");
        fs::write(run.join("gate-backoff/spira-sp-b"), format!("t1\n{}\n", now + 60)).unwrap();
        assert_eq!(ungated_submitted(&run, &rows, &Default::default(), now), ["sp-a".to_string()].into_iter().collect());
        fs::write(run.join("gate-backoff/spira-sp-b"), format!("t1\n{}\n", now - 1)).unwrap();
        assert_eq!(ungated_submitted(&run, &rows, &Default::default(), now).len(), 2, "an ended backoff is a stranding again");
    }

    #[test_id(tx1f0ud)]
    #[test]
    fn work_waiting_on_a_task_that_has_not_ticked_is_reported_and_a_fresh_tick_is_not() {
        let run = testkit::TempDir::new("strand-task-silent");
        for task in ["gate.worker", "batcher.local"] {
            assert!(task_silent(&run, task, 0, "waiting").is_none(), "no work waiting, nothing to report");
            let silent = task_silent(&run, task, 1, "waiting").expect("never ticked");
            assert_eq!(silent.row.kind, format!("{}-silent", task.replace('.', "-")));
            spira_config::ticked::stamp(&run, task);
            assert!(task_silent(&run, task, 1, "waiting").is_none(), "ticked just now");
            let old = std::time::SystemTime::now() - std::time::Duration::from_secs(TASK_SILENT_S + 60);
            fs::File::options().write(true).open(spira_config::ticked::path(&run, task)).unwrap().set_modified(old).unwrap();
            assert!(task_silent(&run, task, 1, "waiting").is_some(), "ticked over two minutes ago");
        }
    }

    #[test_id(tupi8yf)]
    #[test]
    fn a_scheduler_whose_transitions_tick_went_stale_is_reported_whether_or_not_work_waits() {
        let run = testkit::TempDir::new("strand-scheduler-silent");
        let silent = scheduler_silent(&run, 300).expect("never ticked");
        assert_eq!(silent.row.kind, "watch-transitions-silent");
        spira_config::ticked::stamp(&run, SCHEDULER_TASK);
        assert!(scheduler_silent(&run, 300).is_none(), "ticked just now");
        let late = std::time::SystemTime::now() - std::time::Duration::from_secs(300 * SCHEDULER_MISSED_TICKS - 60);
        fs::File::options().write(true).open(spira_config::ticked::path(&run, SCHEDULER_TASK)).unwrap().set_modified(late).unwrap();
        assert!(scheduler_silent(&run, 300).is_none(), "two periods late is not yet three missed ticks");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(300 * SCHEDULER_MISSED_TICKS + 60);
        fs::File::options().write(true).open(spira_config::ticked::path(&run, SCHEDULER_TASK)).unwrap().set_modified(old).unwrap();
        assert!(scheduler_silent(&run, 300).is_some(), "three periods without a tick");
    }
}
