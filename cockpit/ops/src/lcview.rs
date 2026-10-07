//! lc-view — the operator's view of the world through the lifecycle state machine (sp-lpw5ol).
//!
//! The ops pane used to read bead state from `bd`, whose status lags the lifecycle both ways
//! (2026-10-06: 73 beads lifecycle-READY with their own commits already on main; landed work
//! shown as ready P0s). This view reads state ONLY from `spira-lc`; `work list` supplies the
//! title and priority it cannot, and the landing ref's own commits reveal drift (a bead the
//! lifecycle still calls READY/REWORK whose own commit is on the base). It never runs `bd`.
//!
//! Everything below the `Snapshot` is pure, so the frame is tested against fixtures.

use std::collections::{BTreeMap, HashMap};

/// One lifecycle row, as `spira-lc list` prints it.
#[derive(Debug, Clone, Default)]
pub struct Row {
    pub id: String,
    pub state: String,
    pub holder: Option<String>,
    pub holds: Vec<String>,
    pub reason: Option<String>,
    pub updated_at: i64,
    pub since: i64,
    pub lease_until: Option<i64>,
}

/// What `work list` knows that the lifecycle does not.
#[derive(Debug, Clone, Default)]
pub struct Meta {
    pub title: String,
    pub priority: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub now: i64,
    pub release: String,
    pub world: String,
    pub base: String,
    pub rows: Vec<Row>,
    pub meta: HashMap<String, Meta>,
    /// Bead ids whose own commit (`<id>:` subject, or a round's `merge <id> (`) is on the base.
    pub on_base: HashMap<String, String>,
    pub ceiling: usize,
    /// Sources that failed this pass, named in the frame — never a silent empty section.
    pub errors: Vec<String>,
}

pub const FLOW: [&str; 6] = ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"];
pub const SIDE: [&str; 3] = ["REWORK", "DROPPED", "SUPERSEDED"];

/// The ids a commit subject names as its own work: `sp-x: …` or `… merge sp-x (…`.
pub fn own_ids(subject: &str) -> Vec<String> {
    let mut out = Vec::new();
    let id_at = |s: &str| -> Option<String> {
        let s = s.strip_prefix("sp-")?;
        let n: String = s.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '.').collect();
        let n = n.trim_end_matches('.');
        (!n.is_empty()).then(|| format!("sp-{n}"))
    };
    if let Some((head, _)) = subject.split_once(':') {
        if !head.contains(' ') {
            if let Some(id) = id_at(head) {
                if id.len() == head.len() {
                    out.push(id);
                }
            }
        }
    }
    let mut rest = subject;
    while let Some(i) = rest.find("merge sp-") {
        let tail = &rest[i + "merge ".len()..];
        if let Some(id) = id_at(tail) {
            if tail[id.len()..].starts_with(" (") {
                out.push(id);
            }
        }
        rest = &rest[i + 1..];
    }
    out
}

fn age(secs: i64) -> String {
    let s = secs.max(0);
    if s < 90 {
        format!("{s}s")
    } else if s < 5400 {
        format!("{}m", s / 60)
    } else if s < 172_800 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86400)
    }
}

fn cut(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        format!("{}…", t.chars().take(n.saturating_sub(1)).collect::<String>())
    } else {
        t
    }
}

fn prio(m: Option<&Meta>) -> String {
    m.and_then(|m| m.priority).map(|p| format!("P{p}")).unwrap_or_else(|| "P?".into())
}

fn title(m: Option<&Meta>) -> String {
    m.map(|m| m.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| "(no title: not an open bead)".into())
}

/// Colour helpers — plain ANSI so the pane stays a pipe-able text frame.
const B: &str = "\x1b[1m";
const D: &str = "\x1b[2m";
const R: &str = "\x1b[0m";
const RED: &str = "\x1b[31m";
const YEL: &str = "\x1b[33m";
const GRN: &str = "\x1b[32m";
const CYN: &str = "\x1b[36m";

