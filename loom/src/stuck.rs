//! The stuck page: one row per bead touched in the last day, a bar per state, a red outline on
//! a bar past that state's measured p95 dwell, stuck rows first. Reads the lifecycle read model
//! through `spira-lc` and nothing else; every threshold is `ops_dwell_p95`, none is set here.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const WINDOW_S: i64 = 86_400;
const LIVE: [&str; 7] = ["OPEN", "READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "REWORK"];

#[derive(Debug, Clone, PartialEq)]
pub struct Seg {
    pub state: String,
    pub start: i64,
    pub end: i64,
    pub over: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub bead_id: String,
    pub title: String,
    pub priority: Option<i64>,
    pub state: String,
    pub holds: String,
    pub segs: Vec<Seg>,
    pub stuck: bool,
    pub dwell_s: i64,
    pub p95_s: Option<i64>,
    pub last_at: i64,
}

pub fn num(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn is_live(state: &str) -> bool {
    LIVE.contains(&state)
}

pub fn p95_map(rows: &[Value]) -> BTreeMap<String, i64> {
    rows.iter().filter_map(|r| Some((text(&r["state"]), num(&r["p95_s"])?))).collect()
}

/// `events` are the day's applied transitions; `dwell` the live beads with their entry time.
/// A live bead past its p95 is listed even with no event in the window — a bead stuck for
/// thirty hours is the one this page exists to show.
pub fn build(now: i64, events: &[Value], dwell: &[Value], p95: &BTreeMap<String, i64>) -> Vec<Row> {
    let ws = now - WINDOW_S;
    let mut by_bead: BTreeMap<String, Vec<(i64, i64, String, String)>> = BTreeMap::new();
    for e in events {
        if let (Some(seq), Some(at)) = (num(&e["seq"]), num(&e["at"])) {
            by_bead.entry(text(&e["bead_id"])).or_default().push((at, seq, text(&e["from_state"]), text(&e["to_state"])));
        }
    }
    let live: BTreeMap<String, &Value> = dwell.iter().map(|d| (text(&d["bead_id"]), d)).collect();
    let ids: BTreeSet<String> = by_bead.keys().cloned().chain(live.iter().filter(|(_, d)| past(now, d)).map(|(k, _)| k.clone())).collect();
    let mut rows: Vec<Row> = ids.into_iter().map(|id| row(now, ws, &id, by_bead.remove(&id).unwrap_or_default(), live.get(&id).copied(), p95)).collect();
    let overshoot = |r: &Row| if r.stuck { r.dwell_s - r.p95_s.unwrap_or(0) } else { 0 };
    rows.sort_by(|a, b| {
        b.stuck.cmp(&a.stuck)
            .then(overshoot(b).cmp(&overshoot(a)))
            .then(b.last_at.cmp(&a.last_at))
            .then(a.priority.unwrap_or(9).cmp(&b.priority.unwrap_or(9)))
            .then(a.bead_id.cmp(&b.bead_id))
    });
    rows
}

fn past(now: i64, d: &Value) -> bool {
    matches!((num(&d["entered_at"]), num(&d["p95_s"])), (Some(at), Some(p)) if now - at > p)
}

fn row(now: i64, ws: i64, id: &str, mut evs: Vec<(i64, i64, String, String)>, live: Option<&Value>, p95: &BTreeMap<String, i64>) -> Row {
    evs.sort();
    let over = |state: &str, from: i64, to: i64| p95.get(state).is_some_and(|p| to - from > *p);
    let mut segs = Vec::new();
    let mut cursor: Option<(String, i64)> = evs.first().map(|e| (e.2.clone(), ws));
    for (at, _, _from, to) in &evs {
        if let Some((state, start)) = cursor.take() {
            segs.push(Seg { over: is_live(&state) && over(&state, start, *at), state, start, end: *at });
        }
        cursor = Some((to.clone(), *at));
    }
    if let Some(d) = live {
        let (state, entered) = (text(&d["state"]), num(&d["entered_at"]).unwrap_or(ws));
        if cursor.as_ref().map(|c| &c.0) != Some(&state) {
            cursor = Some((state, entered.max(ws)));
        }
    }
    let (state, entered) = cursor.unwrap_or_else(|| ("?".into(), now));
    let dwell_s = live.and_then(|d| num(&d["entered_at"])).map_or(now - entered, |at| now - at);
    let stuck = is_live(&state) && over(&state, now - dwell_s, now);
    let end = if is_live(&state) { now } else { entered + 1 };
    segs.push(Seg { over: stuck, state: state.clone(), start: entered, end });
    let last_at = evs.last().map_or(entered, |e| e.0);
    Row {
        bead_id: id.to_string(),
        title: live.map(|d| text(&d["title"])).unwrap_or_default(),
        priority: live.and_then(|d| num(&d["priority"])),
        holds: live.map(|d| holds_text(&d["holds"])).unwrap_or_default(),
        p95_s: p95.get(&state).copied(),
        state,
        segs,
        stuck,
        dwell_s,
        last_at,
    }
}

pub fn row_json(r: &Row) -> Value {
    serde_json::json!({
        "bead_id": r.bead_id, "title": r.title, "priority": r.priority, "state": r.state, "holds": r.holds,
        "stuck": r.stuck, "dwell_s": r.dwell_s, "p95_s": r.p95_s,
        "segs": r.segs.iter().map(|s| serde_json::json!({"state": s.state, "start": s.start, "end": s.end, "over": s.over})).collect::<Vec<_>>(),
    })
}

fn holds_text(v: &Value) -> String {
    let parsed: Value = v.as_str().and_then(|s| serde_json::from_str(s).ok()).unwrap_or_else(|| v.clone());
    match parsed {
        Value::Array(a) => a.iter().filter_map(|h| h.as_str().map(str::to_string)).collect::<Vec<_>>().join(","),
        _ => String::new(),
    }
}

pub fn dur(s: i64) -> String {
    let s = s.max(0);
    match s {
        0..=89 => format!("{s}s"),
        90..=5399 => format!("{}m", (s + 30) / 60),
        5400..=172_799 => format!("{:.1}h", s as f64 / 3600.0),
        _ => format!("{:.1}d", s as f64 / 86_400.0),
    }
}

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

const CSS: &str = "body{margin:0;font:14px/1.35 system-ui,sans-serif;background:#10141a;color:#d8dee9}\
header{padding:.6rem .8rem;position:sticky;top:0;background:#10141a;border-bottom:1px solid #2a323d}\
h1{font-size:1rem;margin:0}.dim{color:#7d8896}a{color:inherit;text-decoration:none}\
.row{display:block;padding:.5rem .8rem;border-bottom:1px solid #1d232b}.row.stuck{background:#1e1416}\
.head{display:flex;gap:.5rem;align-items:baseline;flex-wrap:wrap}.id{font-weight:600}.t{flex:1 1 12rem;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.bar{position:relative;height:16px;margin-top:.3rem;background:#161b22;border-radius:3px}\
.seg{position:absolute;top:0;bottom:0;border-radius:2px;box-sizing:border-box;min-width:3px}\
.seg.over{outline:2px solid #ff4d4f;outline-offset:-1px;z-index:1}\
.s-OPEN{background:#4a5568}.s-READY{background:#3b82c4}.s-WORKING{background:#3aa57a}.s-SUBMITTED{background:#8b6fd0}\
.s-CERTIFIED{background:#d6a032}.s-IN_DELIVERY{background:#d9772b}.s-REWORK{background:#c25a8a}.s-LANDED{background:#2f6f4f}\
.badge{font-size:.75rem;padding:0 .35rem;border-radius:3px;background:#2a323d}.red{color:#ff6b6d}.badge.red{background:#4a1f21}\
table{border-collapse:collapse;width:100%}td{padding:.3rem .5rem;border-bottom:1px solid #1d232b;vertical-align:top;word-break:break-word}\
tr.refused td{color:#ff6b6d}.legend{display:flex;gap:.6rem;flex-wrap:wrap;padding:.4rem .8rem;font-size:.75rem}.legend i{display:inline-block;width:.7rem;height:.7rem;margin-right:.25rem;border-radius:2px}";

fn shell(title: &str, refresh_s: u32, body: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\"><meta http-equiv=refresh content={refresh_s}><title>{}</title><style>{CSS}</style><body>{body}</body>",
        esc(title)
    )
}

pub fn render_page(now: i64, rows: &[Row]) -> String {
    let stuck = rows.iter().filter(|r| r.stuck).count();
    let mut b = format!(
        "<header><h1>Stuck <span class=dim>— {} beads in 24 h · <span class=\"{}\">{stuck} past p95</span> · <a href=/lifecycle>lifecycle</a></span></h1></header><div class=legend>",
        rows.len(),
        if stuck > 0 { "red" } else { "dim" }
    );
    for s in LIVE.iter().chain(&["LANDED"]) {
        b.push_str(&format!("<span><i class=\"s-{s}\"></i>{s}</span>"));
    }
    b.push_str("<span><i style=\"outline:2px solid #ff4d4f\"></i>past state p95</span></div>");
    let ws = now - WINDOW_S;
    for r in rows {
        let p = r.priority.map_or(String::new(), |p| format!("P{p}"));
        let holds = if r.holds.is_empty() { String::new() } else { format!(" <span class=badge>{}</span>", esc(&r.holds)) };
        let over = match (r.stuck, r.p95_s) {
            (true, Some(p)) => format!(" <span class=\"badge red\">{} in {} · p95 {}</span>", dur(r.dwell_s), esc(&r.state), dur(p)),
            _ => format!(" <span class=dim>{} {}</span>", esc(&r.state), dur(r.dwell_s)),
        };
        b.push_str(&format!(
            "<a class=\"row{}\" href=\"/stuck/{id}\"><div class=head><span class=id>{id}</span><span class=dim>{p}</span><span class=t>{}</span></div><div>{over}{holds}</div><div class=bar>",
            if r.stuck { " stuck" } else { "" },
            esc(&r.title),
            id = esc(&r.bead_id),
        ));
        for s in &r.segs {
            let left = ((s.start - ws).max(0) as f64 / WINDOW_S as f64 * 100.0).min(99.0);
            let width = ((s.end - s.start).max(0) as f64 / WINDOW_S as f64 * 100.0).min(100.0 - left);
            b.push_str(&format!(
                "<span class=\"seg s-{st}{o}\" style=\"left:{left:.2}%;width:{width:.2}%\" title=\"{st} {d}\"></span>",
                st = esc(&s.state),
                o = if s.over { " over" } else { "" },
                d = dur(s.end - s.start),
            ));
        }
        b.push_str("</div></a>");
    }
    if rows.is_empty() {
        b.push_str("<p class=dim style=\"padding:.8rem\">no bead touched in the last 24 h</p>");
    }
    shell("Stuck", 30, &b)
}

/// Ids of the same family as `own` (same prefix before the first dash) named in `texts`.
pub fn blockers(own: &str, texts: &[String]) -> Vec<String> {
    let prefix = own.split('-').next().unwrap_or_default();
    let mut out = BTreeSet::new();
    for t in texts {
        for tok in t.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.')) {
            let tok = tok.trim_matches(|c| c == '-' || c == '.');
            if let Some((p, rest)) = tok.split_once('-') {
                if p == prefix && rest.len() >= 4 && !rest.contains('-') && tok != own && !tok.starts_with(&format!("{own}.")) {
                    out.insert(tok.to_string());
                }
            }
        }
    }
    out.into_iter().collect()
}

