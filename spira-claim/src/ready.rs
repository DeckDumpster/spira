//! `READY_ARGS` / `ready_raw_args` / `ready_shared_exclude` / `fayth_exclude` /
//! `bulk_ready_by_fayth`'s bucketing (`ready-bucket.py`) — lib.sh family F (wave 4
//! decomposition row F, sp-obhv6). Pure argv-building and predicate logic lives here, with
//! no I/O, so it is unit-testable with no database and no chamber directory. The store
//! reads (the machine's claimable set) are `main.rs::machine_claimable` over `Store`;
//! the chamber reads (`fayth_get`, `spira_fayths`) are `spira-config`'s own `chamber`
//! module; the CLI wiring (env, flags, `SPIRA_HOME`) is `main.rs`.
//!
//! `claim_retry` and `epic_parent_lookup`/`epic_rank_rows` are not here: `claim_retry` is a
//! thin bd-retry loop with no predicate of its own (`main.rs::cmd_claim_retry`, next to
//! `Store::bd`); the epic functions already exist as `select`/`epics` (DESIGN.md §2) — this
//! bead only adds their lib.sh shims.

use std::collections::HashMap;

use crate::rank::{LifecycleRow, ReadyRow};
use lifecycle::bead::{BeadState, HoldKind};
use spira_claim::READY_ARGS_BASE;

/// `ready_raw_args`: READY_ARGS without the `SPIRA_SCOPE_LABEL` restriction, one argv
/// token at a time (lib.sh:438) — the broadest "is this bead claimable by ANY persona"
/// query.
pub fn ready_raw_args(no_loop_label: &str) -> Vec<String> {
    let mut v: Vec<String> = READY_ARGS_BASE.iter().map(|s| s.to_string()).collect();
    if !no_loop_label.is_empty() {
        v.push("--exclude-label".into());
        v.push(no_loop_label.to_string());
    }
    v
}

/// The lifecycle half of "ready": the READY and REWORK rows carrying no hold but `wait`,
/// which [`rank::claimable`] judges through the bead's blockers. bd's status and assignee
/// are never read: the row is the claim.
pub fn lifecycle_ready_ids(lc: &HashMap<String, LifecycleRow>) -> Vec<String> {
    let mut ids: Vec<String> = lc
        .values()
        .filter(|r| matches!(r.state, BeadState::Ready | BeadState::Rework) && r.holds.iter().all(|h| *h == HoldKind::Wait))
        .map(|r| r.bead_id.clone())
        .collect();
    ids.sort();
    ids
}

/// The bd-content half: a work type, inside every scope label, carrying no no-loop label.
pub fn in_scope(r: &ReadyRow, scope_label: &str, no_loop_label: &str) -> bool {
    !matches!(r.issue_type.as_deref(), Some("epic") | Some("event"))
        && labels_match(r, &split_csv(scope_label), &split_csv(no_loop_label))
}

/// bd's own `--label` / `--exclude-label` reading: every `inc` label present, no `exc` one.
pub fn labels_match(r: &ReadyRow, inc: &[String], exc: &[String]) -> bool {
    inc.iter().all(|l| r.labels.contains(l)) && !exc.iter().any(|l| r.labels.contains(l))
}

/// `READY_ARGS` itself (lib.sh:434): the base five tokens, `--label <scope>` when a scope
/// restriction is set, then `--exclude-label <no-loop>` when the no-loop label is set — in
/// that order, matching the bash array's own append sequence.
pub fn ready_args(scope_label: &str, no_loop_label: &str) -> Vec<String> {
    let mut v: Vec<String> = READY_ARGS_BASE.iter().map(|s| s.to_string()).collect();
    if !scope_label.is_empty() {
        v.push("--label".into());
        v.push(scope_label.to_string());
    }
    if !no_loop_label.is_empty() {
        v.push("--exclude-label".into());
        v.push(no_loop_label.to_string());
    }
    v
}

