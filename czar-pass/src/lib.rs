//! The czar's detectors, pure: each invariant's raw reading from what was observed, and the
//! remedy ladder's fixed words (subjects, refs, the shadow line). The binary's pass and the
//! scheduler's `czar` task read the world their own way and judge it here; the hysteresis is
//! `reconciler_engine::core::step` for both.

use reconciler_engine::core::{RawStatus, Verdict};

/// The landing line that is a deadlock: the verdict left the batch open with no suite to blame.
pub const DEADLOCK_LINE: &str = "no suites identified; leaving batch open";
/// The landing line that is a failed sort.
pub const SORT_FAILED_LINE: &str = "queue_sort_rows: ranking failed";
/// The landing line every completed pass writes.
pub const PASS_COMPLETE_LINE: &str = "landing: pass complete";

/// The ten registered classes, in the order a pass reports them; `starved` has no detector.
pub const CLASSES: &[&str] = &["deadlock", "attribution-failed", "sort-failed", "loop-stalled", "ci-stalled", "ci-red", "base-red", "pool-idle", "reconciler-stale", "starved"];

/// `ejected 0, requeued N` with N > 0 on any line: attribution isolated nobody.
pub fn requeued_all(lines: &str) -> bool {
    lines.lines().any(|l| match l.find("ejected 0, requeued ") {
        Some(pos) => l[pos + "ejected 0, requeued ".len()..].chars().next().is_some_and(|c| c.is_ascii_digit() && c != '0'),
        None => false,
    })
}

fn gap(desired: impl Into<String>, observed: impl Into<String>, since_hint: Option<u64>) -> RawStatus {
    RawStatus::Gap { desired: desired.into(), observed: observed.into(), since_hint }
}

pub fn deadlock(seen: bool) -> RawStatus {
    if seen { gap("batch closed or requeued", DEADLOCK_LINE, None) } else { RawStatus::Satisfied }
}

pub fn attribution_failed(seen: bool) -> RawStatus {
    if seen { gap("attribution ejects the offender", "ejected 0, requeued the whole batch", None) } else { RawStatus::Satisfied }
}

pub fn sort_failed(seen: bool) -> RawStatus {
    if seen { gap("queue sorted by priority", SORT_FAILED_LINE, None) } else { RawStatus::Satisfied }
}

/// No pass yet is a fresh install, not a stall.
pub fn loop_stalled(now: u64, last_pass: Option<u64>, stall_secs: u64) -> RawStatus {
    match last_pass {
        None => RawStatus::Satisfied,
        Some(at) => {
            let age = now.saturating_sub(at);
            if age > stall_secs { gap(format!("a landing pass within {stall_secs}s"), format!("last pass {age}s ago"), Some(at)) } else { RawStatus::Satisfied }
        }
    }
}

/// A batch's CI job queued with no runner past `max_secs`.
pub fn ci_stalled(now: u64, repo: &str, queued_since: Option<u64>, max_secs: u64) -> RawStatus {
    match queued_since {
        Some(qs) if now.saturating_sub(qs) > max_secs => gap(format!("CI job picked up within {max_secs}s"), format!("{repo}: queued {}s", now.saturating_sub(qs)), Some(qs)),
        _ => RawStatus::Satisfied,
    }
}

/// A batch's CI run completed red and the verdict has not acted past `max_secs`.
pub fn ci_red(now: u64, repo: &str, conclusion: Option<&str>, completed: Option<u64>, max_secs: u64) -> RawStatus {
    match (conclusion, completed) {
        (Some("failure"), Some(done)) if now.saturating_sub(done) > max_secs => gap("verdict acts on a red run", format!("{repo}: red for {}s", now.saturating_sub(done)), Some(done)),
        _ => RawStatus::Satisfied,
    }
}

/// The base ref's own gate run completed red; fires on the first red pass.
pub fn base_red(repo: &str, base: &str, conclusion: Option<&str>) -> RawStatus {
    if conclusion == Some("failure") { gap(format!("{repo}'s base gate is green"), format!("{repo}'s base ({base}) gate run completed red"), None) } else { RawStatus::Satisfied }
}

/// A certified pool at the round threshold with no batch open.
pub fn pool_idle(repo: &str, batch_open: bool, depth: u64, min_n: u64) -> RawStatus {
    if !batch_open && depth >= min_n {
        gap(format!("{repo}'s certified pool cuts a round at {min_n}"), format!("{repo}'s certified pool sits at {depth} with no batch open"), None)
    } else {
        RawStatus::Satisfied
    }
}

