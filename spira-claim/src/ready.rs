//! `READY_ARGS` / `ready_raw_args` / `ready_shared_exclude` / `rune_exclude` /
//! `bulk_ready_by_rune`'s bucketing (the former ready-bucket script) — lib.sh family F (wave 4
//! decomposition row F, sp-obhv6). Pure argv-building and predicate logic lives here, with
//! no I/O, so it is unit-testable with no database and no runes directory. The store
//! reads (the machine's claimable set) are `main.rs::machine_claimable` over `Store`;
//! the runes reads (`rune_get`, `spira_runes`) are `spira-config`'s own `runes`
//! module; the CLI wiring (env, flags, `SPIRA_HOME`) is `main.rs`.
//!
//! `epic_parent_lookup`/`epic_rank_rows` are not here: the epic functions already exist as
//! `select`/`epics` (DESIGN.md §2) — this bead only adds their lib.sh shims.

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
        .filter(|r| matches!(r.state, BeadState::Ready | BeadState::Rework) && r.snoozed_until.is_none() && r.holds.iter().all(|h| *h == HoldKind::Wait))
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

pub use spira_config::runes::{rune_exclude, shared_exclude};

/// One rune's `(name, RUNE_LABELS, RUNE_EXCLUDE_LABELS)` row for [`bucket`] — a rune
/// whose `RUNE_LABELS` is empty is never added to this list at all (bulk_ready_by_rune's
/// own `[ -n "$inc" ] || continue`), not merely counted as zero.
pub use spira_config::runes::{split_csv, Partition as RunePart};

/// `bulk_ready_by_rune`'s bucketing (one ready-set fetch, counted per rune), in the input
/// `parts` order. `queue_wait` is the shared exclusion; open-children is deliberately not.
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
pub fn is_deferred(row: &ReadyRow, now: i64) -> bool {
    row.defer_until.as_deref().and_then(rfc3339_epoch).is_some_and(|t| t > now)
}


