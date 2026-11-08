//! The shared bead-state snapshot: lc-serve rewrites `SPIRA_LC_SNAPSHOT` every
//! `SPIRA_LC_SNAPSHOT_SECS` from its in-memory live rows, and a read-only poller that passes
//! `--snapshot` to `list` is answered from that file instead of a call to the service.
//! Anything that must read its own write omits the flag and keeps asking the service.

use std::io::Write;
use std::path::Path;

use serde_json::{json, Value};

use serde_json::Map;

const LIVE: [&str; 7] = ["OPEN", "READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "REWORK"];

/// The control store's list columns, then the planning fields a plain list joins on.
const LIST_COLS: [&str; 22] = [
    "bead_id", "state", "tip", "holder", "persona", "lease_until", "holds", "reason", "updated_at", "version", "stack", "stack_depth", "since", "express", "dwarf_phase", "disposition", "disposition_note",
    "ejected_red_tip", "repo", "claim_verdict", "claim_blocker", "claim_runes",
];
const FULL_ONLY: [&str; 1] = ["sifted_tip"];
const CLAIM_AT: &str = "claim_at";

#[derive(Default)]
struct ListFilter {
    repo: Option<String>,
    states: Option<Vec<String>>,
    ids: Option<Vec<String>>,
    hold: Option<String>,
    express: bool,
}

fn filter_from_flags(state: Option<&str>, ids: Option<&str>, hold: Option<&str>, express: bool, repo: Option<&str>) -> ListFilter {
    let csv = |v: &str| v.split(',').filter(|x| !x.is_empty()).map(str::to_string).collect::<Vec<_>>();
    ListFilter { states: state.map(csv), ids: ids.map(csv), hold: hold.map(str::to_string), express, repo: repo.map(str::to_string) }
}

fn text<'a>(row: &'a Value, col: &str) -> &'a str {
    row.get(col).and_then(Value::as_str).unwrap_or_default()
}

fn num(row: &Value, col: &str) -> Option<i64> {
    row.get(col).and_then(Value::as_str).and_then(|s| s.parse().ok())
}

fn project(row: &Value, cols: &[&str]) -> Map<String, Value> {
    cols.iter().map(|c| (c.to_string(), row.get(*c).cloned().unwrap_or(Value::Null))).collect()
}

fn columns(full: bool) -> Vec<&'static str> {
    let mut cols: Vec<&str> = LIST_COLS.to_vec();
    cols.push(CLAIM_AT);
    cols.extend(crate::plan::JOINED);
    if full {
        cols.extend(FULL_ONLY);
    }
    cols
}

/// The rows to serve the request from, or `None` when it must go to the service. A poller that
/// asked for the snapshot and did not get it is counted by reason, so a broken snapshot shows.
pub fn answer(args: &[String]) -> Option<String> {
    let wanted = filter_of(args).is_some();
    let out = read(args);
    if wanted {
        if let Err(reason) = &out {
            record_miss(reason);
        }
    }
    out.ok()
}

fn read(args: &[String]) -> Result<String, &'static str> {
    let secs: u64 = spira_config::process::cfg_parse("SPIRA_LC_SNAPSHOT_SECS").ok().filter(|s| *s > 0).ok_or("disabled")?;
    let path = spira_config::process::cfg("SPIRA_LC_SNAPSHOT").ok().filter(|p| !p.is_empty()).ok_or("disabled")?;
    read_at(args, &path, secs, crate::db::now_epoch())
}

fn read_at(args: &[String], path: &str, secs: u64, now: i64) -> Result<String, &'static str> {
    let text = std::fs::read_to_string(path).map_err(|_| "missing")?;
    answer_from(args, &text, now, secs).ok_or("stale")
}

fn record_miss(reason: &str) {
    use spira_otel::metrics::{MetricsSink, Point};
    let Ok(endpoint) = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT") else { return };
    MetricsSink::new(Some(&endpoint), "spira-lc", std::time::Duration::from_millis(250))
        .push(&[Point::Count { name: "spira_lc_snapshot_fallback", value: 1 }], &[("reason", reason)]);
}

