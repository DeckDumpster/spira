//! The two entry points that touch the world: gathering a classification from the live
//! store (or a saved TSV), `report`, and `check` with its mechanical fixes and escalations.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;

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
}

/// Rows from a saved classifier TSV (`--from <f|->`), attributed to SPIRA_LABELS or `-`.
pub fn from_tsv(cfg: &Config, text: &str) -> Classified {
    let part = cfg.labels.clone().unwrap_or_else(|| "-".into());
    let rows = text
        .lines()
        .filter_map(Row::from_tsv)
        .map(|row| PartRow { part: part.clone(), row })
        .collect();
    Classified { rows, watching: vec![part] }
}

/// Classify every partition against the live store. Err means an input could not be read,
/// and the caller must neither report "nothing stranded" nor rewrite the episode state (R7).
pub fn classify_live(cfg: &Config) -> Result<Classified, String> {
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
            (s.labels, excl, s.fayths)
        })
        .collect();
    if parts.is_empty() {
        warn("WARN no persona in the chamber declares a partition — no work is being watched for strands");
        return Ok(Classified { rows: Vec::new(), watching: Vec::new() });
    }

    let store = Store::new(probe::load_store(cfg)?);
    let now = timefmt::now();
    let log_text = cfg.sentinel_log().and_then(|p| fs::read_to_string(p).ok()).unwrap_or_default();
    let run_dir = cfg.run.clone().ok_or("SPIRA_RUN is unknown (not in the environment, no [spira].run)")?;
    let wait_held = probe::wait_held(cfg, &store.beads)?;
    let total_live = probe::aeons_live_total(cfg);
    let capacity = probe::capacity(cfg, now);
    let throttle = probe::throttle(cfg);

    let mut rows = Vec::new();
    let mut watching = Vec::new();
    for (labels, excl, fayths) in parts {
        watching.push(labels.clone());
        let need: Vec<&str> = labels.split(',').map(str::trim).filter(|x| !x.is_empty()).collect();
        let ready = probe::ready(cfg, &labels, &excl)?;
        let holders: HashMap<String, bool> = store
            .beads
            .iter()
            .filter(|b| b.status == crate::model::Status::InProgress && need.iter().all(|l| b.has(l)))
            .map(|b| (b.id.clone(), probe::holder_alive(&run_dir, &b.id)))
            .collect();
        let facts = Facts {
            now,
            ghost_grace: cfg.ghost_grace,
            live: fayths.iter().map(|f| probe::aeon_count(cfg, f, None)).sum(),
            total_live,
            max_aeons: cfg.max_live_aeons,
            capacity_paused: capacity.clone(),
            pool_paused: cfg.max_aeons == Some(0),
            pass_truncated: probe::pass_truncated(&log_text, &fayths),
            throttle: throttle.clone(),
            holders,
            wait_held: wait_held.clone(),
            lifecycle_enforce: cfg.lifecycle_enforce,
        };
        let p = Partition {
            store: &store,
            labels: need.iter().map(|s| s.to_string()).collect(),
            ready,
            vocab: &cfg.vocab,
            facts: &facts,
        };
        rows.extend(p.classify().into_iter().map(|row| PartRow { part: labels.clone(), row }));
    }
    Ok(Classified { rows, watching })
}

// ------------------------------------------------------------------------------------------
// report
// ------------------------------------------------------------------------------------------