/// `ready_shared_exclude`'s three labels (lib.sh:652), folded into [`fayth_exclude`] — its
/// only caller — rather than kept as a separately shimmed function (zero other callers).
pub fn shared_exclude3(queue_wait: &str, submitted: &str, open_children: &str) -> String {
    [queue_wait, submitted, open_children].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(",")
}

/// `fayth_exclude <fayth> [own]` (lib.sh:666): the persona's own exclusions, the shared
/// exclusion set, then every OTHER persona's `fayth:<name>` claim — in that order, matching
/// the bash's own append sequence (own, then shared, then the roster loop).
pub fn fayth_exclude(me: &str, own: &str, roster: &[String], shared: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !own.is_empty() {
        parts.push(own.to_string());
    }
    if !shared.is_empty() {
        parts.push(shared.to_string());
    }
    for f in roster {
        if f != me {
            parts.push(format!("fayth:{f}"));
        }
    }
    parts.join(",")
}

/// One fayth's `(name, FAYTH_LABELS, FAYTH_EXCLUDE_LABELS)` row for [`bucket`] — a fayth
/// whose `FAYTH_LABELS` is empty is never added to this list at all (bulk_ready_by_fayth's
/// own `[ -n "$inc" ] || continue`), not merely counted as zero.
pub struct FaythPart {
    pub name: String,
    pub inc: Vec<String>,
    pub exc: Vec<String>,
}

/// `ready-bucket.py`'s own split: comma-separated, empty tokens dropped.
pub fn split_csv(s: &str) -> Vec<String> {
    s.split(',').map(str::trim).filter(|t| !t.is_empty()).map(str::to_string).collect()
}

/// `ready-bucket.py`'s predicate, exactly: a bead counts for `(inc, exc)` iff none of its
/// labels is in `shared`, its `fayth:` preferences (if it carries any) include `name`,
/// `inc` is a subset of its labels, and `exc` is disjoint from them.
///
/// DELIBERATELY NOT `fayth_exclude`'s own exclude-label semantics: a bead may carry more
/// than one `fayth:<name>` preference, and this (membership) lets it count for every one of
/// them, while `fayth_exclude`'s bd query (OR-matched `--exclude-label`) would exclude it
/// from every partition but the first it names. Both are faithful ports of their own
/// bash/python originals — this discrepancy predates this port and is not this bead's to
/// resolve.
fn bucket_match(labels: &[&str], inc: &[String], exc: &[String], shared: &[&str], name: &str) -> bool {
    if labels.iter().any(|l| shared.contains(l)) {
        return false;
    }
    let mut has_pref = false;
    let mut pref_matches = false;
    for l in labels {
        if let Some(p) = l.strip_prefix("fayth:") {
            has_pref = true;
            if p == name {
                pref_matches = true;
            }
        }
    }
    if has_pref && !pref_matches {
        return false;
    }
    if !inc.iter().all(|want| labels.contains(&want.as_str())) {
        return false;
    }
    if exc.iter().any(|drop| labels.contains(&drop.as_str())) {
        return false;
    }
    true
}

/// `bulk_ready_by_fayth`'s bucketing (one ready-set fetch, counted per fayth), in the input
/// `parts` order. `queue_wait`/`submitted` are `ready-bucket.py`'s own two-label shared
/// exclusion — deliberately NOT `shared_exclude3`'s three (the python's own comment calls
/// out "shared QUEUE_WAIT/SUBMITTED exclusion"; `SPIRA_OPEN_CHILDREN_LABEL` is not one of
/// them, and this port keeps that exactly).
/// RFC3339 `YYYY-MM-DDTHH:MM:SS[.f][Z|±hh:mm]` to epoch seconds; None if unparseable.
fn rfc3339_epoch(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let n = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (n(0, 4)?, n(5, 7)?, n(8, 10)?, n(11, 13)?, n(14, 16)?, n(17, 19)?);
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let mut t = days * 86400 + h * 3600 + mi * 60 + se;
    let rest = s[19..].trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    if rest.len() >= 6 && (rest.starts_with('+') || rest.starts_with('-')) {
        let off = rest[1..3].parse::<i64>().ok()? * 3600 + rest[4..6].parse::<i64>().ok()? * 60;
        t -= if rest.starts_with('+') { off } else { -off };
    }
    Some(t)
}