/// The whole frame, top to bottom.
pub fn render(s: &Snapshot, width: usize) -> Vec<String> {
    let w = width.max(60);
    let mut out = Vec::new();
    let by_state = |st: &str| -> Vec<&Row> { s.rows.iter().filter(|r| r.state == st).collect() };
    let count = |st: &str| s.rows.iter().filter(|r| r.state == st).count();
    let day = |r: &Row| s.now - r.updated_at < 86_400;

    // ── header
    let working = count("WORKING");
    out.push(format!(
        "{B}LIFECYCLE{R} {D}{}{R}  release {B}{}{R}  {}  aeons {B}{working}/{}{R}",
        clock(s.now),
        cut(&s.release, 9),
        if s.world.contains("RUNNING") { format!("{GRN}world RUNNING{R}") } else { format!("{RED}world {}{R}", cut(&s.world, 30)) },
        s.ceiling
    ));
    for e in &s.errors {
        out.push(format!("{RED}  source failed: {}{R}", cut(e, w - 18)));
    }

    // ── the state machine, as a flow
    let ready = by_state("READY");
    let held: Vec<&&Row> = ready.iter().filter(|r| !r.holds.is_empty()).collect();
    let landed_24h = s.rows.iter().filter(|r| r.state == "LANDED" && day(r)).count();
    let mut flow = format!("{B}FLOW{R}   READY {B}{}{R}", ready.len());
    if !held.is_empty() {
        let pct = held.len() * 100 / ready.len().max(1);
        let c = if pct >= 25 { RED } else { YEL };
        flow.push_str(&format!(" {c}({} held, {pct}%){R}", held.len()));
    }
    for st in &FLOW[1..5] {
        flow.push_str(&format!(" → {st} {B}{}{R}", count(st)));
    }
    flow.push_str(&format!(" → LANDED {B}{landed_24h}{R}{D}/24h{R}"));
    out.push(flow);
    out.push(format!(
        "       {YEL}REWORK {}{R}  {D}DROPPED {}/24h · SUPERSEDED {}/24h · LANDED {} all time{R}",
        count("REWORK"),
        s.rows.iter().filter(|r| r.state == "DROPPED" && day(r)).count(),
        s.rows.iter().filter(|r| r.state == "SUPERSEDED" && day(r)).count(),
        count("LANDED")
    ));

    // ── holds, grouped by kind and reason: a starved queue shows here first
    let mut by_kind: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for r in s.rows.iter().filter(|r| !r.holds.is_empty() && !matches!(r.state.as_str(), "LANDED" | "DROPPED" | "SUPERSEDED")) {
        for k in &r.holds {
            let reason = r.reason.clone().unwrap_or_default();
            *by_kind.entry(k.clone()).or_default().entry(cut(&reason, 60)).or_default() += 1;
        }
    }
    if !by_kind.is_empty() {
        out.push(format!("{B}HOLDS{R}"));
        for (k, reasons) in &by_kind {
            let n: usize = reasons.values().sum();
            let mut top: Vec<_> = reasons.iter().collect();
            top.sort_by(|a, b| b.1.cmp(a.1));
            let c = if k == "poison" { RED } else { YEL };
            out.push(format!("  {c}{k:<9}{R}{B}{n:>4}{R}  {}", top.iter().take(2).map(|(r, n)| format!("{n}× {}", if r.is_empty() { "(no reason)" } else { r })).collect::<Vec<_>>().join(" · ")));
        }
    }

    // ── drift: the lifecycle disagrees with the base
    let mut drift: Vec<&Row> = s.rows.iter().filter(|r| matches!(r.state.as_str(), "READY" | "REWORK") && s.on_base.contains_key(&r.id)).collect();
    drift.sort_by(|a, b| a.id.cmp(&b.id));
    if !drift.is_empty() {
        out.push(format!(
            "{RED}{B}DRIFT{R} {RED}{} bead(s) READY/REWORK whose own commit is on {}:{R} {}",
            drift.len(),
            s.base,
            drift.iter().take(6).map(|r| r.id.clone()).collect::<Vec<_>>().join(" ")
        ));
    }

    // ── now: what is being worked
    out.push(format!("{B}NOW{R}    {D}WORKING — holder · bead · lease{R}"));
    let mut wk = by_state("WORKING");
    wk.sort_by_key(|r| r.since);
    if wk.is_empty() {
        out.push(format!("       {D}nothing is being worked{R}"));
    }
    for r in wk {
        let m = s.meta.get(&r.id);
        let lease = r.lease_until.map(|l| if l > s.now { format!("{}", age(l - s.now)) } else { format!("{RED}expired{R}") }).unwrap_or_else(|| "-".into());
        out.push(format!(
            "  {CYN}{:<9}{R} {:<12} {} {:<w2$} {D}{} · lease {}{R}",
            cut(r.holder.as_deref().unwrap_or("?").trim_start_matches("aeon-"), 9),
            r.id,
            prio(m),
            cut(&title(m), w.saturating_sub(48)),
            age(s.now - r.since),
            lease,
            w2 = w.saturating_sub(48).min(70)
        ));
    }

    // ── pipeline: finished work waiting on the machine
    out.push(format!("{B}PIPE{R}   {D}SUBMITTED waits on a gate · CERTIFIED on a round · IN_DELIVERY in one{R}"));
    for st in ["SUBMITTED", "CERTIFIED", "IN_DELIVERY"] {
        let mut v = by_state(st);
        v.sort_by_key(|r| r.since);
        if v.is_empty() {
            continue;
        }
        let oldest = age(s.now - v[0].since);
        out.push(format!(
            "  {:<11} {B}{:>3}{R} {D}oldest {oldest}{R}  {}",
            st,
            v.len(),
            v.iter().take(6).map(|r| format!("{} {D}{}{R}", r.id, age(s.now - r.since))).collect::<Vec<_>>().join("  ")
        ));
    }

    // ── rework: sent back, and why
    let mut rw = by_state("REWORK");
    rw.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
    if !rw.is_empty() {
        out.push(format!("{B}REWORK{R} {D}sent back — bead · why{R}"));
        for r in rw.iter().take(6) {
            out.push(format!("  {YEL}{:<12}{R} {} {D}{}{R} {}", r.id, prio(s.meta.get(&r.id)), age(s.now - r.updated_at), cut(r.reason.as_deref().unwrap_or("-"), w.saturating_sub(30))));
        }
        if rw.len() > 6 {
            out.push(format!("  {D}… {} more{R}", rw.len() - 6));
        }
    }

    // ── next: claimable, by priority then age
    let mut nx: Vec<&Row> = ready.iter().copied().filter(|r| r.holds.is_empty() && !s.on_base.contains_key(&r.id)).collect();
    nx.sort_by_key(|r| (s.meta.get(&r.id).and_then(|m| m.priority).unwrap_or(9), r.since));
    out.push(format!("{B}NEXT{R}   {B}{}{R} claimable {D}(READY, unheld, not on the base) — by priority{R}", nx.len()));
    for r in nx.iter().take(10) {
        let m = s.meta.get(&r.id);
        out.push(format!("  {} {:<12} {}", prio(m), r.id, cut(&title(m), w.saturating_sub(20))));
    }

    // ── recent transitions
    let mut rc: Vec<&Row> = s.rows.iter().collect();
    rc.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
    out.push(format!("{B}RECENT{R} {D}last transitions — when · bead · now{R}"));
    for r in rc.iter().take(8) {
        let c = match r.state.as_str() {
            "LANDED" => GRN,
            "REWORK" | "DROPPED" => YEL,
            _ => "",
        };
        out.push(format!(
            "  {D}{:>4}{R} {:<12} {c}{:<11}{R} {}",
            age(s.now - r.updated_at),
            r.id,
            r.state,
            cut(&title(s.meta.get(&r.id)), w.saturating_sub(34))
        ));
    }
    out.push(format!("{D}source: spira-lc (state) · work list (titles) · {} commits (drift) — no bd{R}", s.base));
    out
}

