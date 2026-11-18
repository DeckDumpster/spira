//! Census's aggregates over the lifecycle facts, computed from `spira-lc fact list` rows. Each
//! report renders as bd's `sql` table text so the one parser in `pipeline` reads both legs.
//! The folds, exclusions, actor filter and since-bound mirror the SQL in `sql.rs`.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The kinds the folds read: the `fact list --kinds` filter.
pub const KINDS: &str = "requeued,reclaimed,recurred,lapsed,reopen";

const BURST_WINDOW_S: i64 = 60;

#[derive(Clone, Debug, PartialEq)]
pub struct FactRow {
    pub event_type: String,
    pub new_value: String,
    pub issue_id: String,
    pub actor: String,
    pub at: i64,
}

fn civil_days(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The epoch second of `YYYY-MM-DDTHH:MM:SSZ`, the `created_at` `fact list` answers.
fn epoch_of(text: &str) -> Option<i64> {
    let (date, time) = text.trim_end_matches('Z').split_once(['T', ' '])?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let mut t = time.split(':').map(|p| p.parse::<i64>().ok());
    let (y, mo, da) = (d.next()??, d.next()??, d.next()??);
    let (h, mi, s) = (t.next()??, t.next()??, t.next()??);
    Some(civil_days(y, mo, da) * 86_400 + h * 3_600 + mi * 60 + s)
}

/// The rows of `fact list` output.
pub fn parse(json: &str) -> Result<Vec<FactRow>, String> {
    let rows: Vec<Value> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    rows.iter()
        .map(|r| {
            let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let at = epoch_of(&s("created_at"))
                .ok_or_else(|| format!("unreadable created_at {:?}", s("created_at")))?;
            Ok(FactRow {
                event_type: s("event_type"),
                new_value: s("new_value"),
                issue_id: s("issue_id"),
                actor: s("actor"),
                at,
            })
        })
        .collect()
}

/// Beads whose reopen cause (a `reopen` fact, or a merge-conflict requeue) is among `rows`.
pub fn recorded_cause_ids(rows: &[FactRow]) -> Vec<String> {
    let ids: BTreeSet<&str> = rows
        .iter()
        .filter(|r| r.event_type == "reopen" || is_merge_conflict(r))
        .map(|r| r.issue_id.as_str())
        .collect();
    ids.into_iter().map(str::to_string).collect()
}

fn is(r: &FactRow, kind: &str, value: &str) -> bool {
    r.event_type == kind && r.new_value == value
}

fn is_merge_conflict(r: &FactRow) -> bool {
    is(r, "requeued", "merge-conflict")
}

fn is_rebase_conflict(r: &FactRow) -> bool {
    is(r, "reopen", "rebase-conflict")
        || is_merge_conflict(r)
        || is(r, "requeued", "rebase-conflict")
}

fn either(value: &'static str) -> impl Fn(&FactRow) -> bool {
    move |r| (r.event_type == "reopen" || r.event_type == "requeued") && r.new_value == value
}

fn harness_actor(r: &FactRow) -> bool {
    r.actor == "harness" || r.actor.to_ascii_lowercase().starts_with("dwarf-")
}

fn is_unjudged_not_charging(r: &FactRow) -> bool {
    r.event_type == "requeued"
        && r.new_value.to_ascii_lowercase().starts_with("unjudged-")
        && r.new_value != format!("unjudged-{}", dwarf::decide::CHARGING_OUTCOME)
}

fn is_deliberate(r: &FactRow, causes: &[String]) -> bool {
    r.event_type == "reopen" && causes.contains(&r.new_value)
}

/// The classes the report counts under a label of their own, whatever the event type that
/// carried them; `desc-changed-since-claim` is left in its own class in the event rows.
type Fold = (&'static str, fn(&FactRow) -> bool);

const FOLDS: [Fold; 5] = [
    ("rebase-conflict", is_rebase_conflict),
    ("eviction-race", |r| either("eviction-race")(r)),
    ("prod-dirty", |r| either("prod-dirty")(r)),
    ("unfinished-reason", |r| either("unfinished-reason")(r)),
    ("desc-changed-since-claim", |r| {
        either("desc-changed-since-claim")(r)
    }),
];

fn main_class(r: &FactRow, causes: &[String], desc_folded: bool) -> bool {
    KINDS.split(',').any(|k| k == r.event_type)
        && !is_merge_conflict(r)
        && !is(r, "reopen", "rebase-conflict")
        && !is(r, "requeued", "rebase-conflict")
        && !either("eviction-race")(r)
        && !either("prod-dirty")(r)
        && !either("unfinished-reason")(r)
        && !(desc_folded && either("desc-changed-since-claim")(r))
        && !is_unjudged_not_charging(r)
        && !(r.event_type == "reopen"
            && (r.new_value == "closed-while-live" || r.new_value == "recurrence"))
        && !is_deliberate(r, causes)
        && harness_actor(r)
}

fn after(rows: &[FactRow], since: Option<i64>) -> impl Iterator<Item = &FactRow> {
    rows.iter().filter(move |r| since.is_none_or(|t| r.at > t))
}

struct Tally {
    beads: usize,
    events: usize,
    bursts: usize,
}

fn tally(rows: &[&FactRow]) -> Tally {
    let mut times: Vec<i64> = rows.iter().map(|r| r.at).collect();
    times.sort_unstable();
    let bursts = times
        .iter()
        .enumerate()
        .filter(|(i, t)| *i == 0 || **t - times[i - 1] > BURST_WINDOW_S)
        .count();
    Tally {
        beads: rows
            .iter()
            .map(|r| r.issue_id.as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        events: rows.len(),
        bursts,
    }
}

fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let sep = format!(
        "+{}+\n",
        headers.iter().map(|_| "---").collect::<Vec<_>>().join("+")
    );
    let line = |cells: Vec<&str>| format!("| {} |\n", cells.join(" | "));
    let mut out = sep.clone();
    out.push_str(&line(headers.to_vec()));
    out.push_str(&sep);
    for r in rows {
        out.push_str(&line(r.iter().map(String::as_str).collect()));
    }
    out.push_str(&sep);
    out
}

fn counted(label: (&str, &str), t: Tally) -> Vec<String> {
    vec![
        label.0.into(),
        label.1.into(),
        t.beads.to_string(),
        t.events.to_string(),
        t.bursts.to_string(),
    ]
}

/// `(event_type, new_value, beads, events, bursts)` per class, most beads first.
pub fn events(rows: &[FactRow], since: Option<i64>, causes: &[String]) -> String {
    let mut groups: BTreeMap<(&str, &str), Vec<&FactRow>> = BTreeMap::new();
    for r in after(rows, since).filter(|r| main_class(r, causes, true)) {
        groups
            .entry((&r.event_type, &r.new_value))
            .or_default()
            .push(r);
    }
    let mut out: Vec<Vec<String>> = groups
        .into_iter()
        .map(|((k, v), g)| counted((k, v), tally(&g)))
        .collect();
    for (value, pred) in FOLDS {
        let g: Vec<&FactRow> = after(rows, since)
            .filter(|r| pred(r) && harness_actor(r))
            .collect();
        if !g.is_empty() {
            out.push(counted(("reopen", value), tally(&g)));
        }
    }
    out.sort_by_key(|r| std::cmp::Reverse(r[2].parse::<usize>().unwrap_or(0)));
    table(
        &[
            "event_type",
            "COALESCE(new_value, '')",
            "beads",
            "events",
            "bursts",
        ],
        &out,
    )
}

/// One `(event_type, new_value, issue_id, epoch)` row per event, for clustering.
pub fn event_rows(rows: &[FactRow], since: Option<i64>, causes: &[String]) -> String {
    let mut out: Vec<(String, String, String, i64)> = after(rows, since)
        .filter(|r| main_class(r, causes, false))
        .map(|r| {
            (
                r.event_type.clone(),
                r.new_value.clone(),
                r.issue_id.clone(),
                r.at,
            )
        })
        .collect();
    for (value, pred) in FOLDS {
        out.extend(
            after(rows, since)
                .filter(|r| pred(r) && harness_actor(r))
                .map(|r| {
                    (
                        "reopen".to_string(),
                        value.to_string(),
                        r.issue_id.clone(),
                        r.at,
                    )
                }),
        );
    }
    out.sort_by(|a, b| (&a.0, &a.1, a.3).cmp(&(&b.0, &b.1, b.3)));
    let cells: Vec<Vec<String>> = out
        .into_iter()
        .map(|(k, v, i, t)| vec![k, v, i, t.to_string()])
        .collect();
    table(&["c1", "c2", "c3", "c4"], &cells)
}

/// `(event_type, new_value, actor, beads, events)` of the events a person wrote, most beads first.
pub fn handwritten(rows: &[FactRow]) -> String {
    let mut groups: BTreeMap<(&str, &str, &str), Vec<&FactRow>> = BTreeMap::new();
    for r in rows.iter().filter(|r| {
        (KINDS.split(',').any(|k| k == r.event_type) || r.event_type == "reopened")
            && !harness_actor(r)
    }) {
        groups
            .entry((&r.event_type, &r.new_value, &r.actor))
            .or_default()
            .push(r);
    }
    let mut out: Vec<(usize, Vec<String>)> = groups
        .into_iter()
        .map(|((k, v, a), g)| {
            let t = tally(&g);
            (
                t.beads,
                vec![
                    k.into(),
                    v.into(),
                    a.into(),
                    t.beads.to_string(),
                    t.events.to_string(),
                ],
            )
        })
        .collect();
    out.sort_by_key(|(beads, _)| std::cmp::Reverse(*beads));
    table(
        &[
            "event_type",
            "COALESCE(new_value, '')",
            "actor",
            "beads",
            "events",
        ],
        &out.into_iter().map(|(_, c)| c).collect::<Vec<_>>(),
    )
}

/// `(event_type, new_value, beads, events)` of the reopens made on purpose, most beads first.
pub fn deliberate(rows: &[FactRow], since: Option<i64>, causes: &[String]) -> String {
    let mut groups: BTreeMap<&str, Vec<&FactRow>> = BTreeMap::new();
    for r in after(rows, since).filter(|r| is_deliberate(r, causes)) {
        groups.entry(&r.new_value).or_default().push(r);
    }
    let mut out: Vec<(usize, Vec<String>)> = groups
        .into_iter()
        .map(|(v, g)| {
            let t = tally(&g);
            (
                t.beads,
                vec![
                    "reopen".into(),
                    v.into(),
                    t.beads.to_string(),
                    t.events.to_string(),
                ],
            )
        })
        .collect();
    out.sort_by_key(|(beads, _)| std::cmp::Reverse(*beads));
    table(
        &["event_type", "COALESCE(new_value, '')", "beads", "events"],
        &out.into_iter().map(|(_, c)| c).collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_700_000_000;
    const SINCE: i64 = T0 + 10;

    fn fixture() -> Vec<FactRow> {
        let row = |issue: &str, kind: &str, value: &str, actor: &str, dt: i64| FactRow {
            event_type: kind.into(),
            new_value: value.into(),
            issue_id: issue.into(),
            actor: actor.into(),
            at: T0 + dt,
        };
        vec![
            row("sp-a", "requeued", "gate-red", "harness", 0),
            row("sp-a", "requeued", "gate-red", "harness", 100),
            row("sp-a", "requeued", "gate-red", "harness", 130),
            row("sp-a", "requeued", "gate-red", "harness", 250),
            row("sp-b", "requeued", "gate-red", "dwarf-3", 100),
            row("sp-b", "requeued", "merge-conflict", "harness", 200),
            row("sp-c", "requeued", "rebase-conflict", "harness", 210),
            row("sp-c", "reopen", "rebase-conflict", "harness", 215),
            row("sp-d", "reopen", "eviction-race", "harness", 300),
            row("sp-d", "requeued", "eviction-race", "harness", 400),
            row("sp-e", "requeued", "prod-dirty", "harness", 500),
            row("sp-e", "reopen", "prod-dirty", "dwarf-1", 510),
            row("sp-f", "requeued", "unfinished-reason", "harness", 600),
            row("sp-f", "reopen", "unfinished-reason", "harness", 700),
            row(
                "sp-g",
                "requeued",
                "desc-changed-since-claim",
                "harness",
                800,
            ),
            row("sp-g", "reopen", "desc-changed-since-claim", "harness", 805),
            row("sp-h", "requeued", "unjudged-killed", "harness", 900),
            row("sp-h", "requeued", "unjudged-unlanded", "harness", 950),
            row("sp-i", "reopen", "eject", "harness", 1000),
            row("sp-i", "reopen", "work-close-converted", "harness", 1100),
            row("sp-j", "reopen", "recurrence", "harness", 1200),
            row("sp-k", "requeued", "gate-red", "operator", 1300),
            row("sp-k", "reclaimed", "lease-expired", "human", 1400),
            row("sp-l", "reopen", "eject", "alice", 1500),
            row("sp-m", "reclaimed", "lease-expired", "harness", 1600),
            row("sp-m", "lapsed", "idle", "dwarf-9", 1700),
        ]
    }

    fn causes() -> Vec<String> {
        vec!["work-close-converted".to_string(), "eject".to_string()]
    }

    // The expected tables are what `spira-lc facts-query` answered for the `sql.rs` builders'
    // text over this fixture, captured before that verb was deleted.
    #[test]
    fn events_equal_what_the_sql_answered() {
        assert_eq!(
            events(&fixture(), Some(SINCE), &causes()),
            concat!(
                "+---+---+---+---+---+\n",
                "| event_type | COALESCE(new_value, '') | beads | events | bursts |\n",
                "+---+---+---+---+---+\n",
                "| requeued | gate-red | 2 | 4 | 2 |\n",
                "| reopen | rebase-conflict | 2 | 3 | 1 |\n",
                "| lapsed | idle | 1 | 1 | 1 |\n",
                "| reclaimed | lease-expired | 1 | 1 | 1 |\n",
                "| requeued | unjudged-unlanded | 1 | 1 | 1 |\n",
                "| reopen | eviction-race | 1 | 2 | 2 |\n",
                "| reopen | prod-dirty | 1 | 2 | 1 |\n",
                "| reopen | unfinished-reason | 1 | 2 | 2 |\n",
                "| reopen | desc-changed-since-claim | 1 | 2 | 1 |\n",
                "+---+---+---+---+---+\n",
            )
        );
    }

    #[test]
    fn event_rows_equal_what_the_sql_answered() {
        assert_eq!(
            event_rows(&fixture(), Some(SINCE), &causes()),
            concat!(
                "+---+---+---+---+\n",
                "| c1 | c2 | c3 | c4 |\n",
                "+---+---+---+---+\n",
                "| lapsed | idle | sp-m | 1700001700 |\n",
                "| reclaimed | lease-expired | sp-m | 1700001600 |\n",
                "| reopen | desc-changed-since-claim | sp-g | 1700000800 |\n",
                "| reopen | desc-changed-since-claim | sp-g | 1700000805 |\n",
                "| reopen | desc-changed-since-claim | sp-g | 1700000805 |\n",
                "| reopen | eviction-race | sp-d | 1700000300 |\n",
                "| reopen | eviction-race | sp-d | 1700000400 |\n",
                "| reopen | prod-dirty | sp-e | 1700000500 |\n",
                "| reopen | prod-dirty | sp-e | 1700000510 |\n",
                "| reopen | rebase-conflict | sp-b | 1700000200 |\n",
                "| reopen | rebase-conflict | sp-c | 1700000210 |\n",
                "| reopen | rebase-conflict | sp-c | 1700000215 |\n",
                "| reopen | unfinished-reason | sp-f | 1700000600 |\n",
                "| reopen | unfinished-reason | sp-f | 1700000700 |\n",
                "| requeued | desc-changed-since-claim | sp-g | 1700000800 |\n",
                "| requeued | gate-red | sp-a | 1700000100 |\n",
                "| requeued | gate-red | sp-b | 1700000100 |\n",
                "| requeued | gate-red | sp-a | 1700000130 |\n",
                "| requeued | gate-red | sp-a | 1700000250 |\n",
                "| requeued | unjudged-unlanded | sp-h | 1700000950 |\n",
                "+---+---+---+---+\n",
            )
        );
    }

    #[test]
    fn handwritten_equals_what_the_sql_answered() {
        assert_eq!(
            handwritten(&fixture()),
            concat!(
                "+---+---+---+---+---+\n",
                "| event_type | COALESCE(new_value, '') | actor | beads | events |\n",
                "+---+---+---+---+---+\n",
                "| reclaimed | lease-expired | human | 1 | 1 |\n",
                "| reopen | eject | alice | 1 | 1 |\n",
                "| requeued | gate-red | operator | 1 | 1 |\n",
                "+---+---+---+---+---+\n",
            )
        );
    }

    #[test]
    fn deliberate_equals_what_the_sql_answered() {
        assert_eq!(
            deliberate(&fixture(), Some(SINCE), &causes()),
            concat!(
                "+---+---+---+---+\n",
                "| event_type | COALESCE(new_value, '') | beads | events |\n",
                "+---+---+---+---+\n",
                "| reopen | eject | 2 | 2 |\n",
                "| reopen | work-close-converted | 1 | 1 |\n",
                "+---+---+---+---+\n",
            )
        );
    }
}
