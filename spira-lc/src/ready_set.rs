//! `ready-set`: the beads the dispatcher's last pass judged claimable, in one read. The lifecycle
//! rows and the judgements come from the control store, the plan items from a copy kept under the
//! planning watermark, so a reader of the ready set neither judges nor queries bd.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::control::records::{LifecycleRecord, Readiness};
use crate::db::Conn;
use crate::{plan, CANNOT_TELL};

const FETCH_CHUNK: usize = 500;

/// The watermark the items were read at, and each id's item (`None`: bd has no such bead).
type Held = (Option<u64>, BTreeMap<String, Option<Value>>);

/// Reads plan items from bd.
type Fetch<'a> = dyn Fn(&[&str]) -> Result<Vec<Value>, String> + 'a;

static ITEMS: Mutex<Held> = Mutex::new((None, BTreeMap::new()));

pub fn run(conn: &Conn) -> (i32, String) {
    let cannot = |e: String| (CANNOT_TELL, format!("cannot tell: ready-set: {e}"));
    let store = match crate::control::served::configured() {
        Some(Ok(s)) => s,
        Some(Err(e)) => return cannot(e),
        None => return cannot("SPIRA_LC_STORE_PATH is unset".into()),
    };
    let (judged, unjudged) = match store.judged_claimable() {
        Ok(j) => j,
        Err(e) => return cannot(e.to_string()),
    };
    match answer(&judged, unjudged, plan::current_watermark(), &ITEMS, &|ids| fetch(conn, ids)) {
        Ok(v) => (0, v.to_string()),
        Err(e) => cannot(e),
    }
}

fn fetch(conn: &Conn, ids: &[&str]) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    for chunk in ids.chunks(FETCH_CHUNK) {
        out.extend(plan::fetch(conn, chunk)?);
    }
    Ok(out)
}

/// The answer for `judged`: its lifecycle rows as `list` prints them, their plan items, and the
/// oldest planning watermark and judgement time among them, the freshness the whole set is
/// good to. `held` keeps items read at `watermark`; with no watermark nothing is kept.
fn answer(
    judged: &[(LifecycleRecord, Readiness)],
    unjudged: usize,
    watermark: Option<u64>,
    held: &Mutex<Held>,
    fetch: &Fetch,
) -> Result<Value, String> {
    let ids: Vec<&str> = judged.iter().map(|(r, _)| r.row.bead_id.as_str()).collect();
    let items = items_of(&ids, watermark, held, fetch)?;
    let oldest = |f: fn(&Readiness) -> Option<&String>| judged.iter().map(|(_, j)| f(j).cloned()).min().flatten();
    Ok(json!({
        "planning_watermark": oldest(|j| j.judged_at_planning_watermark.as_ref()),
        "judged_at": oldest(|j| j.judged_at.as_ref()),
        "unjudged": unjudged,
        "lifecycle": judged.iter().map(|(r, j)| crate::control::state::listed(r, Some(j))).collect::<Vec<_>>(),
        "items": items,
    }))
}