fn clock(epoch: i64) -> String {
    let s = epoch.rem_euclid(86_400);
    format!("{:02}:{:02}Z", s / 3600, (s % 3600) / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, st: &str, since: i64) -> Row {
        Row { id: id.into(), state: st.into(), since, updated_at: since, ..Default::default() }
    }

    fn snap(rows: Vec<Row>) -> Snapshot {
        Snapshot { now: 100_000, release: "abc".into(), world: "plane work: RUNNING".into(), base: "local/main".into(), rows, ceiling: 6, ..Default::default() }
    }

    fn plain(lines: &[String]) -> String {
        let re = regex::Regex::new("\x1b\\[[0-9;]*m").unwrap();
        lines.iter().map(|l| re.replace_all(l, "").into_owned()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn own_ids_reads_a_bead_commit_and_a_round_merge() {
        assert_eq!(own_ids("sp-ab12: fix the thing"), vec!["sp-ab12"]);
        assert_eq!(own_ids("spira: round r-35: merge sp-v70e42 (38569ce9)"), vec!["sp-v70e42"]);
        assert_eq!(own_ids("sp-aflyc.2: sim world stores"), vec!["sp-aflyc.2"]);
        assert!(own_ids("repoint sp-x to the new path").is_empty(), "a mention is not the bead's own work");
        assert!(own_ids("gate: cache fences").is_empty());
    }

    #[test]
    fn next_is_lifecycle_ready_unheld_and_by_priority() {
        let mut held = row("sp-held", "READY", 10);
        held.holds = vec!["ask".into()];
        let mut s = snap(vec![row("sp-low", "READY", 1), row("sp-high", "READY", 5), held, row("sp-landed", "LANDED", 3)]);
        s.meta.insert("sp-low".into(), Meta { title: "low".into(), priority: Some(2) });
        s.meta.insert("sp-high".into(), Meta { title: "high".into(), priority: Some(0) });
        // bd might call sp-landed open and sp-high closed — the view never asks bd.
        let f = plain(&render(&s, 120));
        let next = f.split("NEXT").nth(1).unwrap().split("RECENT").next().unwrap();
        assert!(next.contains("2 claimable"), "{next}");
        let hi = next.find("sp-high").unwrap();
        let lo = next.find("sp-low").unwrap();
        assert!(hi < lo, "P0 before P2:\n{next}");
        assert!(!next.contains("sp-held") && !next.contains("sp-landed"), "{next}");
    }

    #[test]
    fn a_ready_bead_whose_commit_is_on_the_base_is_drift_not_next() {
        let mut s = snap(vec![row("sp-done", "READY", 1)]);
        s.on_base.insert("sp-done".into(), "abc123".into());
        let f = plain(&render(&s, 120));
        assert!(f.contains("DRIFT 1 bead(s)") && f.contains("sp-done"), "{f}");
        assert!(f.contains("0 claimable"), "{f}");
    }

    #[test]
    fn a_starved_queue_shows_its_held_share_and_reasons() {
        let mut rows = vec![row("sp-a", "READY", 1)];
        for i in 0..3 {
            let mut r = row(&format!("sp-h{i}"), "READY", 1);
            r.holds = vec!["ask".into()];
            r.reason = Some("repo:spira has no repo-map entry".into());
            rows.push(r);
        }
        let f = plain(&render(&snap(rows), 120));
        assert!(f.contains("READY 4 (3 held, 75%)"), "{f}");
        assert!(f.contains("ask") && f.contains("3× repo:spira has no repo-map entry"), "{f}");
    }

    #[test]
    fn a_failed_source_is_named_never_an_empty_frame() {
        let mut s = snap(vec![]);
        s.errors.push("spira-lc list: exit 1".into());
        assert!(plain(&render(&s, 100)).contains("source failed: spira-lc list: exit 1"));
    }
}
