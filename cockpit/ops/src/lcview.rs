//! lc-view — the operator's view of the world through the lifecycle state machine (sp-lpw5ol).
//!
//! The ops pane used to read bead state from `bd`, whose status lags the lifecycle both ways
//! (2026-10-06: 73 beads lifecycle-READY with their own commits already on main; landed work
//! shown as ready P0s). This view reads state ONLY from `spira-lc`; `work list` supplies the
//! title and priority it cannot, and the landing ref's own commits reveal drift (a bead the
//! lifecycle still calls READY/REWORK whose own commit is on the base). It never runs `bd`.
//!
//! One `Snapshot` (what was gathered) → one `View` (everything derived: counts, groups,
//! orderings) → two renderers: the terminal pane (`render`) and the phone page (`render_html`,
//! served by loom from the snapshot file the pane writes). Both draw the same `View`, so the
//! pane and the page cannot disagree about a number. Everything below the gather is pure.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// One lifecycle row, as `spira-lc list` prints it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Row {
    pub id: String,
    pub state: String,
    pub holder: Option<String>,
    pub holds: Vec<String>,
    pub reason: Option<String>,
    pub updated_at: i64,
    pub since: i64,
    pub lease_until: Option<i64>,
    #[serde(default)]
    pub persona: Option<String>,
    #[serde(default)]
    pub rework: bool,
    #[serde(default)]
    pub claimable: Option<bool>,
    #[serde(default)]
    pub blocker: Option<String>,
}

/// What `work list` knows that the lifecycle does not.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Meta {
    pub title: String,
    pub priority: Option<i64>,
}

/// The tail of one aeon's session log: its last meaningful lines and the log's mtime.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Tail {
    pub lines: Vec<String>,
    pub mtime: i64,
}

pub const TAIL_BYTES: u64 = 64 * 1024;
pub const STALE_WARN_S: i64 = 300;
pub const STALE_BAD_S: i64 = 900;

/// The last `n` assistant text / tool-call lines of stream-json `bytes`, oldest first. `partial`
/// says the bytes start mid-file, so the first line may be cut and is dropped.
pub fn tail_lines(bytes: &[u8], partial: bool, n: usize) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut lines = text.lines();
    if partial {
        lines.next();
    }
    let mut found: Vec<String> = Vec::new();
    for l in lines.collect::<Vec<_>>().into_iter().rev() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(l) else { continue };
        if v["type"] != "assistant" {
            continue;
        }
        let Some(blocks) = v["message"]["content"].as_array() else { continue };
        for b in blocks.iter().rev() {
            let line = match b["type"].as_str() {
                Some("text") => b["text"].as_str().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" "),
                Some("tool_use") => {
                    let input = &b["input"];
                    let arg = ["command", "file_path", "pattern", "path", "description", "prompt"]
                        .iter()
                        .find_map(|k| input[*k].as_str())
                        .unwrap_or("");
                    format!("▸ {} {}", b["name"].as_str().unwrap_or("?"), arg.split_whitespace().collect::<Vec<_>>().join(" "))
                        .trim_end()
                        .to_string()
                }
                _ => continue,
            };
            if !line.is_empty() {
                found.push(line);
                if found.len() == n {
                    found.reverse();
                    return found;
                }
            }
        }
    }
    found.reverse();
    found
}

/// One legal move of the bead machine, as `spira-lc ops-graph` prints it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphEdge {
    pub from: String,
    pub event: String,
    pub to: String,
}

/// One `ops_edges` row: an applied move (`to_state`) or a refused event (`event`, `refusal`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EdgeRow {
    pub kind: String,
    pub from_state: String,
    pub to_state: Option<String>,
    pub event: Option<String>,
    pub refusal: Option<String>,
    pub n_1h: i64,
}

/// One `ops_dwell` row: when a live bead entered its state, against that state's measured p95.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DwellRow {
    pub bead_id: String,
    pub state: String,
    pub entered_at: i64,
    pub p95_s: Option<i64>,
}

/// One batch of `spira-lc list --batches`, with the beads it carries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BatchRow {
    pub id: String,
    pub state: String,
    pub last_at: i64,
    pub members: Vec<String>,
    /// When the round was opened, and the members it has ejected so far (each eject costs the
    /// round another certification pass).
    #[serde(default)]
    pub opened_at: i64,
    #[serde(default)]
    pub ejected: Vec<String>,
    /// When each eject was recorded (open rounds only); ejects minutes apart are one attribution.
    #[serde(default)]
    pub eject_at: Vec<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// Log tails of WORKING beads, by bead id; a bead with no log is absent.
    pub tails: HashMap<String, Tail>,
    /// Sources that failed this pass, named in the frame — never a silent empty section.
    pub errors: Vec<String>,
    #[serde(default)]
    pub graph: Vec<GraphEdge>,
    #[serde(default)]
    pub edges: Vec<EdgeRow>,
    #[serde(default)]
    pub dwell: Vec<DwellRow>,
    #[serde(default)]
    pub batches: Vec<BatchRow>,
}

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

// ───────────────────────────── the derived view (shared by both renderers)