/// A bead whose `defer_until` is still in the future is held, not ready, in every bucket.
fn is_deferred(row: &ReadyRow, now: i64) -> bool {
    row.defer_until.as_deref().and_then(rfc3339_epoch).is_some_and(|t| t > now)
}

pub fn bucket(rows: &[ReadyRow], parts: &[FaythPart], queue_wait: &str, submitted: &str) -> Vec<(String, u64)> {
    let shared: Vec<&str> = [queue_wait, submitted].into_iter().filter(|s| !s.is_empty()).collect();
    let mut counts: Vec<(String, u64)> = parts.iter().map(|p| (p.name.clone(), 0)).collect();
    let now = now_epoch();
    for row in rows {
        if is_deferred(row, now) {
            continue;
        }
        let labels: Vec<&str> = row.labels.iter().map(String::as_str).collect();
        let has_pref = labels.iter().any(|l| l.starts_with("fayth:"));
        for (i, p) in parts.iter().enumerate() {
            if bucket_match(&labels, &p.inc, &p.exc, &shared, &p.name) {
                counts[i].1 += 1;
                if !has_pref {
                    break; // unlabeled bead counts in exactly one (first) bucket
                }
            }
        }
    }
    counts
}

/// One fayth's own rows by [`bucket`]'s predicate: what `fayth-ready` counts is exactly
/// what `fayth-ready --json` hands an aeon to claim from.
pub fn partition<'a>(rows: &'a [ReadyRow], part: &FaythPart, queue_wait: &str, submitted: &str) -> Vec<&'a ReadyRow> {
    let shared: Vec<&str> = [queue_wait, submitted].into_iter().filter(|s| !s.is_empty()).collect();
    let now = now_epoch();
    rows.iter()
        .filter(|row| !is_deferred(row, now))
        .filter(|row| {
            let labels: Vec<&str> = row.labels.iter().map(String::as_str).collect();
            bucket_match(&labels, &part.inc, &part.exc, &shared, &part.name)
        })
        .collect()
}

/// `ready-count`'s rows under the machine: bd's label predicate over the claimable set, with
/// a deferred bead held as `bd ready` holds it.
pub fn count_matching(rows: &[ReadyRow], inc: &[String], exc: &[String]) -> u64 {
    matching(rows, inc, exc).len() as u64
}

/// The rows [`count_matching`] counts: `ready-count --json` hands a reader the set itself.
pub fn matching<'a>(rows: &'a [ReadyRow], inc: &[String], exc: &[String]) -> Vec<&'a ReadyRow> {
    let now = now_epoch();
    rows.iter().filter(|r| !is_deferred(r, now) && labels_match(r, inc, exc)).collect()
}

