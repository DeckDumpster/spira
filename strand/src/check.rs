//! The two entry points that touch the world: gathering a classification from the live
//! store (or a saved TSV), `report`, and `check` with its mechanical fixes and escalations.

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

fn world_state(run_dir: &std::path::Path) -> Option<&'static str> {
    [("world.halted", "halted"), ("world.draining", "draining")]
        .into_iter()
        .find(|(stamp, _)| run_dir.join(stamp).exists())
        .map(|(_, state)| state)
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
    let total_live = probe::aeons_live_total(cfg);
    let capacity = probe::capacity(cfg, now);
    let throttle = probe::throttle(cfg);

    let lc_rows = match spira_config::lc_state::list() {
        Ok(rows) => rows,
        Err(e) => {
            warn(&format!("WARN the lifecycle rows could not be read ({e}) — no bead is counted as moving by WORKING, parked or poisoned"));
            Vec::new()
        }
    };
    let ids = |f: &dyn Fn(&spira_config::lc_state::Row) -> bool| -> std::collections::HashSet<String> {
        lc_rows.iter().filter(|r| f(r)).map(|r| r.bead_id.clone()).collect()
    };
    let working = ids(&|r| r.working());
    let held_ask = ids(&|r| r.held("ask"));
    let held_poison = ids(&|r| r.held("poison"));
    let mut rows = Vec::new();
    let mut watching = Vec::new();
    for (labels, excl, fayths) in parts {
        watching.push(labels.clone());
        let need: Vec<&str> = labels.split(',').map(str::trim).filter(|x| !x.is_empty()).collect();
        let ready = probe::ready(cfg, &labels, &excl)?;
        let facts = Facts {
            now,
            live: fayths.iter().map(|f| probe::aeon_count(cfg, f, None)).sum(),
            total_live,
            max_aeons: cfg.max_live_aeons,
            capacity_paused: capacity.clone(),
            pool_paused: cfg.max_aeons == Some(0),
            world: world_state(&run_dir),
            pass_truncated: probe::pass_truncated(&log_text, &fayths),
            throttle: throttle.clone(),
        };
        let p = Partition {
            store: &store,
            labels: need.iter().map(|s| s.to_string()).collect(),
            ready,
            working: working.clone(),
            held_ask: held_ask.clone(),
            held_poison: held_poison.clone(),
            vocab: &cfg.vocab,
            facts: &facts,
        };
        rows.extend(p.classify().into_iter().map(|row| PartRow { part: labels.clone(), row }));
    }
    if cfg.labels.is_none() {
        let claimable: std::collections::HashSet<String> = lc_rows
            .iter()
            .filter(|r| r.claimable() && !r.held("ask") && !r.held("poison"))
            .map(|r| r.bead_id.clone())
            .collect();
        match crate::unrostered::personas(cfg) {
            Ok(ps) => rows.extend(
                crate::unrostered::rows(&claimable, &store, &ps, &cfg.lanes, &cfg.shared_exclude())
                    .into_iter()
                    .map(|row| PartRow { part: crate::unrostered::PART.to_string(), row }),
            ),
            Err(e) => warn(&format!("WARN the persona chamber could not be read ({e}) — unrostered READY beads are not checked")),
        }
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

/// Live aeons of the partition's personas, read now rather than from the classification's
/// snapshot. None when the roster cannot be read.
fn lane_live_now(cfg: &Config, specs: Result<Vec<probe::PartitionSpec>, String>) -> Option<u32> {
    let specs = specs.ok()?;
    Some(specs.iter().flat_map(|s| s.fayths.iter()).map(|f| probe::aeon_count(cfg, f, None)).sum())
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
    #[test]
    fn a_planted_deferral_is_found_before_an_empty_result_is_trusted() {
        let rec = "branch=spira/sp-a\nrepo=spira\ncount=7\nsince=1000\nlast=2000\n";
        let d = super::parse_deferred(rec).expect("a real deferral record parses");
        assert_eq!(d, super::Deferred { branch: "spira/sp-a".into(), repo: "spira".into(), count: 7, since: 1000 });
        let out = super::render_deferred(&[d], 1000 + 30 * 3600);
        assert!(out.contains("spira/sp-a") && out.contains("7 consecutive") && out.contains("1800m"), "{out}");
        assert_eq!(super::render_deferred(&[], 5), "");
        assert!(super::parse_deferred("not a record").is_none());
    }

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
        use crate::config::Config;
        let cfg = Config { labels: Some("-".into()), ..Config::test_fixture() };
        let c = from_tsv(&cfg, "ghost\tsp-a\tact\td\ta\n\nbad line\n");
        assert_eq!(c.rows.len(), 1);
        assert_eq!(c.rows[0].part, "-");
        assert_eq!(c.rows[0].row.kind, "ghost");
    }

    // ---- the lifecycle switch (DESIGN.md §9) -------------------------------------------

    fn scratch(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("strand-lc-{tag}"))
    }

    /// An executable that appends its argv to `log` — the fake bd and the fake spira-lc.
    fn recorder(dir: &std::path::Path, name: &str, log: &std::path::Path, reply: &str) -> String {
        let p = dir.join(name);
        testkit::write_exe(&p, &format!("#!/bin/sh\necho \"$@\" >> '{}'\nprintf '%s' '{}'\n", log.display(), reply));
        p.to_string_lossy().into_owned()
    }

    fn cfg(bd: &str, claim: &str) -> Config {
        Config { bd: bd.into(), db: Some("/fake/db".into()), ..Config::test_fixture() }.with_claim(claim)
    }

    #[test]
    fn a_starved_ask_is_withheld_while_the_lane_has_a_live_aeon() {
        let d = scratch("starved-live");
        let spec = || {
            Ok(vec![probe::PartitionSpec { labels: "spira,plan".into(), exclude: String::new(), fayths: vec!["maechen".into()] }])
        };
        let mk = |lines: &str| {
            let p = d.join("systemctl");
            testkit::write_exe(&p, &format!("#!/bin/sh\n{lines}\n"));
            Config { systemctl: p.to_string_lossy().into_owned(), ..Config::test_fixture() }
        };
        let live = mk("echo 'spira-aeon-maechen-1.service loaded active running x'");
        assert_eq!(lane_live_now(&live, spec()), Some(1));
        let none = mk("true");
        assert_eq!(lane_live_now(&none, spec()), Some(0), "positive control: an empty lane reads as starved");
        assert_eq!(lane_live_now(&none, Err("roster".into())), None);
        assert!(starved_stands(None), "an unreadable lane still escalates");
        assert!(starved_stands(Some(0)));
        assert!(!starved_stands(Some(1)));
    }

    // sp-7g5q6: a partition's ready set is spira-claim's — never bd ready — and a refusal is
    // "cannot tell", never empty.
    #[test]
    fn the_ready_set_is_spira_claims_and_bd_is_never_asked() {
        let d = scratch("on-ready");
        let (bdlog, claimlog) = (d.join("bd.log"), d.join("claim.log"));
        let bd = recorder(&d, "bd", &bdlog, r#"[{"id":"sp-bd-says"}]"#);
        let claim = recorder(&d, "spira-claim", &claimlog, r#"[{"id":"sp-machine-says","labels":["spira","plan"]}]"#);
        let c = cfg(&bd, &claim);
        let got = probe::ready(&c, "spira,plan", &["spira-poison".into(), "needs-ryan".into()]).unwrap(); // literal-ok: test fixture
        assert_eq!(got.into_iter().collect::<Vec<_>>(), vec!["sp-machine-says".to_string()]);
        assert_eq!(fs::read_to_string(&claimlog).unwrap(), "ready-count spira,plan spira-poison,needs-ryan --json\n"); // literal-ok: test fixture
        assert!(!bdlog.exists(), "bd's ready query is nobody's ready set");
        let gone = cfg(&bd, "/nonexistent/spira-claim");
        assert!(probe::ready(&gone, "spira,plan", &[]).is_err(), "a refusal is never an empty set");
    }
}