#[derive(Debug, Clone, Default, Serialize)]
pub struct Item {
    pub id: String,
    pub prio: String,
    pub title: String,
    pub who: String,
    pub age: String,
    pub note: String,
    pub state: String,
    pub persona: String,
    pub rework: bool,
    /// The aeon's last output lines, how old, and ok|warn|bad|none.
    pub out: Vec<String>,
    pub out_age: String,
    pub out_level: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct HoldGroup {
    pub kind: String,
    pub count: usize,
    pub top: Vec<(usize, String)>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PipeLine {
    pub state: String,
    pub count: usize,
    pub oldest: String,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SmEdge {
    pub event: String,
    /// Each event behind this move, with its refusals from this state in the last hour and the
    /// newest refusal's reason (the store counts applied moves per edge, not per event).
    #[serde(default)]
    pub events: Vec<(String, i64, String)>,
    pub to: String,
    pub rate_1h: i64,
    pub main: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SmState {
    pub name: String,
    pub count: usize,
    pub detail: String,
    pub red: bool,
    pub edges: Vec<SmEdge>,
    /// The graph has no move from this state to REWORK, though work here can come back.
    pub no_rework_exit: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SmRefusal {
    pub what: String,
    pub n: i64,
    pub why: String,
    pub red: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SmBatch {
    pub name: String,
    pub count: usize,
    pub detail: String,
    pub red: bool,
}

/// The open landing round, the first thing the operator looks for (Ryan, 2026-10-09: "what's in
/// the round? ... i can't see what it's called ... how many attempts ... how long it's been running").
#[derive(Debug, Clone, Default, Serialize)]
pub struct RoundView {
    pub name: String,
    pub state: String,
    pub age: String,
    /// Certification passes so far: one, plus one per eject.
    pub passes: usize,
    pub ejected: Vec<String>,
    pub members: Vec<RoundMember>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RoundMember {
    pub id: String,
    pub prio: String,
    pub title: String,
    /// How many recent rounds have ejected this bead.
    pub ejects: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct View {
    pub clock: String,
    pub release: String,
    pub world: String,
    pub world_running: bool,
    pub working: usize,
    pub ceiling: usize,
    pub errors: Vec<String>,
    /// FLOW: (state, count) in pipeline order, LANDED as its 24 h count.
    pub flow: Vec<(String, usize)>,
    pub ready: usize,
    pub ready_held: usize,
    pub held_pct: usize,
    pub rework: usize,
    pub dropped_24h: usize,
    pub superseded_24h: usize,
    pub landed_total: usize,
    pub holds: Vec<HoldGroup>,
    pub base: String,
    pub drift: Vec<String>,
    pub now_items: Vec<Item>,
    pub pipe: Vec<PipeLine>,
    pub rework_items: Vec<Item>,
    pub next_count: usize,
    pub next: Vec<Item>,
    pub blocked: Vec<Item>,
    pub recent: Vec<Item>,
    pub machine: Vec<SmState>,
    pub terminal: String,
    pub refused: Vec<SmRefusal>,
    pub batch: Vec<SmBatch>,
    pub round: Option<RoundView>,
}

const CHAIN: [&str; 7] = ["OPEN", "READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"];
const TERMINAL: [&str; 3] = ["SUPERSEDED", "DROPPED", "DONE"];
const NEEDS_REWORK_EXIT: [&str; 3] = ["SUBMITTED", "CERTIFIED", "IN_DELIVERY"];
const OPEN_BATCH: [&str; 5] = ["OPEN", "CI_RUNNING", "GREEN", "ATTRIBUTING", "REBUILDING"];
const REFUSED_SHOWN: usize = 4;
const REPEATED_REFUSALS: i64 = 2;

fn kebab(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn bead_ids_in(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.')).to_string())
        .filter(|w| w.starts_with("sp-") && w.len() > 3)
        .collect()
}

fn join_parts(parts: Vec<String>) -> String {
    parts.join(" · ")
}

fn state_machine(s: &Snapshot, on_base: &dyn Fn(&str) -> bool) -> Vec<SmState> {
    let in_batch: std::collections::HashSet<&str> =
        s.batches.iter().filter(|b| OPEN_BATCH.contains(&b.state.as_str())).flat_map(|b| b.members.iter().map(String::as_str)).collect();
    let day = |r: &Row| s.now - r.updated_at < 86_400;
    let mut out = Vec::new();
    for name in CHAIN.iter().copied().chain(["REWORK"]) {
        let rows: Vec<&Row> = s.rows.iter().filter(|r| r.state == name).collect();
        let count = if name == "LANDED" { rows.iter().filter(|r| day(r)).count() } else { rows.len() };
        let mut parts: Vec<String> = Vec::new();
        let mut red = false;
        if matches!(name, "READY" | "REWORK") {
            let (mut claimable, mut blocked, mut held, mut poison) = (0, 0, 0, 0);
            let mut blockers: BTreeMap<String, usize> = BTreeMap::new();
            for r in &rows {
                if r.holds.iter().any(|h| h == "poison") {
                    poison += 1;
                } else if r.holds.iter().any(|h| h == "wait") {
                    blocked += 1;
                    for b in bead_ids_in(r.reason.as_deref().unwrap_or("")) {
                        *blockers.entry(b).or_default() += 1;
                    }
                } else if !r.holds.is_empty() {
                    held += 1;
                } else if !on_base(&r.id) {
                    claimable += 1;
                }
            }
            parts.push(format!("claimable {claimable}"));
            if blocked > 0 {
                let mut top: Vec<(usize, String)> = blockers.into_iter().map(|(b, n)| (n, b)).collect();
                top.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
                let names = top.iter().take(2).map(|(_, b)| b.clone()).collect::<Vec<_>>().join(" ");
                parts.push(if names.is_empty() { format!("blocked {blocked}") } else { format!("blocked {blocked} ← {names}") });
            }
            for (label, n) in [("held", held), ("poison", poison)] {
                if n > 0 {
                    parts.push(format!("{label} {n}"));
                }
            }
        }
        if name == "WORKING" {
            let dead = rows.iter().filter(|r| r.holder.is_none() || r.lease_until.is_some_and(|l| l <= s.now)).count();
            if dead > 0 {
                parts.push(format!("dead holder {dead}"));
                red = true;
            }
        }
        if name == "SUBMITTED" {
            let skipped = rows.iter().filter(|r| r.reason.as_deref().is_some_and(|x| x.to_lowercase().contains("conflict"))).count();
            if skipped > 0 {
                parts.push(format!("skipped: conflict {skipped}"));
            }
        }
        if name == "IN_DELIVERY" {
            let inside = rows.iter().filter(|r| in_batch.contains(r.id.as_str())).count();
            parts.push(format!("in open batch {inside} · orphaned {}", rows.len() - inside));
            red |= inside < rows.len();
        }
        if name == "LANDED" {
            parts.push("last 24h".into());
        }
        let stuck = s.dwell.iter().filter(|d| d.state == name && d.p95_s.is_some_and(|p| s.now - d.entered_at > p)).count();
        if stuck > 0 {
            parts.push(format!("stuck >p95 {stuck}"));
            red = true;
        }
        let mut edges: Vec<SmEdge> = Vec::new();
        let next = CHAIN.iter().position(|c| *c == name).and_then(|i| CHAIN.get(i + 1)).copied();
        let mut tos: Vec<&str> = s.graph.iter().filter(|g| g.from == name && !TERMINAL.contains(&g.to.as_str())).map(|g| g.to.as_str()).collect();
        tos.sort_by_key(|t| (Some(*t) != next, *t));
        tos.dedup();
        for to in tos {
            let events: Vec<String> = s.graph.iter().filter(|g| g.from == name && g.to == to).map(|g| kebab(&g.event)).collect();
            let rate = s.edges.iter().filter(|e| e.kind == "applied" && e.from_state == name && e.to_state.as_deref() == Some(to)).map(|e| e.n_1h).sum();
            let detail = s
                .graph
                .iter()
                .filter(|g| g.from == name && g.to == to)
                .map(|g| {
                    let refused: Vec<&EdgeRow> =
                        s.edges.iter().filter(|e| e.kind == "refused" && e.from_state == name && e.event.as_deref().is_some_and(|x| kebab(x) == kebab(&g.event))).collect();
                    let why = refused.iter().find_map(|e| e.refusal.clone()).unwrap_or_default();
                    (kebab(&g.event), refused.iter().map(|e| e.n_1h).sum(), why)
                })
                .collect();
            edges.push(SmEdge { event: events.join("/"), events: detail, to: to.into(), rate_1h: rate, main: Some(to) == next });
        }
        let no_rework_exit = NEEDS_REWORK_EXIT.contains(&name) && !s.graph.is_empty() && !s.graph.iter().any(|g| g.from == name && g.to == "REWORK");
        out.push(SmState { name: name.into(), count, detail: join_parts(parts), red, edges, no_rework_exit });
    }
    out
}

fn refusals(s: &Snapshot) -> Vec<SmRefusal> {
    let mut by: BTreeMap<(String, String), (i64, String)> = BTreeMap::new();
    for e in s.edges.iter().filter(|e| e.kind == "refused" && e.n_1h > 0) {
        let slot = by.entry((e.from_state.clone(), e.event.clone().unwrap_or_default())).or_insert((0, e.refusal.clone().unwrap_or_default()));
        slot.0 += e.n_1h;
    }
    let mut v: Vec<SmRefusal> = by
        .into_iter()
        .map(|((state, event), (n, why))| SmRefusal { what: format!("{event}@{state}"), n, why, red: n >= REPEATED_REFUSALS })
        .collect();
    v.sort_by(|a, b| b.n.cmp(&a.n).then(a.what.cmp(&b.what)));
    v.truncate(REFUSED_SHOWN);
    v
}

fn batch_block(s: &Snapshot) -> Vec<SmBatch> {
    let green_limit = s.dwell.iter().find(|d| d.state == "IN_DELIVERY").and_then(|d| d.p95_s);
    let of = |st: &str| -> Vec<&BatchRow> { s.batches.iter().filter(|b| b.state == st).collect() };
    let mut out = Vec::new();
    for (name, st) in [("OPEN", "OPEN"), ("CI", "CI_RUNNING"), ("GREEN", "GREEN"), ("ATTRIBUTING", "ATTRIBUTING"), ("REBUILDING", "REBUILDING")] {
        let bs = of(st);
        if bs.is_empty() && matches!(name, "ATTRIBUTING" | "REBUILDING") {
            continue;
        }
        let oldest = bs.iter().map(|b| b.last_at).min();
        let red = st == "GREEN" && oldest.zip(green_limit).is_some_and(|(at, p)| s.now - at > p);
        let detail = if red { format!("stuck {}", age(s.now - oldest.unwrap_or(s.now))) } else { String::new() };
        out.push(SmBatch { name: name.into(), count: bs.len(), detail, red });
    }
    out.push(SmBatch { name: "LANDED".into(), count: of("LANDED").iter().filter(|b| s.now - b.last_at < 86_400).count(), detail: "last 24h".into(), red: false });
    out
}

/// Certification passes a round has taken, as the store knows them: the first, plus one per
/// attribution (ejects within ten minutes of each other were decided from one red pass).
pub fn passes(eject_at: &[i64]) -> usize {
    let mut at = eject_at.to_vec();
    at.sort();
    1 + at.windows(2).filter(|w| w[1] - w[0] > 600).count() + usize::from(!at.is_empty())
}

pub fn age(secs: i64) -> String {
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

pub fn cut(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>())
    } else {
        s.to_string()
    }
}

fn clock(epoch: i64) -> String {
    let s = epoch.rem_euclid(86_400);
    format!("{:02}:{:02}Z", s / 3600, (s % 3600) / 60)
}

pub fn view(s: &Snapshot) -> View {
    let count = |st: &str| s.rows.iter().filter(|r| r.state == st).count();
    let day = |r: &Row| s.now - r.updated_at < 86_400;
    let prio = |id: &str| s.meta.get(id).and_then(|m| m.priority).map(|p| format!("P{p}")).unwrap_or_else(|| "P?".into());
    let title = |id: &str| {
        s.meta.get(id).map(|m| m.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| "(no title: not an open bead)".into())
    };
    let item = |r: &Row, age_of: i64, note: String| Item {
        id: r.id.clone(),
        prio: prio(&r.id),
        title: title(&r.id),
        who: r.holder.as_deref().unwrap_or("").trim_start_matches("aeon-").to_string(),
        age: age(s.now - age_of),
        note,
        state: r.state.clone(),
        persona: r.persona.clone().filter(|p| !p.is_empty()).unwrap_or_else(|| "—".into()),
        rework: r.rework,
        ..Default::default()
    };

    let ready: Vec<&Row> = s.rows.iter().filter(|r| r.state == "READY").collect();
    let ready_held = ready.iter().filter(|r| !r.holds.is_empty()).count();
    let mut v = View {
        clock: clock(s.now),
        release: cut(&s.release, 9),
        world: s.world.clone(),
        world_running: s.world.contains("RUNNING"),
        working: count("WORKING"),
        ceiling: s.ceiling,
        errors: s.errors.clone(),
        ready: ready.len(),
        ready_held,
        held_pct: ready_held * 100 / ready.len().max(1),
        rework: count("REWORK"),
        dropped_24h: s.rows.iter().filter(|r| r.state == "DROPPED" && day(r)).count(),
        superseded_24h: s.rows.iter().filter(|r| r.state == "SUPERSEDED" && day(r)).count(),
        landed_total: count("LANDED"),
        base: s.base.clone(),
        ..Default::default()
    };
    v.flow = vec![
        ("READY".into(), v.ready),
        ("WORKING".into(), count("WORKING")),
        ("SUBMITTED".into(), count("SUBMITTED")),
        ("CERTIFIED".into(), count("CERTIFIED")),
        ("IN_DELIVERY".into(), count("IN_DELIVERY")),
        ("LANDED/24h".into(), s.rows.iter().filter(|r| r.state == "LANDED" && day(r)).count()),
    ];

    v.machine = state_machine(s, &|id| s.on_base.contains_key(id));
    v.terminal = TERMINAL
        .iter()
        .map(|t| format!("{t} {}", s.rows.iter().filter(|r| r.state == *t && day(r)).count()))
        .collect::<Vec<_>>()
        .join(" · ");
    v.refused = refusals(s);
    v.batch = batch_block(s);
    // The newest round, open or not: when none is running the pane says how the last one ended.
    v.round = s
        .batches
        .iter()
        .max_by_key(|b| b.opened_at.max(b.last_at))
        .map(|b| {
            let opened = if b.opened_at > 0 { b.opened_at } else { b.last_at };
            let ejects_of = |id: &str| s.batches.iter().filter(|o| o.ejected.iter().any(|e| e == id)).count();
            RoundView {
                name: b.id.clone(),
                state: if OPEN_BATCH.contains(&b.state.as_str()) { b.state.clone() } else { format!("{} {} ago", b.state, age(s.now - b.last_at)) },
                age: age(s.now - opened),
                passes: passes(&b.eject_at),
                ejected: b.ejected.clone(),
                members: b
                    .members
                    .iter()
                    .filter(|m| !b.ejected.contains(m))
                    .map(|m| RoundMember { id: m.clone(), prio: prio(m), title: title(m), ejects: ejects_of(m) })
                    .collect(),
            }
        });

    let mut by_kind: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for r in s.rows.iter().filter(|r| !r.holds.is_empty() && !matches!(r.state.as_str(), "LANDED" | "DROPPED" | "SUPERSEDED")) {
        for k in &r.holds {
            *by_kind.entry(k.clone()).or_default().entry(cut(r.reason.as_deref().unwrap_or("(no reason)"), 70)).or_default() += 1;
        }
    }
    v.holds = by_kind
        .into_iter()
        .map(|(kind, reasons)| {
            let count = reasons.values().sum();
            let mut top: Vec<(usize, String)> = reasons.into_iter().map(|(r, n)| (n, r)).collect();
            top.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            top.truncate(3);
            HoldGroup { kind, count, top }
        })
        .collect();

    let mut drift: Vec<String> =
        s.rows.iter().filter(|r| matches!(r.state.as_str(), "READY" | "REWORK") && s.on_base.contains_key(&r.id)).map(|r| r.id.clone()).collect();
    drift.sort();
    v.drift = drift;

    let mut wk: Vec<&Row> = s.rows.iter().filter(|r| r.state == "WORKING").collect();
    wk.sort_by_key(|r| r.since);
    v.now_items = wk
        .iter()
        .map(|r| {
            let lease = r.lease_until.map(|l| if l > s.now { format!("lease {}", age(l - s.now)) } else { "lease EXPIRED".into() }).unwrap_or_default();
            let mut it = item(r, r.since, lease);
            if r.holder.is_some() {
                match s.tails.get(&r.id) {
                    Some(t) if !t.lines.is_empty() => {
                        let a = s.now - t.mtime;
                        it.out = t.lines.clone();
                        it.out_age = age(a);
                        it.out_level = if a >= STALE_BAD_S { "bad" } else if a >= STALE_WARN_S { "warn" } else { "ok" }.into();
                    }
                    _ => {
                        it.out = vec!["no output yet".into()];
                        it.out_level = "none".into();
                    }
                }
            }
            it
        })
        .collect();

    for st in ["SUBMITTED", "CERTIFIED", "IN_DELIVERY"] {
        let mut rows: Vec<&Row> = s.rows.iter().filter(|r| r.state == st).collect();
        rows.sort_by_key(|r| r.since);
        if rows.is_empty() {
            continue;
        }
        v.pipe.push(PipeLine {
            state: st.into(),
            count: rows.len(),
            oldest: age(s.now - rows[0].since),
            items: rows.iter().take(8).map(|r| item(r, r.since, String::new())).collect(),
        });
    }

    let mut rw: Vec<&Row> = s.rows.iter().filter(|r| r.state == "REWORK").collect();
    rw.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
    v.rework_items = rw.iter().map(|r| item(r, r.updated_at, r.reason.clone().unwrap_or_default())).collect();

    let blocked_row = |r: &Row| r.blocker.as_deref().is_some_and(|b| !b.is_empty());
    let mut nx: Vec<&Row> = ready
        .iter()
        .copied()
        .filter(|r| r.claimable.unwrap_or(r.holds.is_empty() && !s.on_base.contains_key(&r.id)) && !blocked_row(r))
        .collect();
    nx.sort_by_key(|r| (!r.rework, s.meta.get(&r.id).and_then(|m| m.priority).unwrap_or(9), r.since));
    v.blocked = ready
        .iter()
        .filter(|r| blocked_row(r))
        .map(|r| item(r, r.since, format!("{} <- {}", r.id, r.blocker.as_deref().unwrap_or(""))))
        .collect();
    v.next_count = nx.len();
    v.next = nx.iter().take(12).map(|r| item(r, r.since, String::new())).collect();

    let mut rc: Vec<&Row> = s.rows.iter().collect();
    rc.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
    v.recent = rc.iter().take(10).map(|r| item(r, r.updated_at, String::new())).collect();
    v
}

// ───────────────────────────── terminal renderer

const B: &str = "\x1b[1m";
const D: &str = "\x1b[2m";
const R: &str = "\x1b[0m";
const RED: &str = "\x1b[31m";
const YEL: &str = "\x1b[33m";
const GRN: &str = "\x1b[32m";
const CYN: &str = "\x1b[36m";

const REWORK_SHOWN: usize = 3;
const NEXT_SHOWN: usize = 5;
const RECENT_SHOWN: usize = 5;

pub fn render(v: &View, width: usize) -> Vec<String> {
    let w = width.max(60);
    let mut out = Vec::new();
    out.push(format!(
        "{B}LIFECYCLE{R} {D}{}{R}  release {B}{}{R}  {}  aeons {B}{}/{}{R}",
        v.clock,
        v.release,
        if v.world_running { format!("{GRN}world RUNNING{R}") } else { format!("{RED}world {}{R}", cut(&v.world, 30)) },
        v.working,
        v.ceiling
    ));
    for e in &v.errors {
        out.push(format!("{RED}  source failed: {}{R}", cut(e, w - 18)));
    }
    out.push(format!("{B}STATE MACHINE{R}   {D}(counts now · edge rates per hour){R}"));
    let dw = w.saturating_sub(24);
    let dot = |red: bool| if red { format!(" {RED}●{R}") } else { String::new() };
    for st in &v.machine {
        let c = if st.name == "REWORK" { YEL } else { "" };
        out.push(format!("{c}{B}{:<14}{R}{B}{:>4}{R}{}{}", st.name, st.count, if st.detail.is_empty() { String::new() } else { format!("  {}", cut(&st.detail, dw)) }, dot(st.red)));
        for e in &st.edges {
            let glyph = if e.main { "│" } else { "└▶" };
            let label = if e.main { e.event.clone() } else { format!("{} ▶ {}", e.event, e.to) };
            let lw = if e.main { 27 } else { 26 };
            out.push(format!("  {D}{glyph}{R} {:<lw$}{:>4}/h", cut(&label, lw), e.rate_1h));
        }
        if st.no_rework_exit {
            out.push(format!("  {RED}└▶ (no exit to REWORK){R}  {RED}●{R}"));
        }
        if st.name == "LANDED" {
            out.push(String::new());
        }
    }
    out.push(String::new());
    out.push(format!("{D}terminal 24h{R}    {}", v.terminal));
    out.push(String::new());
    out.push(format!("{B}REFUSED (1h){R}"));
    if v.refused.is_empty() {
        out.push(format!("  {D}none{R}"));
    }
    for r in &v.refused {
        out.push(format!("  {:<24}{B}{:>4}{R}  {}{}", cut(&r.what, 24), r.n, cut(&r.why, dw), dot(r.red)));
    }
    out.push(String::new());
    out.push(format!("{B}BATCH{R}"));
    for b in &v.batch {
        out.push(format!("  {:<12}{B}{:>3}{R}{}{}", b.name, b.count, if b.detail.is_empty() { String::new() } else { format!("  {}", b.detail) }, dot(b.red)));
    }
    if !v.holds.is_empty() {
        out.push(format!("{B}HOLDS{R}"));
        for g in &v.holds {
            let c = if g.kind == "poison" { RED } else { YEL };
            out.push(format!(
                "  {c}{:<9}{R}{B}{:>4}{R}  {}",
                g.kind,
                g.count,
                g.top.iter().take(2).map(|(n, r)| format!("{n}× {r}")).collect::<Vec<_>>().join(" · ")
            ));
        }
    }
    if !v.drift.is_empty() {
        out.push(format!(
            "{RED}{B}DRIFT{R} {RED}{} bead(s) READY/REWORK whose own commit is on {}:{R} {}",
            v.drift.len(),
            v.base,
            v.drift.iter().take(6).cloned().collect::<Vec<_>>().join(" ")
        ));
    }
    out.push(format!("{B}NOW{R}    {D}WORKING — holder · bead · lease{R}"));
    if v.now_items.is_empty() {
        out.push(format!("       {D}nothing is being worked{R}"));
    }
    let tw = w.saturating_sub(48).min(70);
    for i in &v.now_items {
        let tag = if i.rework { format!("{YEL}{B}REWORK{R} ") } else { String::new() };
        out.push(format!("  {CYN}{:<9}{R} {:<12} {tag}{} {:<tw$} {D}{} · {}{R}", cut(&i.who, 9), i.id, i.prio, cut(&i.title, tw), i.age, i.note));
        out.push(format!("  {D}{:<9}{R}", cut(&i.persona, 9)));
        let c = match i.out_level.as_str() {
            "bad" => RED,
            "warn" => YEL,
            _ => D,
        };
        let age = if i.out_age.is_empty() { String::new() } else { format!(" {} ago", i.out_age) };
        for (k, l) in i.out.iter().enumerate() {
            let tail = if k + 1 == i.out.len() { age.as_str() } else { "" };
            out.push(format!("      {c}{}{tail}{R}", cut(l, w.saturating_sub(8 + tail.chars().count()))));
        }
    }
    out.push(format!("{B}PIPE{R}   {D}SUBMITTED waits on a gate · CERTIFIED on a round · IN_DELIVERY in one{R}"));
    for p in &v.pipe {
        out.push(format!(
            "  {:<11} {B}{:>3}{R} {D}oldest {}{R}  {}",
            p.state,
            p.count,
            p.oldest,
            p.items.iter().take(6).map(|i| format!("{} {D}{}{R}", i.id, i.age)).collect::<Vec<_>>().join("  ")
        ));
    }
    if !v.rework_items.is_empty() {
        out.push(format!("{B}REWORK{R} {D}sent back — bead · why{R}"));
        for i in v.rework_items.iter().take(REWORK_SHOWN) {
            out.push(format!("  {YEL}{:<12}{R} {} {D}{}{R} {}", i.id, i.prio, i.age, cut(&i.note, w.saturating_sub(30))));
        }
        if v.rework_items.len() > REWORK_SHOWN {
            out.push(format!("  {D}… {} more{R}", v.rework_items.len() - REWORK_SHOWN));
        }
    }
    out.push(format!("{B}NEXT{R}   {B}{}{R} claimable {D}(claim order: express, REWORK, then priority){R}", v.next_count));
    for i in v.next.iter().take(NEXT_SHOWN) {
        out.push(format!("  {} {:<12} {}", i.prio, i.id, cut(&i.title, w.saturating_sub(20))));
    }
    if !v.blocked.is_empty() {
        out.push(format!("{B}BLOCKED{R} {B}{}{R} {D}(READY, waiting on an unmet dependency){R}", v.blocked.len()));
        for i in v.blocked.iter().take(6) {
            out.push(format!("  {} {:<26} {}", i.prio, i.note, cut(&i.title, w.saturating_sub(36))));
        }
    }
    out.push(format!("{B}RECENT{R} {D}last transitions — when · bead · now{R}"));
    for i in v.recent.iter().take(RECENT_SHOWN) {
        let c = match i.state.as_str() {
            "LANDED" => GRN,
            "REWORK" | "DROPPED" => YEL,
            _ => "",
        };
        out.push(format!("  {D}{:>4}{R} {:<12} {c}{:<11}{R} {}", i.age, i.id, i.state, cut(&i.title, w.saturating_sub(34))));
    }
    out.push(format!("{D}source: spira-lc (state) · work list (titles) · {} commits (drift) — no bd{R}", v.base));
    out
}

// ───────────────────────────── phone renderer (served by loom)

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The same `View` as a self-contained, phone-width HTML page. `stale` is the snapshot's age in
/// seconds when it is old enough to distrust; the page says so rather than showing old numbers
/// as live.
pub fn render_html(v: &View, stale: Option<i64>, refresh_s: u64) -> String {
    let mut h = String::new();
    h.push_str(&format!(
        "<!doctype html><html lang=en><head><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'>\
<meta http-equiv=refresh content='{refresh_s}'><title>Spira lifecycle</title><style>{CSS}</style></head><body>"
    ));
    h.push_str(&format!(
        "<header><b>LIFECYCLE</b> <span class=dim>{}</span> · release <b>{}</b> · <span class={}>{}</span> · aeons <b>{}/{}</b> · <a href=/stuck>where work is stuck →</a></header>",
        esc(&v.clock),
        esc(&v.release),
        if v.world_running { "ok" } else { "bad" },
        if v.world_running { "world RUNNING".to_string() } else { esc(&format!("world {}", v.world)) },
        v.working,
        v.ceiling
    ));
    if let Some(age_s) = stale {
        h.push_str(&format!("<p class='banner bad'>Snapshot is {} old — the pane's collector is not running.</p>", age(age_s)));
    }
    for e in &v.errors {
        h.push_str(&format!("<p class='banner bad'>source failed: {}</p>", esc(e)));
    }
    h.push_str("<section><h2>Flow</h2><div class=flow>");
    for (i, (st, n)) in v.flow.iter().enumerate() {
        let extra = if i == 0 && v.ready_held > 0 {
            format!("<small class={}>{} held · {}%</small>", if v.held_pct >= 25 { "bad" } else { "warn" }, v.ready_held, v.held_pct)
        } else {
            String::new()
        };
        h.push_str(&format!("<div class=stage><span>{}</span><b>{n}</b>{extra}</div>", esc(st)));
    }
    h.push_str(&format!(
        "</div><p class=dim><span class=warn>REWORK {}</span> · DROPPED {}/24h · SUPERSEDED {}/24h · LANDED {} all time</p></section>",
        v.rework, v.dropped_24h, v.superseded_24h, v.landed_total
    ));
    if !v.holds.is_empty() {
        h.push_str("<section><h2>Holds</h2><table>");
        for g in &v.holds {
            let tops = g.top.iter().map(|(n, r)| format!("{n}× {}", esc(r))).collect::<Vec<_>>().join("<br>");
            h.push_str(&format!(
                "<tr><td class={}>{}</td><td class=num>{}</td><td>{tops}</td></tr>",
                if g.kind == "poison" { "bad" } else { "warn" },
                esc(&g.kind),
                g.count
            ));
        }
        h.push_str("</table></section>");
    }
    if !v.drift.is_empty() {
        h.push_str(&format!(
            "<section class=bad><h2>Drift</h2><p>{} bead(s) READY/REWORK whose own commit is on {}: {}</p></section>",
            v.drift.len(),
            esc(&v.base),
            esc(&v.drift.join(" "))
        ));
    }
    let table = |title: &str, sub: &str, items: &[Item], cols: &dyn Fn(&Item) -> String| -> String {
        let mut t = format!("<section><h2>{title} <small class=dim>{sub}</small></h2><table>");
        if items.is_empty() {
            t.push_str("<tr><td class=dim>nothing</td></tr>");
        }
        for i in items {
            t.push_str(&cols(i));
        }
        t.push_str("</table></section>");
        t
    };
    h.push_str(&table("Now", "working — holder · bead · lease", &v.now_items, &|i| {
        format!(
            "<tr><td class=who>{}<br><span class=dim>{}</span></td><td><b>{}</b>{} <span class=dim>{}</span><br>{}{}</td><td class=dim>{}<br>{}</td></tr>",
            esc(&i.who),
            esc(&i.persona),
            esc(&i.id),
            if i.rework { " <b class=warn>REWORK</b>" } else { "" },
            esc(&i.prio),
            esc(&i.title),
            out_html(i),
            esc(&i.age),
            esc(&i.note)
        )
    }));
    h.push_str("<section><h2>Pipeline <small class=dim>submitted waits on a gate · certified on a round</small></h2><table>");
    if v.pipe.is_empty() {
        h.push_str("<tr><td class=dim>nothing waiting</td></tr>");
    }
    for p in &v.pipe {
        let ids = p.items.iter().map(|i| format!("{} <span class=dim>{}</span>", esc(&i.id), esc(&i.age))).collect::<Vec<_>>().join(" · ");
        h.push_str(&format!("<tr><td>{}</td><td class=num>{}</td><td><span class=dim>oldest {}</span><br>{ids}</td></tr>", esc(&p.state), p.count, esc(&p.oldest)));
    }
    h.push_str("</table></section>");
    h.push_str(&table("Rework", "sent back — why", &v.rework_items[..v.rework_items.len().min(10)], &|i| {
        format!("<tr><td><b>{}</b> <span class=dim>{} · {}</span><br><span class=warn>{}</span></td></tr>", esc(&i.id), esc(&i.prio), esc(&i.age), esc(&i.note))
    }));
    h.push_str(&table(&format!("Next ({})", v.next_count), "ready, unheld, not on the base — by priority", &v.next, &|i| {
        format!("<tr><td class=num>{}</td><td><b>{}</b><br>{}</td></tr>", esc(&i.prio), esc(&i.id), esc(&i.title))
    }));
    if !v.blocked.is_empty() {
        h.push_str(&table(&format!("Blocked ({})", v.blocked.len()), "ready, waiting on an unmet dependency", &v.blocked, &|i| {
            format!("<tr><td class=num>{}</td><td><b>{}</b><br>{}</td></tr>", esc(&i.prio), esc(&i.note), esc(&i.title))
        }));
    }
    h.push_str(&table("Recent", "last transitions", &v.recent, &|i| {
        let c = match i.state.as_str() {
            "LANDED" => "ok",
            "REWORK" | "DROPPED" => "warn",
            _ => "",
        };
        format!("<tr><td class=dim>{}</td><td><b>{}</b> <span class={c}>{}</span><br>{}</td></tr>", esc(&i.age), esc(&i.id), esc(&i.state), esc(&i.title))
    }));
    h.push_str(&format!(
        "<footer class=dim>source: spira-lc (state) · work list (titles) · {} commits (drift) — no bd · refreshes every {refresh_s}s</footer></body></html>",
        esc(&v.base)
    ));
    h
}

fn out_html(i: &Item) -> String {
    if i.out.is_empty() {
        return String::new();
    }
    let age = if i.out_age.is_empty() { String::new() } else { format!(" · {} ago", esc(&i.out_age)) };
    let lines = i.out.iter().map(|l| esc(&cut(l, 140))).collect::<Vec<_>>().join("<br>");
    format!("<div class='out {}'>{lines}<small>{age}</small></div>", esc(&i.out_level))
}

const CSS: &str = ":root{--bg:#fff;--fg:#111;--dim:#6b7280;--ok:#15803d;--warn:#b45309;--bad:#b91c1c;--line:#e5e7eb}\
@media (prefers-color-scheme:dark){:root{--bg:#0b0d10;--fg:#e5e7eb;--dim:#9ca3af;--ok:#4ade80;--warn:#fbbf24;--bad:#f87171;--line:#1f2937}}\
body{background:var(--bg);color:var(--fg);font:14px/1.4 ui-monospace,SFMono-Regular,Menlo,monospace;margin:0;padding:12px 16px;max-width:900px}\
header{font-size:15px;margin-bottom:8px}h2{font-size:14px;margin:14px 0 6px;text-transform:uppercase;letter-spacing:.05em}\
section{border-top:1px solid var(--line);padding-top:4px}table{width:100%;border-collapse:collapse}td{padding:4px 6px 4px 0;vertical-align:top;border-bottom:1px solid var(--line)}\
.num{text-align:right;font-weight:bold;width:3em}.who{color:#0891b2;width:6em}.dim{color:var(--dim)}.ok{color:var(--ok)}.warn{color:var(--warn)}.bad{color:var(--bad)}\
.out{margin-top:2px;font-size:12px;word-break:break-word}.out.none,.out.ok{color:var(--dim)}.out.warn{color:var(--warn)}.out.bad{color:var(--bad)}\
.banner{padding:6px 8px;border:1px solid var(--bad);border-radius:4px}.flow{display:flex;flex-wrap:wrap;gap:6px}\
.stage{border:1px solid var(--line);border-radius:6px;padding:6px 8px;min-width:5.5em;display:flex;flex-direction:column}.stage span{font-size:11px;color:var(--dim)}.stage b{font-size:20px}\
footer{margin-top:14px;font-size:12px}";

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
        let v = view(&s);
        assert_eq!(v.next_count, 2);
        assert_eq!(v.next.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), vec!["sp-high", "sp-low"]);
        let f = plain(&render(&v, 120));
        assert!(f.contains("2 claimable") && f.contains("claimable 2"), "{f}");
    }

    #[test]
    fn a_ready_bead_whose_commit_is_on_the_base_is_drift_not_next() {
        let mut s = snap(vec![row("sp-done", "READY", 1)]);
        s.on_base.insert("sp-done".into(), "abc123".into());
        let v = view(&s);
        assert_eq!(v.drift, vec!["sp-done"]);
        assert_eq!(v.next_count, 0);
        assert!(plain(&render(&v, 120)).contains("DRIFT 1 bead(s)"));
    }

    #[test]
    fn a_starved_queue_shows_its_held_share_and_reasons() {
        let mut rows = vec![row("sp-a", "READY", 1)];
        for i in 0..3 {
            let mut r = row(&format!("sp-h{i}"), "READY", 1);
            r.holds = vec!["ask".into()];
            r.reason = Some("repo:spira is not in the map".into());
            rows.push(r);
        }
        let f = plain(&render(&view(&snap(rows)), 120));
        assert!(f.contains("READY") && f.contains("claimable 1 · held 3"), "{f}");
        assert!(f.contains("3× repo:spira is not in the map"), "{f}");
    }

    #[test]
    fn a_failed_source_is_named_never_an_empty_frame() {
        let mut s = snap(vec![]);
        s.errors.push("spira-lc list: exit 1".into());
        let v = view(&s);
        assert!(plain(&render(&v, 100)).contains("source failed: spira-lc list: exit 1"));
        assert!(render_html(&v, None, 10).contains("source failed: spira-lc list: exit 1"));
        assert!(render_html(&v, None, 10).contains("<a href=/stuck>"), "the lifecycle page links to where work is stuck");
    }

    #[test]
    fn the_pane_and_the_page_show_the_same_numbers_from_one_snapshot() {
        let mut held = row("sp-h", "READY", 2);
        held.holds = vec!["poison".into()];
        let mut s = snap(vec![row("sp-r", "READY", 1), held, row("sp-w", "WORKING", 1), row("sp-c", "CERTIFIED", 1), row("sp-x", "REWORK", 1)]);
        s.on_base.insert("sp-x".into(), "c".into());
        let v = view(&s);
        let pane = plain(&render(&v, 120));
        let page = render_html(&v, None, 10);
        for (st, n) in &v.flow {
            let line = v.machine.iter().find(|m| m.name == st.trim_end_matches("/24h")).map(|m| format!("{:<14}{:>4}", m.name, m.count));
            assert!(line.is_some_and(|l| pane.contains(&l)), "pane lacks {st} {n}");
            assert!(page.contains(&format!("<span>{st}</span><b>{n}</b>")), "page lacks {st} {n}");
        }
        assert!(pane.contains("DRIFT 1") && page.contains("1 bead(s) READY/REWORK"));
        assert!(page.contains(&format!("Next ({})", v.next_count)));
    }

    fn now_row(id: &str, persona: Option<&str>, rework: bool) -> Row {
        let mut r = row(id, "WORKING", 1);
        r.holder = Some("aeon-mindy".into());
        r.persona = persona.map(String::from);
        r.rework = rework;
        r
    }

    #[test]
    fn now_rows_carry_a_rework_tag_and_the_persona_under_the_name() {
        let s = snap(vec![now_row("sp-rw", Some("ops"), true), now_row("sp-fresh", Some("guardian"), false), now_row("sp-nul", None, false)]);
        let v = view(&s);
        let pane = plain(&render(&v, 120));
        let l: Vec<&str> = pane.lines().collect();
        let at = |id: &str| l.iter().position(|x| x.contains(id)).unwrap();
        assert!(l[at("sp-rw")].contains("REWORK") && l[at("sp-rw") + 1].trim() == "ops", "{pane}");
        assert!(!l[at("sp-fresh")].contains("REWORK") && l[at("sp-fresh") + 1].trim() == "guardian", "{pane}");
        assert_eq!(l[at("sp-nul") + 1].trim(), "—", "a NULL persona is a dash, not omitted");
        let page = render_html(&v, None, 10);
        assert!(page.contains("<b class=warn>REWORK</b>") && page.contains("<span class=dim>ops</span>") && page.contains("<span class=dim>—</span>"));
        assert_eq!(page.matches("REWORK</b>").count(), 1);
        assert!(render(&v, 120).iter().any(|x| x.contains(&format!("{YEL}{B}REWORK{R}"))), "colour on the tag");
    }

    #[test]
    fn next_lists_only_claimable_rework_first_and_blocked_apart() {
        let mut fresh = row("sp-fresh", "READY", 1);
        fresh.claimable = Some(true);
        let mut rw = row("sp-rw", "READY", 2);
        rw.claimable = Some(true);
        rw.rework = true;
        let mut no = row("sp-no", "READY", 3);
        no.claimable = Some(false);
        let mut bl = row("sp-hq1v76", "READY", 4);
        bl.blocker = Some("sp-o4s4t4".into());
        bl.claimable = Some(false);
        let mut s = snap(vec![fresh, rw, no, bl]);
        s.meta.insert("sp-fresh".into(), Meta { title: "f".into(), priority: Some(0) });
        s.meta.insert("sp-rw".into(), Meta { title: "r".into(), priority: Some(2) });
        let v = view(&s);
        assert_eq!(v.next.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), vec!["sp-rw", "sp-fresh"]);
        assert_eq!(v.next_count, 2);
        let pane = plain(&render(&v, 120));
        assert!(pane.contains("BLOCKED 1") && pane.contains("sp-hq1v76 <- sp-o4s4t4"), "{pane}");
        assert!(render_html(&v, None, 10).contains("sp-hq1v76 &lt;- sp-o4s4t4"));
    }

    fn log_line(kind: &str, body: &str) -> String {
        match kind {
            "text" => format!(r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{body}"}}]}}}}"#),
            _ => format!(r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Bash","input":{{"command":"{body}"}}}}]}}}}"#),
        }
    }

    fn working(tail: Option<Tail>) -> Snapshot {
        let mut r = row("sp-w", "WORKING", 1);
        r.holder = Some("aeon-guardian".into());
        let mut s = snap(vec![r]);
        if let Some(t) = tail {
            s.tails.insert("sp-w".into(), t);
        }
        s
    }

    #[test]
    fn tail_lines_takes_the_last_two_meaningful_lines_and_skips_noise() {
        let log = [
            r#"{"type":"system","subtype":"init"}"#.to_string(),
            log_line("text", "first"),
            log_line("text", "reading   the   file"),
            r#"{"type":"user","message":{"content":[]}}"#.to_string(),
            log_line("tool", "cargo test"),
            "not json".to_string(),
        ]
        .join("\n");
        assert_eq!(tail_lines(log.as_bytes(), false, 2), vec!["reading the file", "▸ Bash cargo test"]);
    }

    #[test]
    fn tail_lines_drops_the_cut_first_line_of_a_partial_read() {
        let log = format!("{}\n{}", &log_line("text", "cut")[10..], log_line("text", "whole"));
        assert_eq!(tail_lines(log.as_bytes(), true, 2), vec!["whole"]);
    }

    #[test]
    fn a_fresh_tail_shows_its_lines_and_age_in_pane_and_page() {
        let s = working(Some(Tail { lines: vec!["one".into(), "two".into()], mtime: 100_000 - 120 }));
        let v = view(&s);
        let pane = plain(&render(&v, 120));
        assert!(pane.contains("one\n      two 2m ago"), "{pane}");
        assert_eq!(v.now_items[0].out_level, "ok");
        let page = render_html(&v, None, 10);
        assert!(page.contains("class='out ok'>one<br>two<small> · 2m ago"), "{page}");
    }

    #[test]
    fn a_silent_aeon_goes_amber_at_five_minutes_and_red_at_fifteen() {
        for (age_s, level, color) in [(299, "ok", D), (300, "warn", YEL), (899, "warn", YEL), (900, "bad", RED), (20 * 60, "bad", RED)] {
            let v = view(&working(Some(Tail { lines: vec!["x".into()], mtime: 100_000 - age_s })));
            assert_eq!(v.now_items[0].out_level, level, "{age_s}s");
            assert!(render(&v, 120).iter().any(|l| l.starts_with(&format!("      {color}x"))), "{age_s}s");
            assert!(render_html(&v, None, 10).contains(&format!("class='out {level}'")));
        }
    }

    #[test]
    fn a_missing_log_says_no_output_yet_not_an_error() {
        let v = view(&working(None));
        assert!(v.errors.is_empty());
        assert!(plain(&render(&v, 120)).contains("no output yet"));
        assert!(render_html(&v, None, 10).contains("no output yet"));
    }

    #[test]
    fn a_working_row_without_a_holder_shows_no_output_line() {
        let mut s = snap(vec![row("sp-w", "WORKING", 1)]);
        s.tails.insert("sp-w".into(), Tail { lines: vec!["x".into()], mtime: 1 });
        assert!(view(&s).now_items[0].out.is_empty());
    }

    #[test]
    fn a_stale_snapshot_says_so_on_the_page() {
        assert!(render_html(&view(&snap(vec![])), Some(600), 10).contains("Snapshot is 10m old"));
    }

    #[test]
    fn the_snapshot_survives_a_round_trip_through_the_file_loom_reads() {
        let mut s = snap(vec![row("sp-a", "READY", 1)]);
        s.meta.insert("sp-a".into(), Meta { title: "t".into(), priority: Some(1) });
        let back: Snapshot = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(serde_json::to_string(&view(&back)).unwrap(), serde_json::to_string(&view(&s)).unwrap());
    }

    fn ge(from: &str, event: &str, to: &str) -> GraphEdge {
        GraphEdge { from: from.into(), event: event.into(), to: to.into() }
    }

    fn graph(with_certified_exit: bool) -> Vec<GraphEdge> {
        let mut g = vec![
            ge("OPEN", "Ready", "READY"),
            ge("READY", "Claim", "WORKING"),
            ge("WORKING", "Submit", "SUBMITTED"),
            ge("WORKING", "Release", "READY"),
            ge("SUBMITTED", "GatePass", "CERTIFIED"),
            ge("SUBMITTED", "GateRed", "REWORK"),
            ge("CERTIFIED", "Deliver", "IN_DELIVERY"),
            ge("IN_DELIVERY", "Delivered", "LANDED"),
            ge("IN_DELIVERY", "Returned", "REWORK"),
            ge("REWORK", "Claim", "WORKING"),
            ge("READY", "Drop", "DROPPED"),
        ];
        if with_certified_exit {
            g.push(ge("CERTIFIED", "GateRed", "REWORK"));
        }
        g
    }

    fn dwell(id: &str, st: &str, entered: i64, p95: i64) -> DwellRow {
        DwellRow { bead_id: id.into(), state: st.into(), entered_at: entered, p95_s: Some(p95) }
    }

    fn applied(from: &str, to: &str, n: i64) -> EdgeRow {
        EdgeRow { kind: "applied".into(), from_state: from.into(), to_state: Some(to.into()), n_1h: n, ..Default::default() }
    }

    fn refused(from: &str, event: &str, why: &str, n: i64) -> EdgeRow {
        EdgeRow { kind: "refused".into(), from_state: from.into(), event: Some(event.into()), refusal: Some(why.into()), n_1h: n, ..Default::default() }
    }

    fn healthy() -> Snapshot {
        let mut s = snap(vec![row("sp-r", "READY", 90_000), row("sp-w", "WORKING", 99_000), row("sp-c", "CERTIFIED", 99_000)]);
        s.rows[1].holder = Some("aeon-guardian".into());
        s.rows[1].lease_until = Some(100_500);
        s.graph = graph(true);
        s.edges = vec![applied("READY", "WORKING", 12), applied("WORKING", "SUBMITTED", 9)];
        s.dwell = vec![dwell("sp-w", "WORKING", 99_000, 7_200), dwell("sp-c", "CERTIFIED", 99_000, 7_200)];
        s
    }

    fn squash(s: &str) -> String {
        s.split('\n').map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).collect::<Vec<_>>().join("\n")
    }

    fn machine_lines(s: &Snapshot) -> Vec<String> {
        let f = plain(&render(&view(s), 120));
        f.lines().skip_while(|l| !l.starts_with("STATE MACHINE")).take_while(|l| !l.starts_with("HOLDS") && !l.starts_with("NOW")).map(String::from).collect()
    }

    #[test]
    fn a_clean_fixture_renders_no_red_dot_and_no_missing_exit() {
        let f = machine_lines(&healthy()).join("\n");
        assert!(!f.contains('●') && !f.contains("no exit"), "{f}");
        assert!(f.contains("REFUSED (1h)\n  none"), "{f}");
    }

    #[test]
    fn the_state_machine_is_vertical_with_fixed_columns_and_every_edge_on_its_own_line() {
        let f = machine_lines(&healthy()).join("\n");
        let want = "\
STATE MACHINE   (counts now · edge rates per hour)
OPEN             0
  │ ready                         0/h
READY            1  claimable 1
  │ claim                        12/h
WORKING          1
  │ submit                        9/h
  └▶ release ▶ READY              0/h
SUBMITTED        0
  │ gate-pass                     0/h
  └▶ gate-red ▶ REWORK            0/h
CERTIFIED        1
  │ deliver                       0/h
  └▶ gate-red ▶ REWORK            0/h
IN_DELIVERY      0  in open batch 0 · orphaned 0
  │ delivered                     0/h
  └▶ returned ▶ REWORK            0/h
LANDED           0  last 24h

REWORK           0  claimable 0
  └▶ claim ▶ WORKING              0/h

terminal 24h    SUPERSEDED 0 · DROPPED 0 · DONE 0

REFUSED (1h)
  none

BATCH
  OPEN          0
  CI            0
  GREEN         0
  LANDED        0  last 24h";
        assert_eq!(f, want);
    }

    #[test]
    fn certified_for_a_day_with_no_exit_to_rework_shows_both_dots_and_the_missing_exit() {
        let mut s = healthy();
        s.rows = vec![row("sp-cert-a", "CERTIFIED", 100_000 - 23 * 3600), row("sp-cert-b", "CERTIFIED", 100_000 - 18 * 3600)];
        s.graph = graph(false);
        s.dwell = vec![dwell("sp-cert-a", "CERTIFIED", 100_000 - 23 * 3600, 3_600), dwell("sp-cert-b", "CERTIFIED", 100_000 - 18 * 3600, 3_600)];
        let f = squash(&machine_lines(&s).join("\n"));
        assert!(f.contains("CERTIFIED 2 stuck >p95 2 ●"), "{f}");
        assert!(f.contains("└▶ (no exit to REWORK) ●"), "{f}");
        assert!(f.contains("│ deliver"), "{f}");
    }

    #[test]
    fn an_orphaned_working_bead_names_its_dead_holder() {
        let mut s = healthy();
        s.rows[1].lease_until = Some(99_000);
        s.rows.push({
            let mut r = row("sp-orphan", "WORKING", 95_000);
            r.holder = None;
            r
        });
        let f = squash(&machine_lines(&s).join("\n"));
        assert!(f.contains("WORKING 2 dead holder 2 ●"), "{f}");
    }

    #[test]
    fn stranded_delivery_is_split_into_open_batch_and_orphaned() {
        let mut s = healthy();
        s.rows = (0..5).map(|i| row(&format!("sp-d{i}"), "IN_DELIVERY", 99_000)).collect();
        s.batches = vec![BatchRow { id: "r-1".into(), state: "CI_RUNNING".into(), last_at: 99_000, members: vec!["sp-d0".into(), "sp-d1".into()], ..Default::default() }];
        let f = squash(&machine_lines(&s).join("\n"));
        assert!(f.contains("IN_DELIVERY 5 in open batch 2 · orphaned 3 ●"), "{f}");
        s.rows.truncate(2);
        assert!(!squash(&machine_lines(&s).join("\n")).contains("orphaned 0 ●"));
    }

    #[test]
    fn ready_splits_into_claimable_blocked_with_its_blocker_held_and_poison() {
        let mut s = healthy();
        let mut blocked = row("sp-b", "READY", 1);
        blocked.holds = vec!["wait".into()];
        blocked.reason = Some("blocked on sp-blk1, sp-blk1".into());
        let mut held = row("sp-h", "READY", 1);
        held.holds = vec!["ask".into()];
        let mut poison = row("sp-p", "READY", 1);
        poison.holds = vec!["poison".into(), "wait".into()];
        s.rows.extend([blocked, held, poison]);
        let f = squash(&machine_lines(&s).join("\n"));
        assert!(f.contains("READY 4 claimable 1 · blocked 1 ← sp-blk1 · held 1 · poison 1"), "{f}");
    }

    #[test]
    fn a_batch_stuck_green_past_its_delivery_p95_is_a_stuck_dot() {
        let mut s = healthy();
        s.dwell.push(dwell("sp-x", "IN_DELIVERY", 99_000, 1_800));
        s.batches = vec![
            BatchRow { id: "r-1".into(), state: "GREEN".into(), last_at: 100_000 - 4 * 3600, members: vec![], ..Default::default() },
            BatchRow { id: "r-0".into(), state: "LANDED".into(), last_at: 90_000, members: vec![], ..Default::default() },
        ];
        let f = squash(&plain(&render(&view(&s), 120)));
        assert!(f.contains("GREEN 1 stuck 4h ●"), "{f}");
        assert!(f.contains("LANDED 1 last 24h"), "{f}");
    }

    #[test]
    fn repeated_refusals_are_listed_by_state_and_event_and_the_edges_use_the_graph() {
        let mut s = healthy();
        s.edges.push(refused("WORKING", "Claim", "double-claim", 14));
        s.edges.push(refused("CERTIFIED", "GateRed", "no legal exit", 3));
        s.edges.push(refused("READY", "Submit", "wrong state", 1));
        let f = squash(&machine_lines(&s).join("\n"));
        assert!(f.contains("Claim@WORKING 14 double-claim ●"), "{f}");
        assert!(f.contains("GateRed@CERTIFIED 3 no legal exit ●"), "{f}");
        assert!(f.contains("Submit@READY 1 wrong state\n") || f.ends_with("wrong state"), "{f}");
        assert!(!f.contains("wrong state ●"), "{f}");
    }

    #[test]
    fn skipped_for_conflict_is_counted_under_submitted() {
        let mut s = healthy();
        let mut r = row("sp-s", "SUBMITTED", 99_000);
        r.reason = Some("skipped: Conflict in round".into());
        s.rows.push(r);
        assert!(squash(&machine_lines(&s).join("\n")).contains("SUBMITTED 1 skipped: conflict 1"));
    }

    #[test]
    fn the_listings_are_shortened_to_make_room() {
        let mut s = healthy();
        for i in 0..9 {
            s.rows.push(row(&format!("sp-n{i}"), "READY", i));
            s.rows.push(row(&format!("sp-k{i}"), "REWORK", i));
        }
        let f = plain(&render(&view(&s), 120));
        assert!(!f.contains("FLOW"), "{f}");
        let after = |head: &str| f.lines().skip_while(|l| !l.starts_with(head)).skip(1).take_while(|l| l.starts_with("  ")).count();
        assert!(after("NEXT") <= 5 && after("RECENT") <= 5 && after("REWORK") <= 4, "{f}");
    }

    #[test]
    fn an_old_snapshot_file_without_the_machine_fields_still_loads() {
        let old = serde_json::json!({"now":1,"release":"","world":"","base":"","rows":[],"meta":{},"on_base":{},"ceiling":0,"tails":{},"errors":[]});
        let s: Snapshot = serde_json::from_value(old).unwrap();
        assert!(s.graph.is_empty() && s.dwell.is_empty());
    }
}