pub fn render_bead(now: i64, id: &str, detail: &Value) -> String {
    let bead = &detail["bead"];
    let mut b = format!("<header><h1><a href=/stuck>← stuck</a> <span class=dim>/</span> {}</h1><div class=dim>{}</div></header>", esc(id), esc(&text(&bead["title"])));
    let state = text(&bead["state"]);
    let holder = text(&bead["holder"]);
    let live = if holder.is_empty() {
        "<span class=dim>no holder</span>".to_string()
    } else {
        match num(&bead["lease_until"]) {
            Some(l) if l >= now => format!("{} · <span>lease live, {} left</span>", esc(&holder), dur(l - now)),
            Some(l) => format!("{} · <span class=red>LEASE EXPIRED {} ago — holder not alive</span>", esc(&holder), dur(now - l)),
            None => format!("{} · <span class=red>no lease</span>", esc(&holder)),
        }
    };
    let holds = holds_text(&bead["holds"]);
    b.push_str(&format!("<div style=\"padding:.5rem .8rem\"><div>state <b>{}</b>{}</div><div>holder: {live}</div>", esc(&state), if holds.is_empty() { String::new() } else { format!(" · holds <span class=badge>{}</span>", esc(&holds)) }));
    let texts: Vec<String> = ["gate_key", "reason"].iter().map(|k| text(&bead[*k])).chain([holds_text(&bead["holds"]), bead["holds"].to_string()]).collect();
    let links = blockers(id, &texts);
    if !links.is_empty() {
        b.push_str("<div>blockers: ");
        for l in &links {
            b.push_str(&format!("<a class=id href=\"/stuck/{0}\" style=\"text-decoration:underline\">{0}</a> ", esc(l)));
        }
        b.push_str("</div>");
    }
    b.push_str("</div>");
    // Newest first. An applied move that stays in its state (a lease Renew, a note) is detail of
    // the move that entered the state, so it folds under it; a refusal stays on its own line.
    let mut evs: Vec<&Value> = detail["events"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    evs.sort_by_key(|e| num(&e["seq"]).unwrap_or(0));
    let mut groups: Vec<(&Value, Vec<&Value>)> = Vec::new();
    let mut last_entry: Option<usize> = None;
    for e in evs {
        let applied = num(&e["applied"]) != Some(0);
        let stays = text(&e["from_state"]) == text(&e["to_state"]);
        match last_entry {
            Some(g) if applied && stays => groups[g].1.push(e),
            _ => {
                groups.push((e, Vec::new()));
                if applied {
                    last_entry = Some(groups.len() - 1);
                }
            }
        }
    }
    let event_row = |e: &Value| {
        let refused = num(&e["applied"]) == Some(0);
        format!(
            "<tr class=\"{}\"><td class=dim>{} ago</td><td>{}<br><span class=dim>{}</span></td><td>{} → {}{}</td></tr>",
            if refused { "refused" } else { "" },
            dur(now - num(&e["at"]).unwrap_or(0)),
            esc(&text(&e["event"])),
            esc(&text(&e["actor"])),
            esc(&text(&e["from_state"])),
            esc(&text(&e["to_state"])),
            if refused { format!("<br>refused: {}", esc(&text(&e["refusal"]))) } else { String::new() },
        )
    };
    b.push_str("<table>");
    for (head, kids) in groups.iter().rev() {
        b.push_str(&event_row(head));
        if kids.is_empty() {
            continue;
        }
        let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
        for k in kids {
            *kinds.entry(text(&k["event"])).or_default() += 1;
        }
        let what = kinds.iter().map(|(k, n)| format!("{}×{n}", esc(k))).collect::<Vec<_>>().join(" · ");
        let latest = kids.iter().filter_map(|k| num(&k["at"])).max().unwrap_or(0);
        b.push_str(&format!(
            "<tr><td></td><td colspan=2><details><summary class=dim>{} update(s) in {} — {what}, latest {} ago</summary><table>",
            kids.len(),
            esc(&text(&head["to_state"])),
            dur(now - latest)
        ));
        for k in kids.iter().rev() {
            b.push_str(&event_row(k));
        }
        b.push_str("</table></details></td></tr>");
    }
    b.push_str("</table>");
    shell(id, 30, &b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_000_000;
    const H: i64 = 3600;

    fn ev(seq: i64, id: &str, from: &str, to: &str, ago: i64) -> Value {
        json!({"seq": seq, "bead_id": id, "from_state": from, "to_state": to, "at": NOW - ago})
    }
    fn live(id: &str, state: &str, ago: i64, p95: i64, p: i64) -> Value {
        json!({"bead_id": id, "state": state, "holds": "[]", "priority": p, "title": format!("t {id}"), "entered_at": NOW - ago, "p95_s": p95, "samples": 40})
    }

    /// The cases that hid: certified with no exit, orphaned in WORKING, stranded IN_DELIVERY.
    fn intent() -> Vec<Row> {
        let p95 = p95_map(&[json!({"state": "CERTIFIED", "p95_s": 2 * H}), json!({"state": "WORKING", "p95_s": 3 * H}), json!({"state": "IN_DELIVERY", "p95_s": H}), json!({"state": "READY", "p95_s": 4 * H}), json!({"state": "SUBMITTED", "p95_s": H})]);
        let events = vec![
            ev(1, "sp-2cah6", "WORKING", "SUBMITTED", 22 * H), ev(2, "sp-2cah6", "SUBMITTED", "CERTIFIED", 20 * H),
            ev(3, "sp-h4umcj", "READY", "WORKING", 9 * H),
            ev(4, "sp-strand", "CERTIFIED", "IN_DELIVERY", 5 * H),
            ev(5, "sp-fine", "WORKING", "SUBMITTED", H / 2), ev(6, "sp-landed", "IN_DELIVERY", "LANDED", H),
        ];
        let dwell = vec![live("sp-2cah6", "CERTIFIED", 20 * H, 2 * H, 1), live("sp-h4umcj", "WORKING", 9 * H, 3 * H, 1), live("sp-strand", "IN_DELIVERY", 5 * H, H, 2), live("sp-fine", "SUBMITTED", H / 2, H, 2), live("sp-old", "READY", 40 * H, 4 * H, 0)];
        build(NOW, &events, &dwell, &p95)
    }

    #[test]
    fn each_stuck_case_is_on_top_and_the_healthy_rows_are_not() {
        let rows = intent();
        let ids: Vec<&str> = rows.iter().map(|r| r.bead_id.as_str()).collect();
        let stuck: BTreeSet<&str> = rows.iter().filter(|r| r.stuck).map(|r| r.bead_id.as_str()).collect();
        assert_eq!(stuck, BTreeSet::from(["sp-2cah6", "sp-h4umcj", "sp-strand", "sp-old"]), "{ids:?}");
        assert!(ids[..4].iter().all(|i| stuck.contains(i)), "stuck rows lead: {ids:?}");
        assert!(rows.iter().find(|r| r.bead_id == "sp-fine").is_some_and(|r| !r.stuck));
        assert!(rows.iter().find(|r| r.bead_id == "sp-landed").is_some_and(|r| !r.stuck), "a landed bead is never stuck");
    }

    #[test]
    fn a_bar_is_outlined_only_past_its_own_states_p95() {
        let rows = intent();
        let r = rows.iter().find(|r| r.bead_id == "sp-2cah6").unwrap();
        let by: Vec<(&str, bool)> = r.segs.iter().map(|s| (s.state.as_str(), s.over)).collect();
        assert_eq!(by, [("WORKING", false), ("SUBMITTED", true), ("CERTIFIED", true)], "WORKING 2h < 3h p95; SUBMITTED 2h > 1h");
    }

    #[test]
    fn the_page_outlines_the_stuck_bars_and_links_every_row() {
        let html = render_page(NOW, &intent());
        assert!(html.contains("seg s-CERTIFIED over"));
        assert!(html.contains("href=\"/stuck/sp-h4umcj\""));
        assert!(html.contains("width=device-width"));
        let quiet = render_page(NOW, &[]);
        assert!(!quiet.contains(" over\""), "positive control: no stuck row, no outline");
    }

    #[test]
    fn blockers_are_the_same_family_ids_the_bead_names() {
        let t = vec!["blocked by sp-abcd12 and sp-zzzz1; see sp-self9 (not in-progress)".to_string()];
        assert_eq!(blockers("sp-self9", &t), ["sp-abcd12", "sp-zzzz1"]);
        assert!(blockers("sp-self9", &["nothing here".into()]).is_empty());
    }

    #[test]
    fn the_timeline_shows_refusals_in_red_and_an_expired_lease() {
        let d = json!({"bead": {"state": "WORKING", "holder": "aeon-1", "lease_until": NOW - 600, "holds": "[]", "title": "x", "gate_key": "waits on sp-abcd12"},
            "events": [{"seq": 1, "event": "Claim", "from_state": "READY", "to_state": "WORKING", "applied": 1, "actor": "a", "at": NOW - 900},
                       {"seq": 2, "event": "Certify", "from_state": "WORKING", "to_state": "WORKING", "applied": 0, "refusal": "wrong-state", "actor": "b", "at": NOW - 60}]});
        let html = render_bead(NOW, "sp-self9", &d);
        assert!(html.contains("tr class=\"refused\"") && html.contains("refused: wrong-state"));
        assert!(html.contains("LEASE EXPIRED 10m ago"));
        assert!(html.contains("href=\"/stuck/sp-abcd12\""));
    }

    #[test]
    fn same_state_moves_fold_under_their_entry_and_the_newest_comes_first() {
        let e = |seq: i64, event: &str, from: &str, to: &str, applied: i64| {
            json!({"seq": seq, "event": event, "from_state": from, "to_state": to, "applied": applied, "actor": "a", "at": NOW - 1000 + seq * 10})
        };
        let d = json!({"bead": {"state": "SUBMITTED", "holds": "[]", "title": "x"},
            "events": [e(1, "Claim", "READY", "WORKING", 1), e(2, "Renew", "WORKING", "WORKING", 1), e(3, "Renew", "WORKING", "WORKING", 1),
                       e(4, "Certify", "WORKING", "WORKING", 0), e(5, "Renew", "WORKING", "WORKING", 1), e(6, "Submit", "WORKING", "SUBMITTED", 1)]});
        let html = render_bead(NOW, "sp-fold1", &d);
        assert!(html.contains("3 update(s) in WORKING — Renew×3"), "{html}");
        let (submit, refusal, claim) = (html.find(">Submit<").unwrap(), html.find(">Certify<").unwrap(), html.find(">Claim<").unwrap());
        assert!(submit < refusal && refusal < claim, "newest first, with the refusal on its own line");
        assert!(html.find("update(s) in WORKING").unwrap() > claim, "the folded renews sit under the Claim that entered WORKING");
    }
}