/// No reconciler pass recorded is a gap, not a fresh install: the reconciler always runs.
pub fn reconciler_stale(now: u64, last_pass: Option<u64>, grace_secs: u64) -> RawStatus {
    match last_pass {
        None => gap(format!("a reconciler pass within {grace_secs}s"), "no reconciler pass recorded", None),
        Some(at) if now.saturating_sub(at) > grace_secs => gap(format!("a reconciler pass within {grace_secs}s"), format!("last pass {}s ago", now.saturating_sub(at)), Some(at)),
        Some(_) => RawStatus::Satisfied,
    }
}

/// The red suites as one ref fragment: sorted, deduped, comma-joined; `unknown` when none.
pub fn suite_key(suites: &[String]) -> String {
    let mut s = suites.to_vec();
    s.sort();
    s.dedup();
    if s.is_empty() { "unknown".into() } else { s.join(",") }
}

/// A repository name as a key fragment.
pub fn repo_safe(repo: &str) -> String {
    repo.replace(',', "-")
}

/// Per-repo verdicts rolled up into the one per-class verdict: a gap past its own grace
/// outranks one inside it, which outranks "could not tell", which outranks satisfied.
pub fn rollup(verdicts: &[Verdict]) -> Verdict {
    if let Some(v) = verdicts.iter().find(|v| v.is_gap) {
        return v.clone();
    }
    if let Some(v) = verdicts.iter().find(|v| !matches!(v.status, RawStatus::Satisfied)) {
        return v.clone();
    }
    Verdict { status: RawStatus::Satisfied, since: None, is_gap: false, just_closed: verdicts.iter().any(|v| v.just_closed), remedy_failed: false }
}

/// The shadow stage's line: what the act stage would have done.
pub fn would_line(now_iso: &str, class: &str, kind: &str, desc: &str) -> String {
    format!("{now_iso} CZAR-WOULD: {class} {kind} — {desc}")
}

/// The inference filing of each line class: (ref, subject).
pub fn deadlock_filing() -> (&'static str, &'static str) {
    ("deadlock-batch-open", "QUEUE: batch open — no suites identified (DEADLOCK)")
}

pub fn attribution_filing() -> (&'static str, &'static str) {
    ("attribution-failed-requeue", "QUEUE: attribution ejected 0, requeued whole batch")
}

pub fn sort_filing() -> (&'static str, &'static str) {
    ("sort-failed-ranking", "QUEUE: queue_sort_rows ranking failed")
}

/// One inference filing: an incident bead deduplicated on `reference`.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Filing {
    pub subject: String,
    pub body: String,
    /// `incident:queue-<ref>` or `incident:base-red:<repo>:<suites>`.
    pub reference: String,
    pub cause: String,
    pub labels: String,
    pub kind: String,
    pub priority: String,
    pub repo: String,
    pub express: bool,
}

impl Filing {
    /// A queue-health filing: P1 task against the harness, under the czar's labels.
    pub fn queue(class: &str, reference: &str, subject: &str, body: &str, labels: &str) -> Filing {
        Filing { subject: subject.into(), body: body.into(), reference: format!("incident:queue-{reference}"), cause: class.into(), labels: labels.into(), kind: "task".into(), priority: "1".into(), repo: "spira".into(), express: false }
    }

    /// A red base: P0, express, a bug against the repository that is red.
    pub fn base_red(reference: &str, subject: &str, body: &str, repo: &str, labels: &str) -> Filing {
        Filing { subject: subject.into(), body: body.into(), reference: format!("incident:base-red:{reference}"), cause: "base-red".into(), labels: labels.into(), kind: "bug".into(), priority: "0".into(), repo: repo.into(), express: true }
    }
}

/// Files `f` through the incident library in this process: spooled, then drained into the
/// store, deduplicated on its reference. `Err` when it was not filed (it may stay spooled).
pub fn file(f: &Filing) -> Result<String, String> {
    let mut env = incident::intake::Env::load();
    if env.db.is_none() {
        return Err("SPIRA_DB is not set: no store to file into".into());
    }
    env.labels = incident::decide::incident_labels(Some(&f.labels), None, "incident");
    env.kind = f.kind.clone();
    env.priority = f.priority.clone();
    env.actor = Some("czar".into());
    env.fenrir_exempt = true;
    env.repo = Some(f.repo.clone()).filter(|r| !r.is_empty());
    env.cause = incident::decide::sanitize_cause(&f.cause);
    env.express = f.express;
    let bd = incident::real::RealBd::from_env()?;
    let entry = incident::spool::SpoolEntry { reference: f.reference.clone(), title: f.subject.clone(), unit: env.unit.clone(), incident_path: env.path.clone(), cause: env.cause.clone(), repo: env.repo.clone(), body: f.body.clone().into_bytes() };
    std::fs::create_dir_all(&env.spool_dir).map_err(|e| format!("{}: {e}", env.spool_dir.display()))?;
    let path = incident::spool::entry_path(&env.spool_dir, &incident::intake::spool_stamp(), &f.reference, std::process::id());
    std::fs::write(&path, incident::spool::format_entry(&entry)).map_err(|e| format!("{}: {e}", path.display()))?;
    match incident::intake::drain_one(&env, &bd, &incident::real::RealMailer, &incident::real::RealClock, &path) {
        Ok(id) => Ok(id.unwrap_or_else(|| f.reference.clone())),
        Err(()) => Err(format!("{} not filed; it stays spooled at {}", f.reference, path.display())),
    }
}