pub fn render_text(active: &str, age: i64, rows: &[AgedRow], watching: &[String]) -> String {
    let mut out = format!("sentinel timer: {active}   last pass: {age}s ago\n\n");
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

pub fn render_json(active: &str, age: i64, rows: &[AgedRow]) -> String {
    let doc = ReportJson {
        sentinel_timer: active.to_string(),
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
    let (active, age) = probe::harness_state(cfg, timefmt::now());
    if json {
        print!("{}", render_json(&active, age, &aged));
    } else {
        print!("{}", render_text(&active, age, &aged, &c.watching));
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
    if r.age < strand_grace {
        return Step::Skip;
    }
    Step::Escalate { detail: r.row.detail.clone() }
}

pub fn check(cfg: &Config, c: Classified, dry: bool) -> i32 {
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
                match r.row.kind.as_str() {
                    "ghost" => act_ghost(cfg, &r.part, &r.row),
                    "stale-blocked" => {
                        let _ = probe::bd(cfg, &["recompute-blocked"], None);
                        println!("RECOMPUTED is_blocked — {} stuck with every blocker closed", r.row.id);
                    }
                    _ => {}
                }
                mark(&state_path, &mut keep, &r.part, &r.row, Mark::Acted);
                handled += 1;
            }
            Step::Escalate { detail } => {
                escalate(cfg, &r.part, &r.row.kind, &r.row.id, &detail, &r.row.action);
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

/// Marks are written as they happen, so a pass that dies half-way cannot re-escalate what it
/// already escalated.
fn mark(path: &Path, st: &mut state::State, part: &str, row: &Row, what: Mark) {
    state::mark(st, part, &row.kind, &row.id, what, timefmt::now());
    if let Err(e) = state::save(path, st) {
        warn(&format!("check: cannot write {}: {e}", path.display()));
    }
}

const RECLAIM_NOTE: &str = "Reclaimed by strand.sh: in_progress with no live aeon holding it and the lease expired. The worker died; this is not an attempt at the work.";

/// The ghost fix: release the dead holder's claim, the `reclaimed` counter, a note, the
/// event, and the once-only reclaim-ceiling escalation. Each step is best-effort, as before:
/// one failing step must not stop the pass from reaching the next row.
fn act_ghost(cfg: &Config, part: &str, row: &Row) {
    let id = row.id.as_str();
    release_dead_holder(cfg, part, id);
    bump_reclaim(cfg, id, "ghost");
    let n = reclaims_of(cfg, id);
    let _ = probe::bd(cfg, &["note", id, "--stdin"], Some(RECLAIM_NOTE.as_bytes()));
    println!("RECLAIMED {} — {}", id, row.detail);
    let n_s = n.map(|v| v.to_string()).unwrap_or_else(|| "?".into());
    spira_event(cfg, "branch.reclaimed", id, &format!("reclaimed {id} — reclaim {n_s}"), &row.detail);
    if n == Some(cfg.reclaim_at) {
        let a = attempts_of(cfg, id).unwrap_or(0);
        escalate(
            cfg,
            part,
            "reclaim-ceiling",
            id,
            &format!("its aeon has died {n_s} times while the work itself has failed {a} time(s) — the bead keeps losing its worker, which is a fault in the host or the summoning rather than in the bead"),
            "read the aeon journal (journalctl --user -u 'spira-aeon-*') and check the account's rate limit. This bead is NOT poisoned and is still being worked; nothing needs to be done to it",
        );
    }
}

/// The switch (DESIGN.md §9) decides which record the claim lives in. Off: the pre-sp-i2m7y
/// `bd reclaim --id <id> --older-than 1s`, scoped to the row's own partition — liveness was
/// already observed directly, so no grace window is left to apply. On: the spira-lc
/// `HolderDead` event. Returns which path ran, for tests.
fn release_dead_holder(cfg: &Config, part: &str, id: &str) -> &'static str {
    if cfg.lifecycle_enforce {
        lc_holder_dead(cfg, id);
        return "spira-lc";
    }
    let mut args = vec!["reclaim", "--id", id, "--older-than", "1s"];
    if part != "-" && !part.is_empty() {
        args.extend(["--label", part]);
    }
    if let Err(e) = probe::bd(cfg, &args, None) {
        warn(&format!("check: {id}: bd reclaim failed: {e}"));
    }
    "bd"
}

/// On only. The step stays best-effort (the rest of the ghost fix still runs), but a
/// machine that cannot be reached, or refuses, is said out loud: under `lifecycle_enforce`
/// it is the record of the claim, and a silent miss leaves the bead held by a dead aeon.
fn lc_holder_dead(cfg: &Config, id: &str) {
    let loud = |why: &str| warn(&format!("check: {id}: lifecycle_enforce is on and spira-lc HolderDead did not happen ({why}) — the bead stays held"));
    let bin = cfg.lc_bin.as_str();
    let o = probe::run("timeout", &["30", bin, "show", id], None, &[]);
    if !o.ok {
        return loud(&format!("show: {}", o.stderr.trim()));
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&o.stdout) else {
        return loud("show: unparseable reply");
    };
    let bead = &v["bead"];
    let state = bead["state"].as_str().unwrap_or("");
    let version = match &bead["version"] {
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        _ => String::new(),
    };
    if state.is_empty() || version.is_empty() {
        return loud("show: no bead row");
    }
    let o = probe::run(
        "timeout",
        &["30", bin, "event", "bead", id, "--expect", state, "--version", &version, "--actor", "strand", "--kind", "\"HolderDead\""],
        None,
        &[],
    );
    if !o.ok {
        loud(&format!("event: {}", o.stderr.trim()));
    }
}

fn uuid4() -> Option<String> {
    fs::read_to_string("/proc/sys/kernel/random/uuid").ok().map(|s| s.trim().to_string())
}

fn bump_reclaim(cfg: &Config, id: &str, cause: &str) {
    if !probe::sql_safe(id) || !probe::sql_safe(&cfg.beads_actor) || !probe::sql_safe(cause) {
        return;
    }
    let Some(u) = uuid4().filter(|u| probe::sql_safe(u)) else { return };
    let q = format!(
        "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('{u}', '{id}', 'reclaimed', '{}', '{cause}', UTC_TIMESTAMP())",
        cfg.beads_actor
    );
    let _ = probe::bd(cfg, &["sql", &q], None);
}

fn reclaims_of(cfg: &Config, id: &str) -> Option<i64> {
    if !probe::sql_safe(id) {
        return None;
    }
    probe::sql_count(
        cfg,
        &format!("SELECT COUNT(*) AS n FROM events WHERE issue_id='{id}' AND event_type='reclaimed'"),
    )
}

/// lib.sh attempts_of: claims minus closes minus thrash/unjudged requeues since the last
/// poison.cleared, floored at 0.
fn attempts_of(cfg: &Config, id: &str) -> Option<i64> {
    if !probe::sql_safe(id) {
        return None;
    }
    let q = format!(
        "select greatest(coalesce(sum(case when event_type='claimed' or (event_type='status_changed' and new_value like '%in_progress%') then 1 else 0 end),0) - coalesce(sum(case when event_type='closed' then 1 else 0 end),0) - coalesce(sum(case when event_type='requeued' and (new_value='thrash' or new_value like 'unjudged%') then 1 else 0 end),0), 0) as n from events where issue_id='{id}' and created_at > coalesce((select max(created_at) from events where issue_id='{id}' and event_type='poison.cleared'), '1970-01-01')"
    );
    probe::sql_count(cfg, &q)
}

/// lib.sh `spira_event`: one line in `$SPIRA_RUN/events.log`, with a per-key cooldown kept in
/// `$SPIRA_RUN/events/<key>` ("<last> <suppressed>"). `bead::event::emit`
/// (`bead/src/event.rs`, sp-ogu8x, wave 4.24, family Z) is the same algorithm's owning-crate
/// home now, but THIS copy is not delegated to it: `bead` depends on `strand` already (for
/// `strand::probe::aeon_alive`), and a `strand` -> `bead` dependency the other way would be a
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
        format!("Spira: {id} keeps losing its aeon — the host, not the bead")
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
    format!("## Question\n{title}\n\n## Default\n{action}\n\n{why}\n\n{ctx}\n")
}

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

fn escalate(cfg: &Config, part: &str, kind: &str, id: &str, detail: &str, action: &str) {
    let title = title_for(part, kind, id);
    let bead = if id.is_empty() || id == "-" { None } else { probe::show(cfg, id) };
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
    let _ = probe::run(
        "mail",
        &["send", "operator", "--from", "Strand check <strand@spira>", "--subject", &title, "--kind", "question", "--default", action],
        Some(body.as_bytes()),
        &[],
    );
}

/// The check state machine over several rows, for tests.
#[cfg(test)]
fn steps(rows: &[AgedRow], dry: bool, grace: i64) -> Vec<Step> {
    rows.iter().map(|r| decide(r, dry, grace)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Row;

    fn aged(kind: &str, disp: Disposition, age: i64, acted: i64, esc: i64) -> AgedRow {
        AgedRow {
            part: "spira,plan".into(),
            row: Row::new(kind, "sp-x", disp, "d".into(), "a".into()),
            age,
            acted,
            escalated: esc,
        }
    }

    #[test]
    fn act_once_then_escalate_once() {
        let g = 900;
        assert_eq!(decide(&aged("ghost", Disposition::Act, 0, 0, 0), false, g), Step::Act);
        assert_eq!(
            decide(&aged("ghost", Disposition::Act, 0, 5, 0), false, g),
            Step::Escalate { detail: "d (the mechanical fix ran and did not clear it)".into() }
        );
        assert_eq!(decide(&aged("ghost", Disposition::Act, 0, 5, 6), false, g), Step::Skip);
        assert_eq!(decide(&aged("stuck", Disposition::Escalate, 899, 0, 0), false, g), Step::Skip, "inside grace");
        assert_eq!(
            decide(&aged("stuck", Disposition::Escalate, 900, 0, 0), false, g),
            Step::Escalate { detail: "d".into() }
        );
        assert_eq!(decide(&aged("stuck", Disposition::Escalate, 9000, 0, 1), false, g), Step::Skip);
        assert_eq!(decide(&aged("sequenced", Disposition::Info, 9000, 0, 0), false, g), Step::Skip);
        assert_eq!(steps(&[aged("stuck", Disposition::Escalate, 0, 0, 0)], true, g), vec![Step::DryRun]);
    }

    #[test]
    fn titles() {
        assert_eq!(title_for("spira,plan", "stuck", "sp-1"), "Spira: sp-1 is stranded (stuck)");
        assert_eq!(title_for("spira,plan", "starved", "-"), "Spira: the [spira,plan] queue is stranded (starved)");
        assert_eq!(title_for("-", "starved", "-"), "Spira: the plan is stranded (starved)");
        assert_eq!(title_for("p", "reclaim-ceiling", "sp-1"), "Spira: sp-1 keeps losing its aeon — the host, not the bead");
    }

    #[test]
    fn report_shapes() {
        let rows = vec![aged("stuck", Disposition::Escalate, 180, 0, 0)];
        let t = render_text("active", 42, &rows, &["spira,plan".into()]);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[0], "sentinel timer: active   last pass: 42s ago");
        assert_eq!(lines[2], "PARTITION        KIND                 ID               DISPOSITION    AGE  DETAIL");
        assert_eq!(lines[3], "spira,plan       stuck                sp-x             escalate      3m  d");
        assert_eq!(lines[4], format!("{}→ a", " ".repeat(71)));
        let empty = render_text("active", 42, &[], &["spira,plan".into(), "spira,incident".into()]);
        assert!(empty.ends_with("no stranded work in [spira,plan spira,incident]\n"));
        let j: serde_json::Value = serde_json::from_str(&render_json("active", 42, &rows)).unwrap();
        assert_eq!(j["sentinel_timer"], "active");
        assert_eq!(j["last_pass_seconds"], 42);
        assert_eq!(j["strands"][0]["partition"], "spira,plan");
        assert_eq!(j["strands"][0]["age_seconds"], 180);
        assert_eq!(j["strands"][0]["escalated_at"], 0);
    }

    #[test]
    fn escalation_body_and_context() {
        let b = crate::model::parse_beads(
            r#"{"id":"sp-1","status":"open","priority":0,"title":"t","labels":["plan"],
                "created_at":"2026-09-28T00:00:00Z","description":"  why  ","notes":"n1\nn2\nn3\nn4"}"#,
        )
        .unwrap();
        let now = crate::timefmt::parse_rfc3339("2026-09-28T05:00:00Z").unwrap();
        let ctx = bead_context(b.first(), "sp-1", now);
        assert!(ctx.starts_with("BEAD    sp-1  [open, P0, open 5h]\nTITLE   t\nLABELS  plan\n\nWHAT THIS BEAD IS FOR\nwhy"));
        assert!(ctx.ends_with("MOST RECENT NOTES\n  - n2\n  - n3\n  - n4"));
        assert_eq!(bead_context(None, "-", now), "(no single bead — this is about the plan as a whole)");
        assert_eq!(bead_context(None, "sp-1", now), "(could not read the bead — say so rather than pretend)");
        assert_eq!(escalation_body("T", "A", "W", "C"), "## Question\nT\n\n## Default\nA\n\nW\n\nC\n");
    }

    #[test]
    fn from_tsv_attributes_the_partition() {
        use crate::config::{Config, Source};
        struct S;
        impl Source for S {
            fn env(&self, k: &str) -> Option<String> {
                (k == "SPIRA_LABELS").then(|| "-".into())
            }
            fn toml(&self, _: &str) -> Option<String> {
                None
            }
        }
        let c = from_tsv(&Config::resolve(&S), "ghost\tsp-a\tact\td\ta\n\nbad line\n");
        assert_eq!(c.rows.len(), 1);
        assert_eq!(c.rows[0].part, "-");
        assert_eq!(c.rows[0].row.kind, "ghost");
    }

    // ---- the lifecycle switch (DESIGN.md §9) -------------------------------------------

    struct Env(Vec<(&'static str, String)>);
    impl crate::config::Source for Env {
        fn env(&self, k: &str) -> Option<String> {
            self.0.iter().find(|(key, _)| *key == k).map(|(_, v)| v.clone())
        }
        fn toml(&self, _: &str) -> Option<String> {
            None
        }
    }

    fn scratch(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("strand-lc-{tag}"))
    }

    /// An executable that appends its argv to `log` — the fake bd and the fake spira-lc.
    fn recorder(dir: &std::path::Path, name: &str, log: &std::path::Path, reply: &str) -> String {
        let p = dir.join(name);
        testkit::write_exe(&p, &format!("#!/bin/sh\necho \"$@\" >> '{}'\nprintf '%s' '{}'\n", log.display(), reply));
        p.to_string_lossy().into_owned()
    }

    fn cfg(enforce: &str, bd: &str, lc: &str) -> Config {
        Config::resolve(&Env(vec![
            ("SPIRA_LIFECYCLE_ENFORCE", enforce.into()),
            ("SPIRA_BD", bd.into()),
            ("SPIRA_DB", "/fake/db".into()),
        ]))
        .with_lc(lc)
    }

    #[test]
    fn off_ghost_fix_is_bd_reclaim_and_never_runs_spira_lc() {
        let d = scratch("off-ghost");
        let (bdlog, lclog) = (d.join("bd.log"), d.join("lc.log"));
        let bd = recorder(&d, "bd", &bdlog, "");
        // A real, executable spira-lc: presence alone must not turn anything on.
        let lc = recorder(&d, "spira-lc", &lclog, "{}");
        let c = cfg("0", &bd, &lc);
        assert!(!c.lifecycle_enforce);
        assert_eq!(release_dead_holder(&c, "spira,plan", "sp-g"), "bd");
        assert_eq!(release_dead_holder(&c, "-", "sp-h"), "bd");
        let calls = fs::read_to_string(&bdlog).unwrap();
        assert!(calls.contains("-C /fake/db reclaim --id sp-g --older-than 1s --label spira,plan"), "{calls}");
        assert!(calls.contains("-C /fake/db reclaim --id sp-h --older-than 1s\n"), "no partition scope for '-': {calls}");
        assert!(!lclog.exists(), "spira-lc must never run with lifecycle_enforce off");
    }

    #[test]
    fn off_wait_exemption_is_the_legacy_label_and_never_runs_spira_lc() {
        let d = scratch("off-wait");
        let lclog = d.join("lc.log");
        let lc = recorder(&d, "spira-lc", &lclog, r#"[{"bead_id":"sp-from-lc"}]"#);
        let c = cfg("", "bd", &lc);
        assert!(!c.lifecycle_enforce, "set-but-empty is off");
        let beads = crate::model::parse_beads(
            r#"[{"id":"sp-w","status":"in_progress","labels":["spira-waiting-operator"]},{"id":"sp-x","status":"in_progress","labels":[]}]"#,
        )
        .unwrap();
        let held = probe::wait_held(&c, &beads).unwrap();
        assert_eq!(held.into_iter().collect::<Vec<_>>(), vec!["sp-w".to_string()]);
        assert!(!lclog.exists(), "spira-lc must never run with lifecycle_enforce off");
    }

    #[test]
    fn on_runs_spira_lc_and_unreachable_is_an_error_not_an_empty_set() {
        let d = scratch("on");
        let (bdlog, lclog) = (d.join("bd.log"), d.join("lc.log"));
        let bd = recorder(&d, "bd", &bdlog, "");
        let lc = recorder(&d, "spira-lc", &lclog, r#"[{"bead_id":"sp-w"}]"#);
        let c = cfg("1", &bd, &lc);
        assert!(c.lifecycle_enforce);
        let held = probe::wait_held(&c, &[]).unwrap();
        assert!(held.contains("sp-w"));
        assert_eq!(release_dead_holder(&c, "spira,plan", "sp-g"), "spira-lc");
        let calls = fs::read_to_string(&lclog).unwrap();
        assert!(calls.contains("list --hold wait") && calls.contains("show sp-g"), "{calls}");
        assert!(!bdlog.exists(), "on: the claim is released through the machine, not bd reclaim");
        let gone = cfg("1", &bd, "/nonexistent/spira-lc");
        let e = probe::wait_held(&gone, &[]).unwrap_err();
        assert!(e.contains("lifecycle_enforce is on and spira-lc is unreachable"), "{e}");
    }
}