pub(crate) fn answer_from(args: &[String], raw: &str, now: i64, secs: u64) -> Option<String> {
    let f = filter_of(args)?;
    let snap: Value = serde_json::from_str(raw).ok()?;
    let at = snap.get("at")?.as_i64()?;
    if now < at || (now - at) as u64 > secs.saturating_mul(3) {
        return None;
    }
    let live = args.iter().any(|a| a == "--live");
    let cols = columns(!(live && f.repo.is_none()));
    let cols = cols.as_slice();
    let wanted = |set: &Option<Vec<String>>, v: &str| set.as_ref().is_none_or(|s| s.iter().any(|x| x == v));
    let out: Vec<Value> = snap
        .get("beads")?
        .as_array()?
        .iter()
        .filter(|b| wanted(&f.states, text(b, "state")) && wanted(&f.ids, text(b, "bead_id")))
        .filter(|b| f.hold.as_ref().is_none_or(|k| serde_json::from_str::<Vec<String>>(text(b, "holds")).is_ok_and(|h| h.contains(k))))
        .filter(|b| !f.express || num(b, "express") == Some(1))
        .filter(|b| f.repo.as_ref().is_none_or(|r| b.get("repo").and_then(Value::as_str) == Some(r.as_str())))
        .map(|b| {
            let mut o = project(b, cols);
            o.insert("blocked_by".into(), b.get("blocked_by").cloned().unwrap_or(Value::Array(vec![])));
            Value::Object(o)
        })
        .collect();
    Some(Value::Array(out).to_string())
}

/// A `--snapshot` list that asks only for live rows: the file holds nothing terminal, so
/// anything that might name one is not answerable from it.
fn filter_of(args: &[String]) -> Option<ListFilter> {
    if args.first().map(String::as_str) != Some("list") || !args.iter().any(|a| a == "--snapshot") {
        return None;
    }
    if args.iter().any(|a| a == "--delivery" || a == "--batches" || a == "--cached") {
        return None;
    }
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str);
    let f = filter_from_flags(value("--state"), value("--ids"), value("--hold"), args.iter().any(|a| a == "--express"), value("--repo"));
    let live_scope = args.iter().any(|a| a == "--live") || f.states.as_ref().is_some_and(|s| !s.is_empty() && s.iter().all(|x| LIVE.contains(&x.as_str())));
    live_scope.then_some(f)
}

/// What the service forwards: the request without the client-only flag.
pub fn without_flag(args: &[String]) -> Vec<String> {
    if args.first().map(String::as_str) != Some("list") {
        return args.to_vec();
    }
    args.iter().filter(|a| *a != "--snapshot").cloned().collect()
}

/// One refresh: the control store's live rows rendered and moved into place atomically.
/// `Ok(false)` while no store is configured; an error leaves the old file to age out, so
/// pollers go to the service rather than read rows the store has moved past.
pub fn write(conn: &crate::db::Conn, path: &str) -> std::io::Result<bool> {
    write_from(crate::control::served::configured(), conn, path, crate::db::now_epoch())
}

