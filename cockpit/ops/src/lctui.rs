//! The ops pane as a navigable tree (sp-5j35g5, per Ryan 2026-10-09). Every line is a node; any
//! node with children is AUTO (sized by the space left), OPEN (shown whole, with everything under
//! it) or COLLAPSED (its own line only), at every depth: a section, a state, a transition.
//! The frame is cut to the pane exactly: lines are truncated, never wrapped.
//!
//! In AUTO, the pane gives up detail only as far as it is forced to, deepest and least important
//! first: RECENT, HOLDS, REFUSED, BLOCKED and DRIFT thin out; then the state machine drops its idle
//! transitions, then its empty states, then folds the rest into their states; then NEXT, REWORK
//! and PIPE shorten; then NOW folds each aeon's output. ROUND and NOW themselves are never cut.
//! Anything AUTO removed comes back on the next frame once there is room or it is not empty.
//!
//! Everything here is pure (view, choices, size in; lines out) so it is tested without a terminal.

use crate::lcview::{cut, View};
use std::collections::{HashMap, HashSet};

const B: &str = "\x1b[1m";
const D: &str = "\x1b[2m";
const R: &str = "\x1b[0m";
const RED: &str = "\x1b[31m";
const YEL: &str = "\x1b[33m";
const GRN: &str = "\x1b[32m";
const CYN: &str = "\x1b[36m";
const REV: &str = "\x1b[7m";

// The order AUTO gives detail up in, first to last (lower goes first).
const R_RECENT_SOME: u16 = 10;
const R_RECENT_ALL: u16 = 11;
const R_HOLD_FOLD: u16 = 12;
const R_HOLD_ALL: u16 = 13;
const R_REFUSED_SOME: u16 = 14;
const R_REFUSED_ALL: u16 = 15;
const R_BLOCKED_SOME: u16 = 16;
const R_BLOCKED_ALL: u16 = 17;
const R_DRIFT: u16 = 18;
// The state machine gives up, in order (per Ryan 2026-10-09): idle transitions, beads past each
// state's oldest three, empty states, the oldest three, then folds each state's transitions.
const R_IDLE_EDGE: u16 = 20;
const R_STATE_BEADS_MORE: u16 = 21;
const R_EMPTY_STATE: u16 = 22;
const R_STATE_BEADS: u16 = 23;
const R_STATE_FOLD: u16 = 24;
const R_TERMINAL: u16 = 25;
const R_NEXT_SOME: u16 = 30;
const R_NEXT_FEW: u16 = 31;
const R_NEXT_ALL: u16 = 36;
const R_NOW_FOLD: u16 = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Mode {
    #[default]
    Auto,
    Open,
    Collapsed,
}

impl Mode {
    pub fn next(self) -> Mode {
        match self {
            Mode::Auto => Mode::Open,
            Mode::Open => Mode::Collapsed,
            Mode::Collapsed => Mode::Auto,
        }
    }
}

/// One line of the pane and what hangs under it. `elide` is the rank at which AUTO may remove the
/// line itself, `fold` the rank at which it may hide the children; `auto_open` false means AUTO
/// starts it folded (detail shown only on request).
#[derive(Debug, Clone, Default)]
pub struct Node {
    pub key: String,
    pub line: String,
    pub kids: Vec<Node>,
    pub elide: Option<u16>,
    pub fold: Option<u16>,
    pub auto_open: bool,
}

impl Node {
    fn new(key: impl Into<String>, line: impl Into<String>) -> Node {
        Node { key: key.into(), line: line.into(), auto_open: true, ..Default::default() }
    }
    fn kids(mut self, k: Vec<Node>) -> Node {
        self.kids = k;
        self
    }
    fn elide(mut self, r: u16) -> Node {
        self.elide = Some(r);
        self
    }
    fn fold(mut self, r: u16) -> Node {
        self.fold = Some(r);
        self
    }
    fn closed(mut self) -> Node {
        self.auto_open = false;
        self
    }
}

/// Item i of a list is removed at the lowest rank whose cap it reaches: `[(5, 30), (2, 31)]`
/// keeps five until rank 30, then two until rank 31.
fn ranked(nodes: Vec<Node>, caps: &[(usize, u16)]) -> Vec<Node> {
    nodes
        .into_iter()
        .enumerate()
        .map(|(i, n)| match caps.iter().filter(|(cap, _)| i >= *cap).map(|(_, r)| *r).min() {
            Some(r) => n.elide(r),
            None => n,
        })
        .collect()
}