fn items_of(ids: &[&str], watermark: Option<u64>, held: &Mutex<Held>, fetch: &Fetch) -> Result<Vec<Value>, String> {
    let Some(w) = watermark else { return fetch(ids) };
    let lock = || held.lock().unwrap_or_else(|p| p.into_inner());
    let (mut out, missing) = {
        let mut g = lock();
        if g.0 != Some(w) {
            *g = (Some(w), BTreeMap::new());
        }
        let mut out = Vec::new();
        let mut missing = Vec::new();
        for id in ids {
            match g.1.get(*id) {
                Some(Some(item)) => out.push(item.clone()),
                Some(None) => {}
                None => missing.push(*id),
            }
        }
        (out, missing)
    };
    if missing.is_empty() {
        return Ok(out);
    }
    let found = fetch(&missing)?;
    let mut g = lock();
    if g.0 == Some(w) {
        let mut known: BTreeMap<String, Option<Value>> = missing.iter().map(|id| (id.to_string(), None)).collect();
        known.extend(found.iter().filter_map(|i| Some((i.get("id")?.as_str()?.to_string(), Some(i.clone())))));
        g.1.extend(known);
    }
    out.extend(found);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn judged(id: &str, watermark: &str, at: &str) -> (LifecycleRecord, Readiness) {
        (
            LifecycleRecord::born(id, None, None, 0),
            Readiness { claimable: true, judged_for_claim: true, judged_at_planning_watermark: Some(watermark.into()), judged_at: Some(at.into()), ..Readiness::default() },
        )
    }

    fn bd(ids: &[&str]) -> Result<Vec<Value>, String> {
        Ok(ids.iter().map(|i| json!({ "id": i, "labels": ["a"] })).collect())
    }

    #[test]
    fn the_answer_names_the_oldest_planning_watermark_and_judgement_it_is_good_to() {
        let held = Mutex::new((None, BTreeMap::new()));
        let rows = [judged("sp-a", "9", "2026-11-11T10:00:05Z"), judged("sp-b", "7", "2026-11-11T10:00:09Z")];
        let v = answer(&rows, 0, Some(9), &held, &|ids| bd(ids)).unwrap();
        assert_eq!((v["planning_watermark"].as_str(), v["judged_at"].as_str()), (Some("7"), Some("2026-11-11T10:00:05Z")));
        assert_eq!(v["lifecycle"].as_array().unwrap().len(), 2);
        assert_eq!(v["items"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn items_are_read_from_bd_once_per_planning_watermark() {
        let held = Mutex::new((None, BTreeMap::new()));
        let rows = [judged("sp-a", "9", "t"), judged("sp-b", "9", "t")];
        let reads = Cell::new(0);
        let fetch = |ids: &[&str]| {
            reads.set(reads.get() + 1);
            bd(ids)
        };
        for _ in 0..3 {
            assert_eq!(answer(&rows, 0, Some(9), &held, &fetch).unwrap()["items"].as_array().unwrap().len(), 2);
        }
        assert_eq!(reads.get(), 1, "the same watermark is served from memory");
        answer(&rows, 0, Some(10), &held, &fetch).unwrap();
        assert_eq!(reads.get(), 2, "a planning write moved the watermark, so bd is asked again");
        let more = [judged("sp-a", "9", "t"), judged("sp-b", "9", "t"), judged("sp-c", "9", "t")];
        let asked = std::cell::RefCell::new(Vec::new());
        answer(&more, 0, Some(10), &held, &|ids| {
            asked.borrow_mut().extend(ids.iter().map(|i| i.to_string()));
            bd(ids)
        })
        .unwrap();
        assert_eq!(*asked.borrow(), ["sp-c"], "only the bead not yet held is read");
    }

    #[test]
    fn nothing_is_kept_without_a_watermark() {
        let held = Mutex::new((None, BTreeMap::new()));
        let rows = [judged("sp-a", "9", "t")];
        let reads = Cell::new(0);
        let fetch = |ids: &[&str]| {
            reads.set(reads.get() + 1);
            bd(ids)
        };
        answer(&rows, 0, None, &held, &fetch).unwrap();
        answer(&rows, 0, None, &held, &fetch).unwrap();
        assert_eq!(reads.get(), 2);
    }

    #[test]
    fn a_bead_bd_does_not_have_is_left_out_and_not_asked_for_again() {
        let held = Mutex::new((None, BTreeMap::new()));
        let rows = [judged("sp-a", "9", "t"), judged("sp-gone", "9", "t")];
        let reads = Cell::new(0);
        let fetch = |ids: &[&str]| {
            reads.set(reads.get() + 1);
            bd(&ids.iter().copied().filter(|i| *i != "sp-gone").collect::<Vec<_>>())
        };
        for _ in 0..2 {
            let v = answer(&rows, 0, Some(9), &held, &fetch).unwrap();
            assert_eq!(v["items"].as_array().unwrap().len(), 1);
        }
        assert_eq!(reads.get(), 1);
    }
}
