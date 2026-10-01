//! lib.sh family L — attempt-counter writes and the small bd accessors they share
//! (wave 4.18, wave4-decomposition.md row 18, sp-sn1re). `attempts`/`requeues` need no
//! new code here: DESIGN.md §3's `events::fold` already answers them (sp-j1q6o), and
//! `main.rs`'s `attempts`/`requeues` verbs are what `lib.sh`'s `attempts_of`/`requeues_of`
//! now shim onto. This module is everything else family L still needed written down:
//! the one generic INSERT every `bump_*` lib.sh wrapper shims onto
//! (`_bump_write_event_try`), the raw per-event-type COUNT doctor's health probe and
//! `requeues_of`'s own predecessor used (`_counter_events_query`), `write_lapse_record`,
//! `bead_metadata`, `thrash_streak_bump` and `counter_label`.
//!
//! `poison_asked_clear` is NOT here: it is retired from lib.sh outright. Its one real
//! caller (groomer's work-fault triage, `groomer/src/cmds.rs::triage_poison`) now calls
//! `spira-claim ask-clear` directly — the same `World::clear_ask_history` step
//! `unpoison::one` already performs as step 2 of its own write (unpoison.rs), exposed
//! standalone because triage-poison wants exactly that write and none of unpoison's
//! threshold/label/lifecycle/note machinery around it.

use serde_json::Value;

use crate::store::{self, Store};
use crate::unpoison::bounded_cause;

// =========================================================================================
// write-event: lib.sh's `_bump_write_event_try`, the one INSERT every `bump_requeue`/
// `bump_lapsed`/`bump_poison_cleared` shim (and the bare helper itself, for census's own
// `recurred`/`reclaimed` test fixtures) now reaches through.
// =========================================================================================

/// A random v4-shaped id, read from the kernel rather than shelling to `python3 -c
/// 'import uuid...'` as the bash helper did — the exact bytes never mattered, only that
/// two concurrent writers cannot collide (same source `unpoison::Live::uuid4` uses).
fn uuid4() -> Result<String, String> {
    let u = std::fs::read_to_string("/proc/sys/kernel/random/uuid").map_err(|e| format!("uuid: {e}"))?;
    let u = u.trim().to_string();
    if u.len() == 36 && u.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        Ok(u)
    } else {
        Err("uuid: unexpected shape".into())
    }
}

/// The literal INSERT, pulled out of [`write_event`] so the shape is testable without a
/// live store.
fn insert_sql(uuid: &str, id: &str, etype: &str, actor: &str, cause: &str) -> String {
    format!(
        "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ({}, {}, {}, {}, {}, UTC_TIMESTAMP())",
        store::sql_quote(uuid),
        store::sql_quote(id),
        store::sql_quote(etype),
        store::sql_quote(&bounded_cause(actor)),
        store::sql_quote(&bounded_cause(cause)),
    )
}

/// One `events` row. Callers (the lib.sh shims) enforce `_bump_write_event_try`'s own
/// no-op early return on an empty `id`/`etype` before ever reaching this — this function
/// always writes.
pub fn write_event(store: &Store, actor: &str, id: &str, etype: &str, cause: &str) -> Result<(), String> {
    let uuid = uuid4()?;
    let q = insert_sql(&uuid, id, etype, actor, cause);
    store::run(store.bd_cmd(&["sql", &q]), store.timeout).map(|_| ())
}

// =========================================================================================
// count-events: `_counter_events_query`'s own raw `COUNT(*)` — generic over event_type,
// no exemption, no floor. Doctor's events-substrate probe writes a reserved
// `__doctor_probe__` event and reads this count straight back; `requeues_of` used to
// read it too, before this bead moved it onto the floored, exemption-aware
// `events::fold` count instead (DESIGN.md §6's already-sanctioned fix).
// =========================================================================================

fn count_sql(id: &str, etype: &str) -> String {
    format!("SELECT COUNT(*) FROM events WHERE issue_id={} AND event_type={}", store::sql_quote(id), store::sql_quote(etype))
}

/// `bd sql`'s own tabular output, third line, spaces stripped — the same extraction
/// `_counter_events_query`/`attempts_of` used against `bd sql`'s plain (non-`--json`)
/// table rendering.
fn parse_scalar_count(out: &str) -> Option<u64> {
    out.lines().nth(2)?.replace(' ', "").parse().ok()
}

pub fn count_events(store: &Store, id: &str, etype: &str) -> Result<u64, String> {
    let out = store::run(store.bd_cmd(&["sql", &count_sql(id, etype)]), store.timeout)?;
    parse_scalar_count(&out).ok_or_else(|| format!("not a count: {out:?}"))
}

// =========================================================================================
// write_lapse_record
// =========================================================================================

/// The exact record format `watchtower` parses — lib.sh's own comment: "written in the
/// same format aeon.sh uses".
pub fn lapse_record_content(bead: &str, quiet: &str, last: &str, tip: &str) -> String {
    format!("bead: {bead}\nquiet: {quiet}s\nlast: {last}\nbranch: spira/{bead}\ntip: {tip}\n")
}