/// Interaction state the operator controls: a mode per node path (persisted by the binary), the
/// selected node, and a scroll offset.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Ui {
    pub modes: HashMap<String, Mode>,
    #[serde(default)]
    pub selected: String,
    #[serde(skip)]
    pub scroll: usize,
}

impl Ui {
    pub fn mode(&self, key: &str) -> Mode {
        self.modes.get(key).copied().unwrap_or_default()
    }
    pub fn set(&mut self, key: &str, m: Mode) {
        if m == Mode::Auto {
            self.modes.remove(key);
        } else {
            self.modes.insert(key.into(), m);
        }
    }
    pub fn cycle(&mut self, key: &str) {
        let m = self.mode(key).next();
        self.set(key, m);
    }
    /// Back to AUTO for this node and everything under it.
    pub fn reset(&mut self, key: &str) {
        let sub = format!("{key}/");
        self.modes.retain(|k, _| k != key && !k.starts_with(&sub));
    }
}

/// The parent of a node path (`state/READY/WORKING` → `state/READY`).
pub fn parent_key(key: &str) -> Option<&str> {
    key.rsplit_once('/').map(|(p, _)| p)
}

/// A laid-out frame: the lines to draw, which screen row is which node, and the visible nodes in
/// order with whether each has children and is showing them (for navigation).
#[derive(Debug, Clone, Default)]
pub struct Frame {
    pub lines: Vec<String>,
    pub rows: Vec<(usize, String)>,
    pub order: Vec<String>,
    pub nodes: HashMap<String, (bool, bool)>,
    pub all_keys: HashSet<String>,
}

fn dot(red: bool) -> String {
    if red {
        format!(" {RED}●{R}")
    } else {
        String::new()
    }
}

fn banner(v: &View) -> Vec<String> {
    let mut out = vec![format!(
        "{B}LIFECYCLE{R} {D}{}{R}  release {B}{}{R}  {}  aeons {B}{}/{}{R}",
        v.clock,
        v.release,
        if v.world_running { format!("{GRN}world RUNNING{R}") } else { format!("{RED}world {}{R}", cut(&v.world, 24)) },
        v.working,
        v.ceiling
    )];
    if !v.errors.is_empty() {
        let first = v.errors.first().cloned().unwrap_or_default();
        let more = if v.errors.len() > 1 { format!(" (+{} more)", v.errors.len() - 1) } else { String::new() };
        out.push(format!("{RED}  source failed: {}{more}{R}", cut(&first, 90)));
    }
    out
}

