//! The shared bead-state snapshot: lc-serve rewrites `SPIRA_LC_SNAPSHOT` every
//! `SPIRA_LC_SNAPSHOT_SECS` from its in-memory live rows, and a read-only poller that passes
//! `--snapshot` to `list` is answered from that file instead of a call to the service.
//! Anything that must read its own write omits the flag and keeps asking the service.

use std::io::Write;
use std::path::Path;

use serde_json::{json, Value};

use crate::live::{self, ListFilter};

/// The rows to serve the request from, or `None` when it must go to the service.
pub fn answer(args: &[String]) -> Option<String> {
    let secs: u64 = spira_config::process::cfg_parse("SPIRA_LC_SNAPSHOT_SECS").ok().filter(|s| *s > 0)?;
    let path = spira_config::process::cfg("SPIRA_LC_SNAPSHOT").ok().filter(|p| !p.is_empty())?;
    let text = std::fs::read_to_string(path).ok()?;
    answer_from(args, &text, crate::db::now_epoch(), secs)
}

pub(crate) fn answer_from(args: &[String], text: &str, now: i64, secs: u64) -> Option<String> {
    let f = filter_of(args)?;
    let snap: Value = serde_json::from_str(text).ok()?;
    let at = snap.get("at")?.as_i64()?;
    if now < at || (now - at) as u64 > secs.saturating_mul(3) {
        return None;
    }
    let live = args.iter().any(|a| a == "--live");
    let cols: &[&str] = if live && f.repo.is_none() { &live::LIST_COLS } else { &live::FULL_LIST_COLS };
    let wanted = |set: &Option<Vec<String>>, v: &str| set.as_ref().is_none_or(|s| s.iter().any(|x| x == v));
    let out: Vec<Value> = snap
        .get("beads")?
        .as_array()?
        .iter()
        .filter(|b| wanted(&f.states, live::text(b, "state")) && wanted(&f.ids, live::text(b, "bead_id")))
        .filter(|b| f.hold.as_ref().is_none_or(|k| serde_json::from_str::<Vec<String>>(live::text(b, "holds")).is_ok_and(|h| h.contains(k))))
        .filter(|b| !f.express || live::num(b, "express") == Some(1))
        .filter(|b| f.repo.as_ref().is_none_or(|r| b.get("repo").and_then(Value::as_str) == Some(r.as_str())))
        .map(|b| {
            let mut o = live::project(b, cols);
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
    let f = live::filter_from_flags(value("--state"), value("--ids"), value("--hold"), args.iter().any(|a| a == "--express"), value("--repo"));
    let live_scope = args.iter().any(|a| a == "--live") || f.states.as_ref().is_some_and(|s| !s.is_empty() && s.iter().all(|x| live::LIVE.contains(&x.as_str())));
    live_scope.then_some(f)
}

/// What the service forwards: the request without the client-only flag.
pub fn without_flag(args: &[String]) -> Vec<String> {
    if args.first().map(String::as_str) != Some("list") {
        return args.to_vec();
    }
    args.iter().filter(|a| *a != "--snapshot").cloned().collect()
}

/// One refresh: the live rows rendered and moved into place atomically. `Ok(false)` while the
/// rows are not in memory.
pub fn write(conn: &crate::db::Conn, path: &str) -> std::io::Result<bool> {
    let Some(beads) = conn.live().and_then(|l| l.list_live_full(&ListFilter::default())) else { return Ok(false) };
    let body = json!({ "at": crate::db::now_epoch(), "beads": beads }).to_string();
    let tmp = format!("{path}.tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(body.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(Path::new(&tmp), path)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn fixture(at: i64) -> String {
        let bead = |id: &str, state: &str, holds: &str, express: &str, repo: &str| {
            json!({"bead_id": id, "state": state, "holds": holds, "express": express, "repo": repo, "title": "t", "tip": "", "blocked_by": ["sp-b"]})
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
        assert!(out[0].get("repo").is_none() && out[0].get("blocked_by").is_some());
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
            ("queue-watch certified", &["list", "--state", "CERTIFIED", "--snapshot"], &[]),
            ("queue-watch express", &["list", "--express", "--live", "--snapshot"], &["sp-b"]),
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
    fn the_flag_is_not_forwarded() {
        assert_eq!(without_flag(&args(&["list", "--snapshot", "--live"])), ["list", "--live"]);
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