/// `date -u +%Y%m%dT%H%M%SZ`, shelled out rather than hand-rolled: this fires once per
/// lapse, never on a hot path, and must match the format `watchtower`'s reader already
/// expects from the real writer (test-watchtower.sh's gap G8, test-watchtower-lapse.sh).
fn utc_stamp() -> String {
    std::process::Command::new("date")
        .args(["-u", "+%Y%m%dT%H%M%SZ"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// `$run_dir/lapsed/<bead>-<ts>`, best-effort: a `run_dir` that cannot be created, or a
/// write that fails, is silent — bash's own `mkdir -p ... || return 0`.
pub fn write_lapse_record(run_dir: &std::path::Path, bead: &str, quiet: &str, last: &str, tip: &str) {
    let dir = run_dir.join("lapsed");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let ts = utc_stamp();
    let _ = std::fs::write(dir.join(format!("{bead}-{ts}")), lapse_record_content(bead, quiet, last, tip));
}

// =========================================================================================
// bead_metadata / thrash_streak_bump
// =========================================================================================

/// `bd show <id> --long --json`'s metadata object — `--long` is required, `bd show
/// --json` omits metadata by default (sp-4rzlw). `bd` sometimes prints a warning line
/// before the JSON payload, same as every other `bd show`/`bd list` caller in this
/// crate, so this starts at the first `[` or `{`.
fn parse_metadata(text: &str) -> serde_json::Map<String, Value> {
    let empty = serde_json::Map::new();
    let Some(start) = text.find(['[', '{']) else { return empty };
    let Ok(v) = serde_json::from_str::<Value>(text[start..].trim()) else { return empty };
    let first = match v {
        Value::Array(a) => a.into_iter().next(),
        other => Some(other),
    };
    match first.and_then(|b| b.get("metadata").cloned()) {
        Some(Value::Object(m)) => m,
        _ => empty,
    }
}

/// The value `bd update --set-metadata` wrote, or empty when unset or unreadable —
/// bash's own fail-open (a parse error there printed "" too).
pub fn bead_metadata(store: &Store, id: &str, key: &str) -> String {
    if id.is_empty() || key.is_empty() {
        return String::new();
    }
    let Ok(text) = store::run(store.bd_cmd(&["show", id, "--long", "--json"]), store.timeout) else {
        return String::new();
    };
    parse_metadata(&text).get(key).and_then(Value::as_str).map(str::to_string).unwrap_or_default()
}

/// The streak AFTER this bump: the same tip as last time increments it; any other tip
/// (including the first thrash ever, an empty tip, or `"?"`, or one that moved) resets
/// it to 1.
pub fn next_streak(prev_tip: &str, prev_streak: u32, tip: &str) -> u32 {
    if !tip.is_empty() && tip != "?" && tip == prev_tip {
        prev_streak.saturating_add(1)
    } else {
        1
    }
}

/// bash's `tr '\n\r' '  ' | cut -c1-300` — folded into one pass.
pub fn truncate_note(note: &str) -> String {
    note.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).take(300).collect()
}

/// Stores `thrash_tip`/`thrash_streak`/`thrash_last` as bead metadata and returns the new
/// streak; best-effort — the write's own failure is swallowed, matching bash (which never
/// checked `bd update`'s exit code either).
pub fn thrash_streak_bump(store: &Store, id: &str, tip: &str, note: &str) -> u32 {
    let prev_tip = bead_metadata(store, id, "thrash_tip");
    let prev_streak: u32 = bead_metadata(store, id, "thrash_streak").trim().parse().unwrap_or(0);
    let streak = next_streak(&prev_tip, prev_streak, tip);
    let note = truncate_note(note);
    let tip_arg = format!("thrash_tip={tip}");
    let streak_arg = format!("thrash_streak={streak}");
    let last_arg = format!("thrash_last={note}");
    let args = ["update", id, "--set-metadata", &tip_arg, "--set-metadata", &streak_arg, "--set-metadata", &last_arg];
    let _ = store::run_full(store.bd_cmd(&args), store.timeout, None);
    streak
}

// =========================================================================================
// counter_label — read-only, for whatever historical sp-attempt-N/sp-reclaim-N/
// sp-requeue-N label a bead still carries. `bump_counter` stopped writing these at
// sp-lzt; nothing production writes any more, but capacity.sh still renders one if found.
// =========================================================================================

/// `grep -xE "<prefix>-<n>(-.*)?"`'s first match, in label order.
pub fn counter_label(labels: &[String], prefix: &str, n: &str) -> Option<String> {
    let exact = format!("{prefix}-{n}");
    let with_suffix = format!("{exact}-");
    labels.iter().find(|l| l.as_str() == exact || l.starts_with(&with_suffix)).cloned()
}

/// `bd show <id> --json`'s labels, for [`counter_label`] — reuses `unpoison::parse_bead`
/// so the "bd sometimes prints a warning first" handling is not duplicated a third time.
pub fn bead_labels(store: &Store, id: &str) -> Result<Vec<String>, String> {
    let text = store::run(store.bd_cmd(&["show", id, "--json"]), store.timeout)?;
    Ok(crate::unpoison::parse_bead(&text)?.map(|b| b.labels).unwrap_or_default())
}

#[cfg(test)]
#[path = "counters_tests.rs"]
mod tests;