/// The whole pane as a tree, in Ryan's order: DECIDE, ROUND, NOW, STATE MACHINE (with each state's
/// beads under it; PIPE and REWORK retired 2026-10-09), NEXT,
/// BLOCKED, DRIFT, REFUSED, HOLDS, RECENT.
pub fn tree(v: &View) -> Vec<Node> {
    let mut out = Vec::new();

    // DECIDE: what is waiting on the operator, above everything else; it is never work.
    let asks: Vec<Node> = v.decide.iter().map(|i| Node::new(format!("decide/{}", i.id), format!("{YEL}{:<12}{R} {D}{:>4}{R} {}", i.id, i.age, i.title))).collect();
    out.push(
        Node::new(
            "decide",
            if asks.is_empty() { format!("{B}DECIDE{R} {D}nothing waiting on you{R}") } else { format!("{YEL}{B}DECIDE{R} {B}{}{R} {D}waiting on you{R}", asks.len()) },
        )
        .kids(asks),
    );

    out.push(match &v.round {
        None => Node::new("round", format!("{B}ROUND{R}  {D}none open{R}")),
        Some(r) => {
            let mut kids: Vec<Node> = r
                .members
                .iter()
                .map(|m| {
                    let ej = if m.ejects > 0 { format!("{YEL}ejected {}× before{R} ", m.ejects) } else { String::new() };
                    Node::new(format!("round/{}", m.id), format!("{:<14} {} {ej}{}", m.id, m.prio, m.title))
                })
                .collect();
            if !r.ejected.is_empty() {
                kids.push(Node::new("round/ejected", format!("{D}ejected this round: {}{R}", r.ejected.join(" "))));
            }
            Node::new(
                "round",
                format!("{B}ROUND{R}  {B}{}{R} · pass {B}{}{R} · {} · {} · {} member(s)", r.name, r.passes, r.age, r.state, r.members.len()),
            )
            .kids(kids)
        }
    });

    let mut aeons: Vec<Node> = v
        .now_items
        .iter()
        .map(|i| {
            let name = i.who.split('@').next().unwrap_or("");
            let who = match (i.persona.as_str(), name) {
                ("—" | "", n) => n.to_string(),
                (p, "") => p.to_string(),
                (p, n) => format!("{p}/{n}"),
            };
            let tag = if i.rework { format!("{YEL}{B}REWORK{R} ") } else { String::new() };
            let c = match i.out_level.as_str() {
                "bad" => RED,
                "warn" => YEL,
                _ => D,
            };
            let age = if i.out_age.is_empty() { String::new() } else { format!(" {} ago", i.out_age) };
            let out = i
                .out
                .iter()
                .enumerate()
                .map(|(k, l)| {
                    let tail = if k + 1 == i.out.len() { age.as_str() } else { "" };
                    Node::new(format!("now/{}/{k}", i.id), format!("{c}{l}{tail}{R}"))
                })
                .collect();
            Node::new(format!("now/{}", i.id), format!("{CYN}{:<14}{R} {:<12} {tag}{} {D}{} · {}{R} {}", cut(&who, 14), i.id, i.prio, i.age, i.note, i.title))
                .kids(out)
                .fold(R_NOW_FOLD)
        })
        .collect();
    if aeons.is_empty() {
        aeons.push(Node::new("now/none", format!("{D}nothing is being worked{R}")));
    }
    out.push(Node::new("now", format!("{B}NOW{R}    {B}{}{R} working {D}of {} — holder · bead · lease{R}", v.now_items.len(), v.ceiling)).kids(aeons));

    let mut states = Vec::new();
    for st in &v.machine {
        let sk = format!("state/{}", st.name);
        let mut moves = Vec::new();
        for e in &st.edges {
            let glyph = if e.main { "│" } else { "└▶" };
            let label = if e.main { e.event.clone() } else { format!("{} ▶ {}", e.event, e.to) };
            let events = e
                .events
                .iter()
                .map(|(name, refused, why)| {
                    let r = if *refused > 0 { format!("  {RED}refused {refused}/h{R} {D}{why}{R}") } else { String::new() };
                    Node::new(format!("{sk}/{}/{name}", e.to), format!("{D}event{R} {name}{r}"))
                })
                .collect();
            let mut t = Node::new(format!("{sk}/{}", e.to), format!("{D}{glyph}{R} {:<27}{:>4}/h", cut(&label, 27), e.rate_1h)).kids(events).closed();
            if e.rate_1h == 0 {
                t = t.elide(R_IDLE_EDGE);
            }
            moves.push(t);
        }
        if st.no_rework_exit {
            moves.push(Node::new(format!("{sk}/no-rework-exit"), format!("{RED}└▶ (no exit to REWORK)  ●{R}")));
        }
        // The beads themselves, under their state (per Ryan 2026-10-09, replacing PIPE).
        let beads = st
            .beads
            .iter()
            .map(|i| {
                // REWORK shows the rejection type up front and its evidence folded underneath
                // (ported from the retired REWORK section, per Ryan 2026-10-09).
                let kind = if i.note.is_empty() || st.name != "REWORK" { String::new() } else { format!("{YEL}[{}]{R} ", i.note) };
                let note = if i.note.is_empty() || st.name == "REWORK" { String::new() } else { format!(" {D}· {}{R}", i.note) };
                let n = Node::new(format!("{sk}/{}", i.id), format!("{:<12} {D}{:>4}{R} {} {kind}{}{note}", i.id, i.age, i.prio, i.title));
                if i.why.is_empty() {
                    n
                } else {
                    n.kids(vec![Node::new(format!("{sk}/{}/why", i.id), format!("{D}{}{R}", i.why))]).closed()
                }
            })
            .collect();
        moves.extend(ranked(beads, &[(3, R_STATE_BEADS_MORE), (0, R_STATE_BEADS)]));
        let c = if st.name == "REWORK" { YEL } else { "" };
        let detail = if st.detail.is_empty() { String::new() } else { format!("  {}", st.detail) };
        let mut n = Node::new(sk, format!("{c}{B}{:<12}{R}{B}{:>4}{R}{detail}{}", st.name, st.count, dot(st.red))).kids(moves).fold(R_STATE_FOLD);
        if st.count == 0 && !st.red && !st.no_rework_exit {
            n = n.elide(R_EMPTY_STATE);
        }
        states.push(n);
    }
    states.push(Node::new("state/terminal", format!("{D}terminal 24h  {}{R}", v.terminal)).elide(R_TERMINAL));
    out.push(Node::new("state", format!("{B}STATE MACHINE{R}  {D}counts now · moves per hour{R}")).kids(states));

    let next = v.next.iter().map(|i| Node::new(format!("next/{}", i.id), format!("{} {:<12} {}", i.prio, i.id, i.title))).collect();
    out.push(
        Node::new("next", format!("{B}NEXT{R}   {B}{}{R} claimable {D}(express, REWORK, then priority){R}", v.next_count))
            .kids(ranked(next, &[(5, R_NEXT_SOME), (2, R_NEXT_FEW), (0, R_NEXT_ALL)])),
    );

    let blocked = v.blocked.iter().map(|i| Node::new(format!("blocked/{}", i.id), format!("{} {:<26} {}", i.prio, i.note, i.title))).collect();
    out.push(
        Node::new("blocked", format!("{B}BLOCKED{R} {B}{}{R} {D}READY, waiting on a dependency{R}", v.blocked.len()))
            .kids(ranked(blocked, &[(3, R_BLOCKED_SOME), (0, R_BLOCKED_ALL)])),
    );

    let drift = v.drift.iter().map(|d| Node::new(format!("drift/{d}"), format!("{RED}{d}{R}"))).collect();
    out.push(
        Node::new(
            "drift",
            if v.drift.is_empty() {
                format!("{B}DRIFT{R}  {D}none{R}")
            } else {
                format!("{RED}{B}DRIFT{R} {RED}{} READY/REWORK bead(s) whose commit is already on {}{R}", v.drift.len(), v.base)
            },
        )
        .kids(ranked(drift, &[(0, R_DRIFT)])),
    );

    let refused = v
        .refused
        .iter()
        .map(|r| Node::new(format!("refused/{}", r.what), format!("{:<24}{B}{:>4}{R}  {}{}", cut(&r.what, 24), r.n, r.why, dot(r.red))))
        .collect();
    out.push(
        Node::new("refused", format!("{B}REFUSED{R} {D}(1h){R} {B}{}{R}", v.refused.iter().map(|r| r.n).sum::<i64>()))
            .kids(ranked(refused, &[(2, R_REFUSED_SOME), (0, R_REFUSED_ALL)])),
    );

    let holds = v
        .holds
        .iter()
        .map(|g| {
            let c = if g.kind == "poison" { RED } else { YEL };
            let reasons = g.top.iter().enumerate().map(|(k, (n, r))| Node::new(format!("holds/{}/{k}", g.kind), format!("{n}× {r}"))).collect();
            Node::new(format!("holds/{}", g.kind), format!("{c}{:<9}{R}{B}{:>4}{R}", g.kind, g.count)).kids(reasons).fold(R_HOLD_FOLD).elide(R_HOLD_ALL)
        })
        .collect();
    let hold_sum = v.holds.iter().map(|g| format!("{} {}", g.kind, g.count)).collect::<Vec<_>>().join(" · ");
    out.push(Node::new("holds", format!("{B}HOLDS{R}  {}", if hold_sum.is_empty() { format!("{D}none{R}") } else { hold_sum })).kids(holds));

    let recent = v
        .recent
        .iter()
        .map(|i| {
            let c = match i.state.as_str() {
                "LANDED" => GRN,
                "REWORK" | "DROPPED" => YEL,
                _ => "",
            };
            Node::new(format!("recent/{}", i.id), format!("{D}{:>4}{R} {:<12} {c}{:<11}{R} {}", i.age, i.id, i.state, i.title))
        })
        .collect();
    out.push(Node::new("recent", format!("{B}RECENT{R} {D}last transitions{R}")).kids(ranked(recent, &[(3, R_RECENT_SOME), (0, R_RECENT_ALL)])));
    out
}