/// `forge batch-ci-status <repo> <branch>`'s `key: value` lines, through the forge library.
pub fn batch_ci_status(forge: &str, repo_path: &str, branch: &str) -> Result<String, String> {
    let r = forge::client::run(&forge::client::route_of(std::path::Path::new(forge)), "batch-ci-status", std::path::Path::new(repo_path), &[branch], None);
    if r.ok() { Ok(r.out) } else { Err(format!("forge exited {}", r.code)) }
}

/// Re-dispatches a workflow run through the forge library (`gh` is its only child).
pub fn rerun_workflow(forge: &str, repo_path: &str, run_id: &str) -> Result<String, String> {
    let r = forge::client::run(&forge::client::route_of(std::path::Path::new(forge)), "workflow-rerun", std::path::Path::new(repo_path), &[run_id], None);
    if r.ok() { Ok(r.out) } else { Err(format!("forge workflow-rerun exited {}: {}", r.code, r.err.trim())) }
}

// ── Time and the forge's key: value lines ──────────────────────────────────────

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian (Howard Hinnant's civil_from_days).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// (year, month, day) → days since 1970-01-01 (Hinnant's days_from_civil).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = i64::from(if m > 2 { m - 3 } else { m + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `date -u +%Y-%m-%dT%H:%M:%SZ` of an epoch second.
pub fn iso_of(epoch: u64) -> String {
    let (days, rem) = ((epoch / 86_400) as i64, epoch % 86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

pub fn parse_field(output: &str, key: &str) -> Option<String> {
    let prefix = format!("{}: ", key);
    output
        .lines()
        .find(|l| l.starts_with(&prefix))
        .map(|l| l[prefix.len()..].trim().to_string())
}

/// A UTC `YYYY-MM-DDTHH:MM:SS[Z]` stamp (the landing log's own, `T` or a space between) as
/// epoch seconds; `None` for anything else, which the caller reads as unobservable.
pub fn parse_iso_to_epoch(ts: &str) -> Option<u64> {
    let ts = ts.trim();
    let ts = ts.strip_suffix('Z').unwrap_or(ts);
    let b = ts.as_bytes();
    if b.len() != 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<u32> { ts.get(r).filter(|s| s.bytes().all(|c| c.is_ascii_digit()))?.parse().ok() };
    let (y, mo, d, h, mi, s) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || d == 0 || d > 31 || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let days = days_from_civil(i64::from(y), mo, d);
    if civil_from_days(days) != (i64::from(y), mo, d) {
        return None;
    }
    u64::try_from(days * 86_400 + i64::from(h * 3600 + mi * 60 + s)).ok()
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_requeue_of_zero_is_not_a_failed_attribution() {
        assert!(requeued_all("x ejected 0, requeued 3 y"));
        assert!(!requeued_all("x ejected 0, requeued 0"));
        assert!(!requeued_all("x ejected 1, requeued 3"));
    }

    #[test]
    fn thresholds_are_strict_and_absent_inputs_read_as_their_own_case() {
        assert_eq!(loop_stalled(100, None, 10), RawStatus::Satisfied);
        assert_eq!(loop_stalled(100, Some(90), 10), RawStatus::Satisfied);
        assert!(matches!(loop_stalled(100, Some(89), 10), RawStatus::Gap { since_hint: Some(89), .. }));
        assert!(matches!(reconciler_stale(100, None, 10), RawStatus::Gap { since_hint: None, .. }), "no pass is a gap");
        assert!(matches!(ci_red(100, "r", Some("failure"), Some(10), 10), RawStatus::Gap { .. }));
        assert_eq!(ci_red(100, "r", Some("success"), Some(10), 10), RawStatus::Satisfied);
        assert!(matches!(pool_idle("r", false, 2, 2), RawStatus::Gap { .. }));
        assert_eq!(pool_idle("r", true, 9, 2), RawStatus::Satisfied);
    }

    #[test]
    fn the_suite_key_is_sorted_and_unknown_when_empty() {
        assert_eq!(suite_key(&["lint".into(), "a".into(), "lint".into()]), "a,lint");
        assert_eq!(suite_key(&[]), "unknown");
    }
}