fn now_epoch() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_args_appends_label_then_exclude_label_in_order() {
        assert_eq!(
            ready_args("plan", "no-loop"), // literal-ok: fixture value, SPIRA_NO_LOOP_LABEL's own default
            vec!["ready", "--limit", "0", "--exclude-type", "epic,event", "-u", "--label", "plan", "--exclude-label", "no-loop"] // literal-ok: ditto
        );
        assert_eq!(ready_args("", ""), vec!["ready", "--limit", "0", "--exclude-type", "epic,event", "-u"]);
    }

    #[test]
    fn ready_raw_args_never_carries_the_scope_label() {
        assert_eq!(
            ready_raw_args("no-loop"), // literal-ok: fixture value, SPIRA_NO_LOOP_LABEL's own default
            vec!["ready", "--limit", "0", "--exclude-type", "epic,event", "-u", "--exclude-label", "no-loop"] // literal-ok: ditto
        );
    }

    #[test]
    fn fayth_exclude_adds_own_then_shared_then_every_other_fayth() {
        let roster = vec!["builder".to_string(), "ops".to_string(), "qa".to_string()];
        assert_eq!(fayth_exclude("builder", "qa-proposed", &roster, "spira-queue-waiting"), "qa-proposed,spira-queue-waiting,fayth:ops,fayth:qa");
        // G16: never excludes its OWN fayth: label, and composes with no own-exclusion.
        assert_eq!(fayth_exclude("ops", "", &roster, ""), "fayth:builder,fayth:qa");
    }

    #[test]
    fn shared_exclude3_drops_empty_labels() {
        assert_eq!(shared_exclude3("spira-queue-waiting", "", "spira-open-children"), "spira-queue-waiting,spira-open-children");
        assert_eq!(shared_exclude3("", "", ""), "");
    }

    fn row(id: &str, labels: &[&str]) -> ReadyRow {
        ReadyRow { id: id.into(), labels: labels.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    fn part(name: &str, inc: &[&str], exc: &[&str]) -> FaythPart {
        FaythPart { name: name.into(), inc: inc.iter().map(|s| s.to_string()).collect(), exc: exc.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn bucket_counts_by_label_superset_and_disjoint_exclude() {
        let rows = vec![
            row("a", &["spira", "plan"]),
            row("b", &["spira", "plan", "spira-queue-waiting"]), // shared-excluded
            row("c", &["spira", "ops-trigger"]),
            row("d", &["spira", "plan", "fayth:ops"]), // preference names ops only
        ];
        let parts = vec![part("builder", &["spira", "plan"], &[]), part("ops", &["spira", "ops-trigger"], &[])];
        let counts = bucket(&rows, &parts, "spira-queue-waiting", "spira-submitted");
        assert_eq!(counts, vec![("builder".to_string(), 1), ("ops".to_string(), 1)]);
    }

    #[test]
    fn bucket_counts_an_unlabeled_bead_in_exactly_one_bucket() {
        let rows = vec![row("a", &[])];
        let parts = vec![part("builder", &[], &[]), part("ops", &[], &[])];
        let counts = bucket(&rows, &parts, "q", "s");
        assert_eq!(counts.iter().map(|c| c.1).sum::<u64>(), 1);
    }

    #[test]
    fn bucket_lets_a_bead_count_for_every_named_preference() {
        // Two fayth: preferences on one bead count for BOTH buckets — the discrepancy
        // against fayth_exclude's OR-matched exclude-label this module's doc calls out.
        let rows = vec![row("a", &["spira", "plan", "fayth:builder", "fayth:ops"])];
        let parts = vec![part("builder", &["spira", "plan"], &[]), part("ops", &["spira", "plan"], &[])];
        let counts = bucket(&rows, &parts, "", "");
        assert_eq!(counts, vec![("builder".to_string(), 1), ("ops".to_string(), 1)]);
    }

    #[test]
    fn bucket_excludes_a_bead_with_no_matching_preference() {
        let rows = vec![row("a", &["spira", "plan", "fayth:qa"])];
        let parts = vec![part("builder", &["spira", "plan"], &[])];
        assert_eq!(bucket(&rows, &parts, "", ""), vec![("builder".to_string(), 0)]);
    }

    #[test]
    fn bucket_excludes_a_future_deferred_bead_but_counts_a_past_one() {
        let mut held = row("a", &["spira", "plan"]);
        held.defer_until = Some("2999-01-01T00:00:00Z".into());
        let mut past = row("b", &["spira", "plan"]);
        past.defer_until = Some("2000-01-01T00:00:00-07:00".into());
        let parts = vec![part("builder", &["spira", "plan"], &[])];
        assert_eq!(bucket(&[held, past], &parts, "", ""), vec![("builder".to_string(), 1)]);
        assert_eq!(rfc3339_epoch("1970-01-02T00:00:00Z"), Some(86400));
    }
}