/// Visible width of a line, ignoring ANSI escapes.
fn visible_len(s: &str) -> usize {
    let mut n = 0;
    let mut esc = false;
    for c in s.chars() {
        if esc {
            if c.is_ascii_alphabetic() {
                esc = false;
            }
        } else if c == '\x1b' {
            esc = true;
        } else {
            n += 1;
        }
    }
    n
}

/// Cut a line to `w` visible columns without breaking an escape, and reset attributes at the end.
pub fn clip(s: &str, w: usize) -> String {
    if visible_len(s) <= w {
        return format!("{s}{R}");
    }
    let mut out = String::new();
    let mut n = 0;
    let mut esc = false;
    for c in s.chars() {
        if esc {
            out.push(c);
            if c.is_ascii_alphabetic() {
                esc = false;
            }
            continue;
        }
        if c == '\x1b' {
            esc = true;
            out.push(c);
            continue;
        }
        if n + 1 >= w {
            out.push('…');
            break;
        }
        out.push(c);
        n += 1;
    }
    out.push_str(R);
    out
}

struct Flat {
    key: String,
    line: String,
    depth: usize,
    parent: Option<usize>,
    kids: Vec<usize>,
    elide: Option<u16>,
    fold: Option<u16>,
    auto_open: bool,
}

fn flatten(level: Vec<Node>, depth: usize, parent: Option<usize>, out: &mut Vec<Flat>) -> Vec<usize> {
    let mut ids = Vec::new();
    for n in level {
        let i = out.len();
        out.push(Flat { key: n.key, line: n.line, depth, parent, kids: Vec::new(), elide: n.elide, fold: n.fold, auto_open: n.auto_open });
        let kids = flatten(n.kids, depth + 1, Some(i), out);
        out[i].kids = kids;
        ids.push(i);
    }
    ids
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Show {
    Hidden,
    Folded,
    Open,
}

fn walk(a: &[Flat], ids: &[usize], show: &[Show], out: &mut Vec<usize>) {
    for &i in ids {
        if show[i] == Show::Hidden {
            continue;
        }
        out.push(i);
        if show[i] == Show::Open {
            walk(a, &a[i].kids, show, out);
        }
    }
}

/// Lay the tree out into at most `h` rows of at most `w` columns.
pub fn layout(v: &View, ui: &Ui, w: usize, h: usize) -> Frame {
    let top = banner(v);
    let mut a = Vec::new();
    let roots = flatten(tree(v), 0, None, &mut a);
    let n = a.len();
    // Under an OPEN ancestor nothing is cut to save space, but a node whose detail is folded by
    // default (a transition's events, a bead's title) stays folded until opened itself.
    let forced: Vec<bool> = (0..n)
        .map(|i| {
            let mut p = a[i].parent;
            while let Some(j) = p {
                if ui.mode(&a[j].key) == Mode::Open {
                    return true;
                }
                p = a[j].parent;
            }
            false
        })
        .collect();
    let mut show: Vec<Show> = (0..n)
        .map(|i| match ui.mode(&a[i].key) {
            Mode::Collapsed => Show::Folded,
            Mode::Open => Show::Open,
            Mode::Auto if a[i].auto_open => Show::Open,
            Mode::Auto => Show::Folded,
        })
        .collect();
    let body_h = h.saturating_sub(top.len()).max(1);
    let visible = |show: &[Show]| {
        let mut out = Vec::new();
        walk(&a, &roots, show, &mut out);
        out
    };
    // AUTO cuts, cheapest first; within a rank the deepest and the last on screen go first.
    let mut cuts: Vec<(u16, usize, usize, bool)> = Vec::new();
    for (i, f) in a.iter().enumerate() {
        if ui.mode(&f.key) != Mode::Auto || forced[i] {
            continue;
        }
        if let Some(r) = f.fold {
            cuts.push((r, f.depth, i, false));
        }
        if let Some(r) = f.elide {
            cuts.push((r, f.depth, i, true));
        }
    }
    cuts.sort_by(|x, y| x.0.cmp(&y.0).then(y.1.cmp(&x.1)).then(y.2.cmp(&x.2)));
    for (_, _, i, elide) in cuts {
        if visible(&show).len() <= body_h {
            break;
        }
        if elide {
            show[i] = Show::Hidden;
        } else if show[i] == Show::Open {
            show[i] = Show::Folded;
        }
    }

    let vis = visible(&show);
    let index: HashMap<&str, usize> = a.iter().enumerate().map(|(i, f)| (f.key.as_str(), i)).collect();
    // The selection, or its nearest visible ancestor when AUTO removed it.
    let mut sel = index.get(ui.selected.as_str()).copied();
    while let Some(i) = sel {
        if vis.contains(&i) {
            break;
        }
        sel = a[i].parent;
    }
    let sel = sel.or_else(|| vis.first().copied());

    let mut body = Vec::new();
    let mut keys = Vec::new();
    let mut sel_row = 0;
    for &i in &vis {
        let f = &a[i];
        let marker = if f.kids.is_empty() {
            " "
        } else {
            match (ui.mode(&f.key), show[i]) {
                (Mode::Collapsed, _) => "▸",
                (Mode::Open, _) => "▼",
                (Mode::Auto, Show::Open) => "▿",
                _ => "▹",
            }
        };
        let hidden = match show[i] {
            Show::Open => f.kids.iter().filter(|&&k| show[k] == Show::Hidden).count(),
            _ => f.kids.len(),
        };
        let note = if hidden > 0 { format!(" {D}(+{hidden}){R}") } else { String::new() };
        let line = format!("{}{marker} {}{note}", "  ".repeat(f.depth), f.line);
        if Some(i) == sel {
            sel_row = body.len();
            body.push(format!("{REV}{}", clip(&line, w)));
        } else {
            body.push(line);
        }
        keys.push(f.key.clone());
    }
    // Still too tall (OPEN is never cut): a window that keeps the selection in view.
    let mut start = 0;
    if body.len() > body_h {
        let max_start = body.len() - body_h;
        start = (sel_row.saturating_sub(body_h / 3) + ui.scroll).min(max_start);
        if sel_row < start {
            start = sel_row;
        }
    }
    let end = (start + body_h).min(body.len());
    let mut lines: Vec<String> = top.iter().map(|l| clip(l, w)).collect();
    let mut rows = Vec::new();
    for (r, k) in (start..end).enumerate() {
        let text = if r == 0 && start > 0 {
            format!("{D}↑ {start} more above{R}")
        } else if k + 1 == end && end < body.len() {
            format!("{D}↓ {} more below{R}", body.len() - end)
        } else {
            rows.push((lines.len(), keys[k].clone()));
            body[k].clone()
        };
        lines.push(clip(&text, w));
    }
    lines.truncate(h);
    Frame {
        lines,
        rows,
        order: keys,
        nodes: vis.iter().map(|&i| (a[i].key.clone(), (!a[i].kids.is_empty(), show[i] == Show::Open))).collect(),
        all_keys: a.into_iter().map(|f| f.key).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lcview::{Item, RoundMember, RoundView, SmEdge, SmState};

    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut esc = false;
        for c in s.chars() {
            if esc {
                if c.is_ascii_alphabetic() {
                    esc = false;
                }
            } else if c == '\x1b' {
                esc = true;
            } else {
                out.push(c);
            }
        }
        out
    }

    fn st(name: &str, count: usize, rates: &[i64]) -> SmState {
        SmState {
            name: name.into(),
            count,
            edges: rates
                .iter()
                .enumerate()
                .map(|(k, &r)| SmEdge {
                    event: format!("ev{k}"),
                    events: vec![(format!("ev{k}-a"), 2, "illegal".into()), (format!("ev{k}-b"), 0, String::new())],
                    to: format!("TO{k}"),
                    rate_1h: r,
                    main: false,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn busy_view() -> View {
        let item = |id: &str| Item {
            id: id.into(),
            prio: "P1".into(),
            title: format!("title of {id}"),
            who: "anima".into(),
            age: "5m".into(),
            persona: "builder".into(),
            out: vec!["▸ cargo test".into(), "▸ git commit".into()],
            ..Default::default()
        };
        View {
            clock: "16:00Z".into(),
            ceiling: 6,
            round: Some(RoundView {
                name: "r-auto-96".into(),
                state: "OPEN".into(),
                age: "13m".into(),
                passes: 2,
                ejected: vec!["sp-x".into()],
                members: (0..4).map(|k| RoundMember { id: format!("sp-m{k}"), prio: "P1".into(), title: "member".into(), ejects: k }).collect(),
            }),
            now_items: (0..3).map(|k| item(&format!("sp-w{k}"))).collect(),
            machine: vec![
                st("OPEN", 4, &[0]),
                st("READY", 10, &[3, 0]),
                st("WORKING", 2, &[6, 0, 0]),
                st("SUBMITTED", 0, &[0, 0]),
                st("CERTIFIED", 0, &[0]),
                st("IN_DELIVERY", 4, &[6, 1]),
                st("LANDED", 106, &[]),
                st("REWORK", 1, &[5]),
            ],
            next: (0..12).map(|k| item(&format!("sp-n{k}"))).collect(),
            next_count: 12,
            recent: (0..10).map(|k| item(&format!("sp-r{k}"))).collect(),
            rework_items: (0..5).map(|k| item(&format!("sp-k{k}"))).collect(),
            ..Default::default()
        }
    }

    fn text(f: &Frame) -> Vec<String> {
        f.lines.iter().map(|l| strip(l)).collect()
    }

    fn has(t: &[String], s: &str) -> bool {
        t.iter().any(|l| l.contains(s))
    }

    fn state_line(t: &[String], name: &str) -> bool {
        t.iter().any(|l| l.trim_start().trim_start_matches(['▿', '▹', '▸', '▼', ' ']).starts_with(name))
    }

    #[test]
    fn a_70x50_pane_shows_round_and_every_aeon_and_never_wraps() {
        let t = text(&layout(&busy_view(), &Ui::default(), 70, 50));
        assert!(t.len() <= 50, "{} rows", t.len());
        assert!(t.iter().all(|l| l.chars().count() <= 70), "{t:#?}");
        assert!(has(&t, "r-auto-96") && has(&t, "pass 2"), "{t:#?}");
        for k in 0..3 {
            assert!(has(&t, &format!("sp-w{k}")), "aeon sp-w{k} missing: {t:#?}");
        }
        assert!(has(&t, "STATE MACHINE"));
    }

    #[test]
    fn a_120x81_pane_fits_and_keeps_more_than_a_70x50_one() {
        let v = busy_view();
        let big = text(&layout(&v, &Ui::default(), 120, 81));
        let small = text(&layout(&v, &Ui::default(), 70, 50));
        assert!(big.len() <= 81 && big.iter().all(|l| l.chars().count() <= 120), "{big:#?}");
        assert!(big.len() > small.len(), "the taller pane shows more: {} vs {}", big.len(), small.len());
        assert!(state_line(&big, "SUBMITTED"), "room for empty states at 81 rows: {big:#?}");
    }

    #[test]
    fn a_pane_shorter_than_the_essentials_still_fits_exactly() {
        let t = text(&layout(&busy_view(), &Ui::default(), 70, 12));
        assert_eq!(t.len(), 12, "{t:#?}");
        assert!(t[0].contains("LIFECYCLE"));
    }

    #[test]
    fn idle_transitions_go_before_empty_states() {
        let v = busy_view();
        let mut saw_idle_gone_states_kept = false;
        for h in (20..=120).rev() {
            let t = text(&layout(&v, &Ui::default(), 80, h));
            let idle = t.iter().any(|l| l.trim_end().ends_with(" 0/h"));
            let empty_state = state_line(&t, "SUBMITTED");
            assert!(!(idle && !empty_state), "an empty state went while an idle transition stayed (h={h}): {t:#?}");
            saw_idle_gone_states_kept |= !idle && empty_state;
        }
        assert!(saw_idle_gone_states_kept, "some height shows empty states without idle transitions");
    }

    #[test]
    fn an_elided_state_comes_back_once_it_is_not_empty() {
        let mut v = busy_view();
        assert!(!state_line(&text(&layout(&v, &Ui::default(), 70, 30)), "SUBMITTED"), "elided while empty on a short pane");
        v.machine.iter_mut().find(|s| s.name == "SUBMITTED").unwrap().count = 3;
        let t = text(&layout(&v, &Ui::default(), 70, 30));
        assert!(state_line(&t, "SUBMITTED"), "{t:#?}");
    }

    #[test]
    fn one_state_collapses_while_its_siblings_stay_auto() {
        let v = busy_view();
        let mut ui = Ui::default();
        ui.set("state/WORKING", Mode::Collapsed);
        let t = text(&layout(&v, &ui, 100, 200));
        assert!(t.iter().any(|l| l.contains("▸ WORKING") && l.contains("(+3)")), "{t:#?}");
        assert!(!has(&t, "ev2 ▶ TO2"), "WORKING is the only state with a third move, and it is collapsed: {t:#?}");
        assert!(has(&t, "ev1 ▶ TO1"), "the other states' moves still show: {t:#?}");
    }

    #[test]
    fn a_transition_opens_inside_an_auto_state() {
        let v = busy_view();
        let mut ui = Ui::default();
        assert!(!has(&text(&layout(&v, &ui, 100, 200)), "event ev0-a"), "transitions start folded");
        ui.set("state/READY/TO0", Mode::Open);
        let t = text(&layout(&v, &ui, 100, 200));
        assert!(has(&t, "event ev0-a") && has(&t, "refused 2/h"), "{t:#?}");
        assert_eq!(t.iter().filter(|l| l.contains("event ev0-a")).count(), 1, "only READY's transition opened");
    }

    #[test]
    fn a_states_beads_fold_under_it_and_show_when_it_is_opened() {
        let mut v = busy_view();
        let item = |id: &str| Item { id: id.into(), prio: "P1".into(), title: format!("t {id}"), age: "3m".into(), ..Default::default() };
        v.machine.iter_mut().find(|s| s.name == "SUBMITTED").unwrap().beads = (0..8).map(|k| item(&format!("sp-sub{k}"))).collect();
        v.machine.iter_mut().find(|s| s.name == "SUBMITTED").unwrap().count = 8;
        let mut saw_three = false;
        for h in (12..=120).rev() {
            let t = text(&layout(&v, &Ui::default(), 80, h));
            let idle = t.iter().any(|l| l.trim_end().ends_with(" 0/h"));
            let (first, fourth) = (has(&t, "sp-sub0"), has(&t, "sp-sub3"));
            assert!(!(idle && !fourth && state_line(&t, "SUBMITTED")), "a bead went before an idle transition (h={h}): {t:#?}");
            assert!(!(fourth && !first), "a later bead outlived the oldest (h={h})");
            saw_three |= first && has(&t, "sp-sub2") && !fourth && !idle;
        }
        assert!(saw_three, "some height shows a state's oldest three beads without the rest or any idle transition");
        let mut ui = Ui::default();
        ui.set("state/SUBMITTED", Mode::Open);
        let t = text(&layout(&v, &ui, 70, 200));
        assert!(has(&t, "sp-sub0") && has(&t, "sp-sub7"), "an opened state lists every bead: {t:#?}");
        assert!(!has(&t, "event ev0-a"), "opening a state does not spill its transitions' events: {t:#?}");
        assert!(!t.iter().any(|l| l.contains("PIPE")), "PIPE is gone");
    }

    #[test]
    fn a_rework_bead_shows_its_rejection_type_and_folds_its_evidence() {
        let mut v = busy_view();
        let rw = Item { id: "sp-rw1".into(), prio: "P1".into(), title: "fix it".into(), age: "4m".into(), note: "suites-failed".into(), why: "test-x #3 red: wanted 1 got 0".into(), ..Default::default() };
        v.machine.iter_mut().find(|s| s.name == "REWORK").unwrap().beads = vec![rw];
        let t = text(&layout(&v, &Ui::default(), 100, 200));
        assert!(t.iter().any(|l| l.contains("sp-rw1") && l.contains("[suites-failed]")), "{t:#?}");
        assert!(!has(&t, "wanted 1 got 0"), "the evidence starts folded");
        assert!(!t.iter().any(|l| l.trim_start().trim_start_matches(['▿', '▹', '▸', '▼', ' ']).starts_with("REWORK") && l.contains("sent back")), "no top-level REWORK section");
        let mut ui = Ui::default();
        ui.set("state/REWORK/sp-rw1", Mode::Open);
        assert!(has(&text(&layout(&v, &ui, 100, 200)), "wanted 1 got 0"), "opening the bead shows why");
    }

    #[test]
    fn auto_shrinks_around_what_the_operator_opened() {
        let v = busy_view();
        let mut ui = Ui::default();
        ui.set("recent", Mode::Open);
        let t = text(&layout(&v, &ui, 70, 50));
        assert!(has(&t, "sp-r9"), "an OPEN section is shown whole: {t:#?}");
        assert!(has(&t, "sp-w0"), "NOW still shows");
    }

    #[test]
    fn reset_returns_a_subtree_to_auto() {
        let mut ui = Ui::default();
        ui.set("state", Mode::Open);
        ui.set("state/READY", Mode::Collapsed);
        ui.set("state/READY/TO0", Mode::Open);
        ui.set("now", Mode::Collapsed);
        ui.reset("state");
        assert_eq!(ui.modes.len(), 1);
        assert_eq!(ui.mode("now"), Mode::Collapsed);
        assert_eq!(parent_key("state/READY/TO0"), Some("state/READY"));
    }

    #[test]
    fn every_drawn_row_names_its_node() {
        let f = layout(&busy_view(), &Ui::default(), 70, 50);
        assert!(!f.rows.is_empty());
        for (row, key) in &f.rows {
            assert!(f.all_keys.contains(key), "{key}");
            assert!(*row < f.lines.len());
        }
    }

    #[test]
    fn a_round_counts_one_pass_per_attribution_not_per_eject() {
        use crate::lcview::passes;
        assert_eq!(passes(&[]), 1);
        assert_eq!(passes(&[1000, 1012]), 2, "two ejects from one red pass");
        assert_eq!(passes(&[1000, 1012, 2000]), 3);
    }

    #[test]
    fn clip_never_breaks_an_escape_or_exceeds_the_width() {
        let s = format!("{B}abcdefghij{R}{RED}klmnop{R}");
        let c = clip(&s, 8);
        assert_eq!(strip(&c).chars().count(), 8);
        assert!(c.ends_with(R));
    }
}