fn write_from<B: state_store::Backend, S: std::ops::Deref<Target = crate::control::store::Store<B>>>(
    store: Option<Result<S, String>>,
    conn: &crate::db::Conn,
    path: &str,
    now: i64,
) -> std::io::Result<bool> {
    let Some(store) = store else { return Ok(false) };
    let beads = store
        .and_then(|s| store_rows(&s, now))
        .and_then(|rows| with_planning(rows, conn))
        .map_err(std::io::Error::other)?;
    let tmp = format!("{path}.tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(body(beads, now).as_bytes())?;
    f.sync_all()?;
    std::fs::rename(Path::new(&tmp), path)?;
    Ok(true)
}

fn body(beads: Vec<Value>, at: i64) -> String {
    json!({ "at": at, "beads": beads }).to_string()
}

/// The control store's live rows at its head as of this call: the answer `list --live` gives.
fn store_rows<B: state_store::Backend>(store: &crate::control::store::Store<B>, now: i64) -> Result<Vec<Value>, String> {
    let request = crate::control::served::request_id();
    let call = crate::control::state::Call { now, request: &request };
    match crate::control::state::run(store, &[String::from("list"), String::from("--live")], &call) {
        (0, out) => serde_json::from_str(&out).map_err(|e| format!("the store's live list: {e}")),
        (_, out) => Err(out),
    }
}

/// Titles and priorities from the planning plane, joined as the service joins them on a plain list.
fn with_planning(rows: Vec<Value>, conn: &crate::db::Conn) -> Result<Vec<Value>, String> {
    match crate::plan::with_planning_fields((0, Value::Array(rows).to_string()), conn) {
        (0, out) => serde_json::from_str(&out).map_err(|e| format!("the planning join: {e}")),
        (_, out) => Err(out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn fixture(at: i64) -> String {
        let bead = |id: &str, state: &str, holds: &str, express: &str, repo: &str| {
            json!({"bead_id": id, "state": state, "holds": holds, "express": express, "repo": repo, "tip": "", "blocked_by": ["sp-b"]})
        };
        json!({"at": at, "beads": [
            bead("sp-a", "IN_DELIVERY", "[]", "0", "spira"),
            bead("sp-b", "READY", "[\"wait\"]", "1", "other"),
            bead("sp-c", "READY", "[]", "0", "spira"),
        ]})
        .to_string()
    }

    fn ids(out: &str) -> Vec<String> {
        serde_json::from_str::<Vec<Value>>(out).unwrap().iter().map(|b| b["bead_id"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn a_state_filter_is_answered_from_the_fixture() {
        let out = answer_from(&args(&["list", "--snapshot", "--state", "READY"]), &fixture(100), 101, 2).unwrap();
        assert_eq!(ids(&out), ["sp-b", "sp-c"]);
        let out = answer_from(&args(&["list", "--snapshot", "--state", "IN_DELIVERY"]), &fixture(100), 101, 2).unwrap();
        assert_eq!(ids(&out), ["sp-a"]);
    }

    #[test]
    fn hold_express_repo_and_ids_narrow_the_live_set() {
        let t = fixture(100);
        let run = |a: &[&str]| ids(&answer_from(&args(a), &t, 100, 2).unwrap());
        assert_eq!(run(&["list", "--snapshot", "--live", "--hold", "wait"]), ["sp-b"]);
        assert_eq!(run(&["list", "--snapshot", "--live", "--express"]), ["sp-b"]);
        assert_eq!(run(&["list", "--snapshot", "--live", "--repo", "spira"]), ["sp-a", "sp-c"]);
        assert_eq!(run(&["list", "--snapshot", "--live", "--ids", "sp-c,sp-zz"]), ["sp-c"]);
        assert_eq!(run(&["list", "--snapshot", "--live"]), ["sp-a", "sp-b", "sp-c"]);
    }

    #[test]
    fn a_live_list_carries_the_columns_of_the_service_answer() {
        let out: Vec<Value> = serde_json::from_str(&answer_from(&args(&["list", "--snapshot", "--live"]), &fixture(100), 100, 2).unwrap()).unwrap();
        assert!(out[0].get("blocked_by").is_some());
        assert_eq!(out.iter().map(|b| b["repo"].as_str().unwrap()).collect::<Vec<_>>(), ["spira", "other", "spira"], "the pane tags each row from this column");
        let full: Vec<Value> = serde_json::from_str(&answer_from(&args(&["list", "--snapshot", "--live", "--repo", "spira"]), &fixture(100), 100, 2).unwrap()).unwrap();
        assert!(full[0].get("repo").is_some() && full[0].get("sifted_tip").is_some());
    }

    #[test]
    fn what_the_file_cannot_answer_goes_to_the_service() {
        let t = fixture(100);
        let none = |a: &[&str]| assert!(answer_from(&args(a), &t, 100, 2).is_none(), "{a:?}");
        none(&["list", "--live"]);
        none(&["list", "--snapshot"]);
        none(&["list", "--snapshot", "--state", "LANDED"]);
        none(&["list", "--snapshot", "--state", "READY,LANDED"]);
        none(&["list", "--snapshot", "--ids", "sp-a"]);
        none(&["list", "--snapshot", "--live", "--delivery"]);
        none(&["list", "--snapshot", "--live", "--cached"]);
        none(&["show", "--snapshot", "--live"]);
    }

    #[test]
    fn every_converted_poller_is_answered_from_the_fixture() {
        let t = fixture(100);
        let callers: [(&str, &[&str], &[&str]); 7] = [
            ("lc-view", &["list", "--live", "--snapshot"], &["sp-a", "sp-b", "sp-c"]),
            ("watchtower", &["list", "--state", "READY", "--snapshot"], &["sp-b", "sp-c"]),
            ("certified list", &["list", "--state", "CERTIFIED", "--snapshot"], &[]),
            ("express list", &["list", "--express", "--live", "--snapshot"], &["sp-b"]),
            ("batcher-cut", &["list", "--state", "SUBMITTED,CERTIFIED", "--snapshot"], &[]),
            ("hold-sweep", &["list", "--hold", "wait", "--live", "--snapshot"], &["sp-b"]),
            ("supersede-duty", &["list", "--live", "--hold", "manual", "--snapshot"], &[]),
        ];
        for (who, a, want) in callers {
            let out = answer_from(&args(a), &t, 100, 2).unwrap_or_else(|| panic!("{who} fell through to the service"));
            assert_eq!(ids(&out), want, "{who}");
        }
    }

    #[test]
    fn a_stale_or_future_or_broken_file_is_not_believed() {
        let a = args(&["list", "--snapshot", "--live"]);
        assert!(answer_from(&a, &fixture(100), 106, 2).is_some());
        assert!(answer_from(&a, &fixture(100), 107, 2).is_none());
        assert!(answer_from(&a, &fixture(100), 99, 2).is_none());
        assert!(answer_from(&a, "{not json", 100, 2).is_none());
        assert!(answer_from(&a, "{\"beads\":[]}", 100, 2).is_none());
    }

    #[test]
    fn a_missing_or_stale_file_names_why_it_was_not_answered() {
        let dir = testkit::TempDir::new("lc-snapshot-miss");
        let a = args(&["list", "--snapshot", "--live"]);
        let path = dir.join("snap.json");
        let p = path.to_str().unwrap();
        assert_eq!(read_at(&a, p, 2, 100), Err("missing"));
        std::fs::write(&path, fixture(100)).unwrap();
        assert!(read_at(&a, p, 2, 100).is_ok());
        assert_eq!(read_at(&a, p, 2, 200), Err("stale"));
    }

    #[test]
    fn the_flag_is_not_forwarded() {
        assert_eq!(without_flag(&args(&["list", "--snapshot", "--live"])), ["list", "--live"]);
    }

    /// A control store on the in-memory backend, as lc-serve holds one once the lifecycle is on it.
    fn store() -> crate::control::store::Store<state_store::MemBackend> {
        let clock = runtime::ports::VirtualClock::new(100_000);
        let c = state_store::Contract::open(state_store::MemBackend::new(), state_store::Ports::virtual_clock(&clock), state_store::Config { min_lock_ttl_ms: 1_000 }).unwrap();
        crate::control::store::Store::new(c, 1_000)
    }

    fn create(s: &crate::control::store::Store<state_store::MemBackend>, argv: &[&str]) {
        let call = crate::control::state::Call { now: 100, request: &format!("t/{argv:?}") };
        let (code, out) = crate::control::state::run(s, &args(argv), &call);
        assert_eq!(code, 0, "{argv:?}: {out}");
    }

    /// The refresh lc-serve runs, with the lifecycle on the store: the file it renders holds a
    /// row written after the store opened, the same rows a plain `list --live` answers.
    #[test]
    fn the_refresh_renders_a_row_written_after_the_store_opened() {
        let dir = testkit::TempDir::new("lc-snapshot-store");
        let path = dir.join("snap.json");
        let p = path.to_str().unwrap();
        let conn = crate::db::Conn::unreachable();
        let s = store();
        create(&s, &["create-bead", "sp-old", "--ready"]);
        assert!(write_from(Some(Ok(&s)), &conn, p, 100).unwrap(), "the store's rows need no dolt");
        create(&s, &["create-bead", "sp-new", "--submitted-tip", "0123456789abcdef0123456789abcdef01234567"]);
        let a = |argv: &[&str]| ids(&answer_from(&args(argv), &std::fs::read_to_string(&path).unwrap(), 100, 2).unwrap());
        assert_eq!(a(&["list", "--snapshot", "--state", "SUBMITTED"]), Vec::<String>::new(), "a file answers the store as it stood when rendered");
        assert!(write_from(Some(Ok(&s)), &conn, p, 100).unwrap());
        assert_eq!(a(&["list", "--snapshot", "--state", "SUBMITTED"]), ["sp-new"], "the next refresh carries the write");
        assert_eq!(a(&["list", "--snapshot", "--live"]), ["sp-new", "sp-old"]);
        let call = crate::control::state::Call { now: 100, request: "t/plain" };
        let plain: Vec<Value> = serde_json::from_str(&crate::control::state::run(&s, &args(&["list", "--live"]), &call).1).unwrap();
        let snap: Vec<Value> = serde_json::from_str(&answer_from(&args(&["list", "--snapshot", "--live"]), &std::fs::read_to_string(&path).unwrap(), 100, 2).unwrap()).unwrap();
        assert_eq!(snap.len(), plain.len());
        for (x, y) in snap.iter().zip(&plain) {
            for k in ["bead_id", "state", "tip", "holds", "version", "express", "blocked_by"] {
                assert_eq!(x[k], y[k], "{k}");
            }
        }
    }

    #[test]
    fn a_file_rendered_earlier_still_answers_its_own_rows_until_it_ages_out() {
        let s = store();
        create(&s, &["create-bead", "sp-a", "--ready"]);
        let old = body(store_rows(&s, 100).unwrap(), 100);
        create(&s, &["create-bead", "sp-b", "--ready"]);
        let a = args(&["list", "--snapshot", "--live"]);
        assert_eq!(ids(&answer_from(&a, &old, 106, 2).unwrap()), ["sp-a"]);
        assert!(answer_from(&a, &old, 107, 2).is_none(), "past three refreshes the service answers instead");
    }

    #[test]
    fn the_file_is_replaced_whole() {
        let dir = testkit::TempDir::new("lc-snapshot");
        let path = dir.join("snap.json");
        std::fs::write(&path, "old").unwrap();
        let conn = crate::db::Conn::unreachable();
        assert!(!write(&conn, path.to_str().unwrap()).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
    }
}