/// `bulk-ready-by-rune`'s counts, over rows whose taking runes the claim view named: a bead that
/// carries no `rune:` preference counts for the first rune that takes it only.
pub fn bucket_taken(rows: &[ReadyRow], taken: &HashMap<String, Vec<String>>, parts: &[RunePart]) -> Vec<(String, u64)> {
    let mut counts: Vec<(String, u64)> = parts.iter().map(|p| (p.name.clone(), 0)).collect();
    let now = now_epoch();
    for row in rows.iter().filter(|r| !is_deferred(r, now)) {
        let Some(runes) = taken.get(&row.id) else { continue };
        let has_pref = row.labels.iter().any(|l| l.starts_with("rune:"));
        for (i, p) in parts.iter().enumerate() {
            if runes.contains(&p.name) {
                counts[i].1 += 1;
                if !has_pref {
                    break;
                }
            }
        }
    }
    counts
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

pub(crate) fn now_epoch() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bucket(rows: &[ReadyRow], parts: &[RunePart], queue_wait: &str) -> Vec<(String, u64)> {
        let taken = rows.iter().map(|r| (r.id.clone(), spira_config::runes::taking(&r.labels, parts, queue_wait))).collect();
        bucket_taken(rows, &taken, parts)
    }

    fn lc_row(id: &str, state: BeadState) -> (String, LifecycleRow) {
        (id.to_string(), LifecycleRow { taken_over: false, bead_id: id.to_string(), state, holds: Default::default(), stack_depth: 0, tip: None, snoozed_until: None, stack_conflict: false, known_red: false, stack: Default::default(), reason: None })
    }

    /// NEXT's ready set takes no bd status: a LANDED bead is out however bd's row reads,
    /// and a READY bead is in however bd's row reads (the function has no bd input).
    #[test_id(tb18go6)]
    #[test]
    fn the_ready_set_is_lifecycle_state_alone() {
        let lc: HashMap<_, _> = [lc_row("sp-landed", BeadState::Landed), lc_row("sp-ready", BeadState::Ready), lc_row("sp-rework", BeadState::Rework)].into();
        assert_eq!(lifecycle_ready_ids(&lc), vec!["sp-ready".to_string(), "sp-rework".to_string()]);
    }

    #[test_id(t2pg5ky)]
    #[test]
    fn ready_args_appends_label_then_exclude_label_in_order() {
        assert_eq!(
            ready_args("plan", "no-loop"), // literal-ok: fixture value, SPIRA_NO_LOOP_LABEL's own default
            vec!["ready", "--limit", "0", "--exclude-type", "epic,event", "-u", "--label", "plan", "--exclude-label", "no-loop"] // literal-ok: ditto
        );
        assert_eq!(ready_args("", ""), vec!["ready", "--limit", "0", "--exclude-type", "epic,event", "-u"]);
    }

    #[test_id(trcl9vm)]
    #[test]
    fn ready_raw_args_never_carries_the_scope_label() {
        assert_eq!(
            ready_raw_args("no-loop"), // literal-ok: fixture value, SPIRA_NO_LOOP_LABEL's own default
            vec!["ready", "--limit", "0", "--exclude-type", "epic,event", "-u", "--exclude-label", "no-loop"] // literal-ok: ditto
        );
    }

    #[test_id(t7cod0s)]
    #[test]
    fn rune_exclude_adds_own_then_shared_then_every_other_rune() {
        let roster = vec!["builder".to_string(), "ops".to_string(), "qa".to_string()];
        assert_eq!(rune_exclude("builder", "qa-proposed", &roster, "spira-queue-waiting"), "qa-proposed,spira-queue-waiting,rune:ops,rune:qa");
        // G16: never excludes its OWN rune: label, and composes with no own-exclusion.
        assert_eq!(rune_exclude("ops", "", &roster, ""), "rune:builder,rune:qa");
    }

    #[test_id(tgq3o45)]
    #[test]
    fn shared_exclude_drops_empty_labels() {
        assert_eq!(shared_exclude("spira-queue-waiting", "spira-open-children"), "spira-queue-waiting,spira-open-children");
        assert_eq!(shared_exclude("", ""), "");
    }

    fn row(id: &str, labels: &[&str]) -> ReadyRow {
        ReadyRow { id: id.into(), labels: labels.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    fn part(name: &str, inc: &[&str], exc: &[&str]) -> RunePart {
        RunePart { name: name.into(), inc: inc.iter().map(|s| s.to_string()).collect(), exc: exc.iter().map(|s| s.to_string()).collect() }
    }

    // covers: UC-dwarf-execution-21
    #[test_id(tb6n35c)]
    #[test]
    fn an_epic_and_a_no_loop_bead_are_outside_the_scope_the_poison_pass_examines() {
        let skip = "keep-out"; // literal-ok: test fixture
        let typed = |id: &str, kind: &str, labels: &[&str]| ReadyRow { issue_type: Some(kind.into()), ..row(id, labels) };
        assert!(in_scope(&typed("t", "task", &["plan"]), "plan", skip), "positive control");
        assert!(!in_scope(&typed("e", "epic", &["plan"]), "plan", skip));
        assert!(!in_scope(&typed("n", "task", &["plan", skip]), "plan", skip));
    }

    #[test_id(t0j3s8v)]
    #[test]
    fn bucket_counts_by_label_superset_and_disjoint_exclude() {
        let rows = vec![
            row("a", &["spira", "plan"]),
            row("b", &["spira", "plan", "spira-queue-waiting"]), // shared-excluded
            row("c", &["spira", "ops-trigger"]),
            row("d", &["spira", "plan", "rune:ops"]), // preference names ops only
        ];
        let parts = vec![part("builder", &["spira", "plan"], &[]), part("ops", &["spira", "ops-trigger"], &[])];
        let counts = bucket(&rows, &parts, "spira-queue-waiting");
        assert_eq!(counts, vec![("builder".to_string(), 1), ("ops".to_string(), 1)]);
    }

    #[test_id(ta2udr6)]
    #[test]
    fn a_stale_submitted_label_does_not_hide_a_lifecycle_ready_bead() {
        let rows = vec![row("a", &["spira", "plan", "spira-submitted"])];
        let parts = vec![part("builder", &["spira", "plan"], &[])];
        assert_eq!(bucket(&rows, &parts, "q"), vec![("builder".to_string(), 1)]);
        assert_eq!(shared_exclude("q", "oc"), "q,oc");
    }

    #[test_id(tg1owct)]
    #[test]
    fn a_base_red_incident_without_plan_is_in_no_builder_bucket() {
        let rows = vec![row("sp-incident", &["spira", "repo:spira"])];
        let parts = vec![part("builder", &["spira", "plan"], &[])];
        assert_eq!(bucket(&rows, &parts, ""), vec![("builder".to_string(), 0)]);
    }

    #[test_id(tjb8kbf)]
    #[test]
    fn bucket_counts_an_unlabeled_bead_in_exactly_one_bucket() {
        let rows = vec![row("a", &[])];
        let parts = vec![part("builder", &[], &[]), part("ops", &[], &[])];
        let counts = bucket(&rows, &parts, "q");
        assert_eq!(counts.iter().map(|c| c.1).sum::<u64>(), 1);
    }

    #[test_id(tu4yzcp)]
    #[test]
    fn bucket_lets_a_bead_count_for_every_named_preference() {
        // Two rune: preferences on one bead count for BOTH buckets — the discrepancy
        // against rune_exclude's OR-matched exclude-label this module's doc calls out.
        let rows = vec![row("a", &["spira", "plan", "rune:builder", "rune:ops"])];
        let parts = vec![part("builder", &["spira", "plan"], &[]), part("ops", &["spira", "plan"], &[])];
        let counts = bucket(&rows, &parts, "");
        assert_eq!(counts, vec![("builder".to_string(), 1), ("ops".to_string(), 1)]);
    }

    #[test_id(tnojdu8)]
    #[test]
    fn bucket_excludes_a_bead_with_no_matching_preference() {
        let rows = vec![row("a", &["spira", "plan", "rune:qa"])];
        let parts = vec![part("builder", &["spira", "plan"], &[])];
        assert_eq!(bucket(&rows, &parts, ""), vec![("builder".to_string(), 0)]);
    }

    #[test_id(tvp67bp)]
    #[test]
    fn bucket_excludes_a_future_deferred_bead_but_counts_a_past_one() {
        let mut held = row("a", &["spira", "plan"]);
        held.defer_until = Some("2999-01-01T00:00:00Z".into());
        let mut past = row("b", &["spira", "plan"]);
        past.defer_until = Some("2000-01-01T00:00:00-07:00".into());
        let parts = vec![part("builder", &["spira", "plan"], &[])];
        assert_eq!(bucket(&[held, past], &parts, ""), vec![("builder".to_string(), 1)]);
        assert_eq!(rfc3339_epoch("1970-01-02T00:00:00Z"), Some(86400));
    }

    fn shipped_partition(rune: &str) -> RunePart {
        let file = spira_config::resolve::locate_home_for_process().unwrap().join(format!("runes/{rune}.rune"));
        let text = std::fs::read_to_string(&file).unwrap();
        let lookup = |n: &str| match n {
            "SPIRA_SCOPE_LABEL" => Some("scope-x".to_string()),
            "SPIRA_CZAR_LABEL" => Some("czar-x".to_string()),
            "SPIRA_PLAN_LABEL" => Some("plan-x".to_string()),
            "SPIRA_CI_LABEL" => Some("ci-x".to_string()),
            _ => None,
        };
        let vars = spira_config::rune_file::parse(file.to_str().unwrap(), &text, &lookup).unwrap();
        part(rune, &split_csv(&vars["RUNE_LABELS"]).iter().map(String::as_str).collect::<Vec<_>>(), &split_csv(&vars["RUNE_EXCLUDE_LABELS"]).iter().map(String::as_str).collect::<Vec<_>>())
    }

    // covers: UC-dispatch-09 UC-config-store-preflight-15
    #[test_id(t6wkzl5)]
    #[test]
    fn the_shipped_czar_and_builder_partitions_each_take_their_own_bead_and_never_the_others() {
        let czar = shipped_partition("czar");
        let builder = shipped_partition("builder");
        let trigger = row("czq", &["scope-x", "czar-x"]);
        let plan = row("pl", &["scope-x", "plan-x"]);
        let rows = [trigger, plan];
        let ids = |p: &RunePart| matching(&rows, &p.inc, &p.exc).iter().map(|r| r.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&czar), vec!["czq"], "the czar takes the queue-state escalation and not plan work");
        assert_eq!(ids(&builder), vec!["pl"], "the builder takes plan work and not the czar's bead");
        assert!(czar.inc.iter().all(|l| !builder.inc.contains(l) || l == "scope-x"), "only the scope label is shared: {:?} vs {:?}", czar.inc, builder.inc);
    }
}
