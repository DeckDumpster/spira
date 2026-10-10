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
    /// The line drawn instead of `line` while the node is showing its children.
    pub open_line: Option<String>,
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
    fn open_line(mut self, l: impl Into<String>) -> Node {
        self.open_line = Some(l.into());
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
        "{B}SPIRA{R} {D}{}{R}  release {B}{}{R}  {}  aeons {B}{}/{}{R}",
        v.clock,
        v.release,
        if v.world_running { format!("{GRN}world RUNNING{R}") } else { format!("{RED}world {}{R}", cut(&v.world, 24)) },
        v.working,
        v.ceiling
    )];
    // The running pass, pinned under the banner so it is visible whatever is collapsed or
    // scrolled (per Ryan 2026-10-09); ROUND's pass subsection keeps the detail.
    if let Some(p) = v.progress.as_ref().filter(|p| p.running(v.now)) {
        out.push(pass_line(p, v.now));
    }
    if !v.errors.is_empty() {
        let first = v.errors.first().cloned().unwrap_or_default();
        let more = if v.errors.len() > 1 { format!(" (+{} more)", v.errors.len() - 1) } else { String::new() };
        out.push(format!("{RED}  source failed: {}{more}{R}", cut(&first, 90)));
    }
    out
}

/// The whole pane as a tree, in Ryan's order: DECIDE, ROUND, NOW, STATE MACHINE (with each state's
/// beads under it; PIPE and REWORK retired 2026-10-09), NEXT, and DRIFT only as an alarm,
/// BLOCKED, DRIFT, REFUSED, HOLDS, RECENT.
pub fn tree(v: &View) -> Vec<Node> {
    let mut out = Vec::new();

    // DECIDE: what is waiting on the operator, above everything else; it is never work.
    let asks: Vec<Node> = v.decide.iter().map(|i| Node::new(format!("decide/{}", i.id), format!("{YEL}{:<12}{R} {D}{:>4}{R} {}", i.id, i.age, i.title))).collect();
    // Only when something waits on him (per Ryan 2026-10-09).
    if !asks.is_empty() {
        out.push(Node::new("decide", format!("{YEL}{B}DECIDE{R} {B}{}{R} {D}waiting on you{R}", asks.len())).kids(asks));
    }

    // DRIFT is an alarm, not a section (per Ryan 2026-10-09): beads whose own commit is on the
    // base while the lifecycle still says READY/REWORK. The content-on-base reconciler closes
    // these within a pass, so a line here means it is behind or down.
    if !v.drift.is_empty() {
        let drift = v.drift.iter().map(|d| Node::new(format!("drift/{d}"), format!("{RED}{d}{R}"))).collect();
        out.push(
            Node::new("drift", format!("{RED}{B}DRIFT{R} {RED}{} READY/REWORK bead(s) already on {} — the content-on-base reconciler is behind{R}", v.drift.len(), v.base))
                .kids(ranked(drift, &[(3, R_DRIFT)])),
        );
    }

    out.push(match &v.round {
        None => Node::new("round", format!("{B}ROUND{R}  {D}none open{R}")),
        Some(r) => {
            let member_nodes = |round: &crate::lcview::RoundView, key: &str| -> Vec<Node> {
                round
                    .members
                    .iter()
                    .map(|m| {
                        let ej = if m.ejects > 0 { format!("{YEL}ejected {}× before{R} ", m.ejects) } else { String::new() };
                        Node::new(format!("{key}/{}", m.id), format!("{:<14} {} {ej}{}", m.id, m.prio, m.title))
                    })
                    .collect()
            };
            let mut kids = member_nodes(r, "round");
            for c in &r.staged {
                let key = format!("round/staged/{}", c.name);
                let head = format!("{B}{}{R} · {} behind {} · fenced · {} member(s)", c.name, c.state, r.name, c.members.len());
                kids.push(Node::new(key.clone(), head).kids(member_nodes(c, &key)));
            }
            if !r.ejected.is_empty() {
                kids.push(Node::new("round/ejected", format!("{D}ejected this round: {}{R}", r.ejected.join(" "))));
            }
            if let Some(p) = v.progress.as_ref().filter(|p| r.name.starts_with(&p.round)) {
                kids.insert(0, pass_node(p, v.now));
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
            // The bead's title is the first line of the expanded view, whole: on the aeon's own
            // line it was cut short by everything else there (per Ryan 2026-10-09).
            let title = Node::new(format!("now/{}/title", i.id), format!("{B}{}{R}", i.title));
            let out = std::iter::once(title).chain(i
                .out
                .iter()
                .enumerate()
                .map(|(k, l)| {
                    let tail = if k + 1 == i.out.len() { age.as_str() } else { "" };
                    Node::new(format!("now/{}/{k}", i.id), format!("{c}{l}{tail}{R}"))
                }))
                .collect();
            let head = format!("{CYN}{:<14}{R} {:<12} {tag}{} {D}{} · {}{R}", cut(&who, 14), i.id, i.prio, i.age, i.note);
            Node::new(format!("now/{}", i.id), format!("{head} {}", i.title))
                .open_line(head)
                .kids(out)
                .fold(R_NOW_FOLD)
        })
        .collect();
    if aeons.is_empty() {
        aeons.push(Node::new("now/none", format!("{D}no aeon is live{R}")));
    }
    out.push(Node::new("now", format!("{B}NOW{R}    {B}{}{R} live {D}of {} — aeon · bead · phase{R}", v.now_items.len(), v.ceiling)).kids(aeons));

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
    out.push(Node::new("state", format!("{B}STATES{R}  {D}counts now · moves per hour{R}")).kids(states));

    let next = v.next.iter().map(|i| Node::new(format!("next/{}", i.id), format!("{} {:<12} {}", i.prio, i.id, i.title))).collect();
    out.push(
        Node::new(
            "next",
            if v.next_unknown {
                format!("{B}NEXT{R}   {YEL}unknown{R} {D}— the claim tool has not answered{R}")
            } else {
                format!("{B}NEXT{R}   {B}{}{R} claimable {D}(the claim tool's own set){R}", v.next_count)
            },
        )
            .kids(ranked(next, &[(5, R_NEXT_SOME), (2, R_NEXT_FEW), (0, R_NEXT_ALL)])),
    );

    let blocked = v.blocked.iter().map(|i| Node::new(format!("blocked/{}", i.id), format!("{} {:<26} {}", i.prio, i.note, i.title))).collect();
    if !v.blocked.is_empty() {
        out.push(
            Node::new("blocked", format!("{B}BLOCKED{R} {B}{}{R} {D}READY, waiting on a dependency{R}", v.blocked.len()))
                .kids(ranked(blocked, &[(3, R_BLOCKED_SOME), (0, R_BLOCKED_ALL)])),
        );
    }

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

/// One line for a running pass: the round, the phase, and in suites a bar of suites done of
/// total, the reds, and the time against the cap (yellow past the 12-minute goal, red past it).
fn pass_line(p: &crate::lcview::PassProgress, now: i64) -> String {
    let head = format!("{CYN}⟳{R} {B}{}{R} pass {}", p.round, p.pass);
    match p.phase.as_str() {
        "fences" => format!("{head} · fences on the merged head {D}{}m, outside the cap{R}", (now - p.build_started) / 60),
        "build" => format!("{head} · building {D}{}m, outside the cap{R}", (now - p.build_started) / 60),
        _ => {
            let total = p.total.max(1);
            let w = 20usize;
            let fill = (w as u32 * p.done.min(total) / total) as usize;
            let el = now - p.suites_started;
            let tc = if el > p.cap { RED } else if el > 720 { YEL } else { "" };
            let red = if p.red.is_empty() { "0 red".to_string() } else { format!("{RED}{} red{R}", p.red.len()) };
            format!("{head} · suites ▕{}{}▏ {}/{} · {red} · {tc}{}m{:02}s of {}m{R}", "█".repeat(fill), "░".repeat(w - fill), p.done, p.total, el / 60, el % 60, p.cap / 60)
        }
    }
}

/// The current pass, as a subsection of ROUND, its bar on its own line so it shows without
/// opening anything (per Ryan 2026-10-09); its red suites are the children, open while it runs.
fn pass_node(p: &crate::lcview::PassProgress, now: i64) -> Node {
    let running = p.running(now);
    let bar = || {
        let total = p.total.max(1);
        let w = 20usize;
        let fill = (w as u32 * p.done.min(total) / total) as usize;
        let el = if running { now - p.suites_started } else { p.updated_at - p.suites_started };
        let tc = if el > p.cap { RED } else if el > 720 { YEL } else { "" };
        let red = if p.red.is_empty() { "0 red".to_string() } else { format!("{RED}{} red{R}", p.red.len()) };
        format!("▕{}{}▏ {}/{} · {red} · {tc}{}m{:02}s of {}m{R}", "█".repeat(fill), "░".repeat(w - fill), p.done, p.total, el / 60, el % 60, p.cap / 60)
    };
    let head = match (running, p.phase.as_str()) {
        (true, "fences") => format!("{B}pass {}{R} {CYN}fences{R} {D}on the merged head, {}m — outside the cap{R}", p.pass, (now - p.build_started) / 60),
        (true, "build") => format!("{B}pass {}{R} {CYN}building{R} {D}{}m — outside the cap{R}", p.pass, (now - p.build_started) / 60),
        (true, _) => format!("{B}pass {}{R} {}", p.pass, bar()),
        (false, "done") => {
            let c = if p.verdict == "green" { GRN } else { RED };
            let tail = if p.suites_started > 0 { format!(" {}", bar()) } else { String::new() };
            format!("{B}pass {}{R} {c}{}{R}{tail} {D}{} ago{R}", p.pass, p.verdict, crate::lcview::age(now - p.updated_at))
        }
        _ => format!("{B}pass {}{R} {D}no word from the pass for {}{R}", p.pass, crate::lcview::age(now - p.updated_at)),
    };
    let kids = p.red.iter().map(|s| Node::new(format!("round/pass/{s}"), format!("{RED}{s}{R}"))).collect();
    let n = Node::new("round/pass", head).kids(kids);
    if running { n } else { n.closed() }
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
    open_line: Option<String>,
}

fn flatten(level: Vec<Node>, depth: usize, parent: Option<usize>, out: &mut Vec<Flat>) -> Vec<usize> {
    let mut ids = Vec::new();
    for n in level {
        let i = out.len();
        out.push(Flat { key: n.key, line: n.line, depth, parent, kids: Vec::new(), elide: n.elide, fold: n.fold, auto_open: n.auto_open, open_line: n.open_line });
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
        let text = match (&f.open_line, show[i]) {
            (Some(l), Show::Open) => l,
            _ => &f.line,
        };
        let line = format!("{}{marker} {}{note}", "  ".repeat(f.depth), text);
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

// ───────────────────────────── the web page: the same tree, as HTML

/// The `/lifecycle` page: the pane's own tree (`tree`) and banner, rendered as nested
/// collapsible sections, so the web and the pane cannot drift apart. Every node is drawn, with
/// its key as `data-key`. A section is open by default exactly where the pane's AUTO mode opens
/// it, and the viewer's own open/close choices are remembered per browser. The page refreshes
/// itself from `?fragment=1` every few seconds without losing them (per Ryan 2026-10-09: "a
/// web-based version of the same information ... the two don't fall out of sync", with test
/// progress and the interactive parts).
pub fn render_page(v: &View, stale: Option<i64>, poll_s: u64) -> String {
    format!(
        "<!doctype html><html lang=en><head><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'>\
<title>Spira lifecycle</title><style>{PAGE_CSS}</style></head><body>\
<header id=banner>{}</header><nav><a href=/stuck>where work is stuck →</a> · <button id=reset type=button>reset open/closed</button></nav>\
<main id=tree>{}</main><script>const POLL_MS={};{PAGE_JS}</script></body></html>",
        banner_html(v, stale),
        tree_html(v),
        poll_s * 1000
    )
}

/// The two live parts of the page, for its poll: `{"banner": html, "tree": html}`.
pub fn render_fragment(v: &View, stale: Option<i64>) -> String {
    serde_json::json!({ "banner": banner_html(v, stale), "tree": tree_html(v) }).to_string()
}

fn banner_html(v: &View, stale: Option<i64>) -> String {
    let mut h: String = banner(v).iter().map(|l| format!("<div class=line>{}</div>", ansi_html(l))).collect();
    if let Some(age_s) = stale {
        h.push_str(&format!(
            "<div class='line bad'>Snapshot is {} old — the pane's collector is not running.</div>",
            crate::lcview::age(age_s)
        ));
    }
    h
}

fn tree_html(v: &View) -> String {
    let mut h = String::new();
    for n in tree(v) {
        node_html(&n, &mut h);
    }
    h
}

fn node_html(n: &Node, h: &mut String) {
    let key = html_esc(&n.key);
    if n.kids.is_empty() {
        h.push_str(&format!("<div class=leaf data-key=\"{key}\">{}</div>", ansi_html(&n.line)));
        return;
    }
    let head = match &n.open_line {
        Some(o) => format!("<span class=closed-line>{}</span><span class=open-line>{}</span>", ansi_html(&n.line), ansi_html(o)),
        None => ansi_html(&n.line),
    };
    h.push_str(&format!(
        "<details data-key=\"{key}\"{}><summary>{head} <small class=count>({})</small></summary><div class=kids>",
        if n.auto_open { " open" } else { "" },
        n.kids.len()
    ));
    for k in &n.kids {
        node_html(k, h);
    }
    h.push_str("</div></details>");
}

fn html_esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A pane line as HTML: its ANSI styles become classes, the text is escaped, and every bead id
/// links to its `/stuck/<id>` timeline.
/// A pane line without its ANSI styles, for tests that compare the page to the pane.
pub fn strip_for_test(s: &str) -> String {
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

pub fn ansi_html(s: &str) -> String {
    let mut out = String::new();
    let mut open = 0usize;
    let mut text = String::new();
    let mut chars = s.chars().peekable();
    let flush = |text: &mut String, out: &mut String| {
        out.push_str(&link_ids(&html_esc(text)));
        text.clear();
    };
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            let mut code = String::new();
            while let Some(&d) = chars.peek() {
                chars.next();
                if d == 'm' {
                    break;
                }
                code.push(d);
            }
            flush(&mut text, &mut out);
            let class = match code.as_str() {
                "0" | "" => {
                    out.push_str(&"</span>".repeat(open));
                    open = 0;
                    continue;
                }
                "1" => "b",
                "2" => "d",
                "7" => "rev",
                "31" => "red",
                "32" => "grn",
                "33" => "yel",
                "36" => "cyn",
                _ => continue,
            };
            out.push_str(&format!("<span class={class}>"));
            open += 1;
        } else {
            text.push(c);
        }
    }
    flush(&mut text, &mut out);
    out.push_str(&"</span>".repeat(open));
    out
}

/// `sp-<id>` tokens in already-escaped text, as links to their timelines.
fn link_ids(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        let boundary = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'/' || b[i - 1] == b'-');
        if boundary && s[i..].starts_with("sp-") {
            let mut j = i + 3;
            while j < b.len() && (b[j].is_ascii_lowercase() || b[j].is_ascii_digit() || b[j] == b'.') {
                j += 1;
            }
            while j > i + 3 && b[j - 1] == b'.' {
                j -= 1;
            }
            if j > i + 3 {
                let id = &s[i..j];
                out.push_str(&format!("<a href=/stuck/{id}>{id}</a>"));
                i = j;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

const PAGE_CSS: &str = ":root{--bg:#fff;--fg:#111;--dim:#6b7280;--grn:#15803d;--yel:#b45309;--red:#b91c1c;--cyn:#0e7490;--line:#e5e7eb}\
@media (prefers-color-scheme:dark){:root{--bg:#0b0d10;--fg:#e5e7eb;--dim:#9ca3af;--grn:#4ade80;--yel:#fbbf24;--red:#f87171;--cyn:#22d3ee;--line:#1f2937}}\
body{background:var(--bg);color:var(--fg);font:13px/1.45 ui-monospace,SFMono-Regular,Menlo,monospace;margin:0;padding:0 16px 24px;max-width:1000px}\
header{position:sticky;top:0;background:var(--bg);padding:8px 0 6px;border-bottom:1px solid var(--line);z-index:1}\
nav{margin:6px 0 8px;font-size:12px}nav button{font:inherit;font-size:11px;background:none;color:var(--dim);border:1px solid var(--line);border-radius:4px;padding:1px 6px}\
.line,.leaf,summary{white-space:pre-wrap;word-break:break-word}summary{cursor:pointer;padding:2px 0}\
.kids{padding-left:1.2em;border-left:1px solid var(--line);margin-left:.3em}.leaf{padding:1px 0}\
.count{color:var(--dim)}details[open]>summary>.count{display:none}\
details:not([open])>summary>.open-line{display:none}details[open]>summary>.closed-line{display:none}\
a{color:inherit;text-decoration:underline dotted}.b{font-weight:bold}.d{color:var(--dim)}.red,.bad{color:var(--red)}.grn{color:var(--grn)}.yel{color:var(--yel)}.cyn{color:var(--cyn)}\
.rev{background:var(--fg);color:var(--bg)}";

const PAGE_JS: &str = "const K='lcview-open';\
function load(){try{return JSON.parse(localStorage.getItem(K)||'{}')}catch(e){return {}}}\
function save(m){try{localStorage.setItem(K,JSON.stringify(m))}catch(e){}}\
function apply(){const m=load();document.querySelectorAll('details[data-key]').forEach(d=>{const k=d.dataset.key;if(k in m&&d.open!==m[k])d.open=m[k];});}\
document.addEventListener('toggle',e=>{const d=e.target;if(!d.dataset||!d.dataset.key)return;const m=load();m[d.dataset.key]=d.open;save(m);},true);\
document.getElementById('reset').onclick=()=>{save({});location.reload();};\
async function tick(){try{const r=await fetch(location.pathname+'?fragment=1',{cache:'no-store'});if(r.ok){const j=await r.json();\
document.getElementById('banner').innerHTML=j.banner;document.getElementById('tree').innerHTML=j.tree;apply();}}catch(e){}}\
apply();setInterval(tick,POLL_MS);";

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
                staged: Vec::new(),
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
        assert!(has(&t, "STATES"));
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
        assert!(t[0].contains("SPIRA"));
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
    fn empty_decide_and_blocked_do_not_appear_and_the_names_are_spira_and_states() {
        let v = busy_view();
        let t = text(&layout(&v, &Ui::default(), 100, 200));
        assert!(t[0].starts_with("SPIRA"), "{}", t[0]);
        assert!(has(&t, "STATES") && !has(&t, "STATE MACHINE"));
        assert!(!has(&t, "DECIDE") && !has(&t, "BLOCKED"), "empty sections are not drawn: {t:#?}");
    }

    #[test]
    fn drift_is_an_alarm_line_only_when_there_is_drift() {
        let mut v = busy_view();
        assert!(!has(&text(&layout(&v, &Ui::default(), 100, 200)), "DRIFT"), "no drift, no line");
        v.drift = vec!["sp-landed1".into()];
        let t = text(&layout(&v, &Ui::default(), 100, 200));
        let at = t.iter().position(|l| l.contains("DRIFT")).expect("a drift line");
        assert!(at < t.iter().position(|l| l.contains("ROUND")).unwrap(), "the alarm sits above ROUND: {t:#?}");
        assert!(has(&t, "sp-landed1"));
    }

    #[test]
    fn a_staged_round_is_a_child_row_under_the_open_round_and_expands_to_its_members() {
        let mut v = busy_view();
        let mut staged = v.round.clone().unwrap();
        staged.name = "r-stage-1".into();
        staged.state = "STAGED".into();
        staged.members = (0..2).map(|k| RoundMember { id: format!("sp-s{k}"), prio: "P1".into(), title: "staged member".into(), ejects: 0 }).collect();
        v.round.as_mut().unwrap().staged = vec![staged];
        let t = text(&layout(&v, &Ui::default(), 100, 200));
        let at = |s: &str| t.iter().position(|l| l.contains(s)).unwrap_or_else(|| panic!("{s}: {t:#?}"));
        let (parent, child, member) = (at("r-auto-96"), at("r-stage-1 · STAGED behind r-auto-96 · fenced · 2 member(s)"), at("sp-s0"));
        assert!(parent < at("sp-m0") && at("sp-m3") < child && child < member, "{t:#?}");
        let indent = |i: usize| t[i].len() - t[i].trim_start().len();
        assert!(indent(member) > indent(child), "members sit under the child: {t:#?}");
    }

    #[test]
    fn a_running_pass_shows_its_progress_open_under_round_and_folds_when_done() {
        use crate::lcview::PassProgress;
        let mut v = busy_view();
        v.now = 10_000;
        v.progress = Some(PassProgress { round: "r-auto-96".into(), pass: 2, phase: "suites".into(), done: 226, total: 452, red: vec!["test-x.sh".into()], build_started: 9_000, suites_started: 9_400, cap: 900, updated_at: 9_995, ..Default::default() });
        let t = text(&layout(&v, &Ui::default(), 100, 200));
        let at = |s: &str| t.iter().position(|l| l.contains(s));
        assert!(has(&t, "pass 2") && has(&t, "226/452") && has(&t, "test-x.sh"), "{t:#?}");
        assert!(at("pass 2").unwrap() < at("sp-m0").unwrap(), "the pass comes first under ROUND");
        let all_collapsed = Ui { modes: [("round".to_string(), Mode::Collapsed)].into_iter().collect(), ..Default::default() };
        let pinned = text(&layout(&v, &all_collapsed, 100, 40));
        assert!(pinned[1].contains("⟳ r-auto-96 pass 2") && pinned[1].contains("226/452"), "pinned under the banner even with ROUND collapsed: {pinned:#?}");
        let pass_folded = Ui { modes: [("round/pass".to_string(), Mode::Collapsed)].into_iter().collect(), ..Default::default() };
        let t = text(&layout(&v, &pass_folded, 100, 200));
        let row = t.iter().find(|l| l.trim_start().starts_with("▸ pass 2")).expect("the pass row stays when collapsed");
        assert!(row.contains("226/452") && row.contains("▕"), "the bar is on the pass row itself, no expansion needed: {t:#?}");
        assert!(!has(&t, "test-x.sh"), "collapsing hides only the reds");
        v.progress.as_mut().unwrap().phase = "done".into();
        v.progress.as_mut().unwrap().verdict = "red".into();
        let t = text(&layout(&v, &Ui::default(), 100, 200));
        assert!(has(&t, "pass 2") && has(&t, "226/452") && !has(&t, "test-x.sh"), "a finished pass folds its reds and keeps its bar: {t:#?}");
        assert!(!t[1].contains("⟳"), "no pinned line once the pass is over");
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
    fn the_page_draws_every_node_of_the_panes_tree_in_order() {
        let v = busy_view();
        let page = render_page(&v, None, 5);
        let mut keys = Vec::new();
        fn walk(n: &[Node], out: &mut Vec<String>) {
            for x in n {
                out.push(x.key.clone());
                walk(&x.kids, out);
            }
        }
        walk(&tree(&v), &mut keys);
        assert!(keys.len() > 10, "a busy view has a real tree");
        let mut at = 0;
        for k in &keys {
            let needle = format!("data-key=\"{}\"", html_esc(k));
            let i = page[at..].find(&needle).unwrap_or_else(|| panic!("node {k} is missing from the page, or out of the pane's order"));
            at += i + needle.len();
        }
        let frag = render_fragment(&v, None);
        assert!(frag.contains("\"tree\"") && frag.contains("\"banner\""));
    }

    #[test]
    fn a_page_line_keeps_the_panes_styles_and_links_bead_ids() {
        let h = ansi_html(&format!("{RED}sp-ab1c red{R} and <x> sp-q2.3. not-sp-zz"));
        assert!(h.contains("<span class=red><a href=/stuck/sp-ab1c>sp-ab1c</a> red</span>"), "{h}");
        assert!(h.contains("&lt;x&gt;"), "text is escaped: {h}");
        assert!(h.contains("<a href=/stuck/sp-q2.3>sp-q2.3</a>."), "a trailing dot is not part of the id: {h}");
        assert!(!h.contains("/stuck/sp-zz"), "only a bead id at a word boundary is linked: {h}");
    }

    #[test]
    fn an_open_section_shows_its_open_line_and_a_folded_one_its_summary() {
        let n = Node::new("now/sp-w0", "head title").open_line("head").kids(vec![Node::new("now/sp-w0/title", "title")]);
        let mut h = String::new();
        node_html(&n, &mut h);
        assert!(h.contains("<span class=closed-line>head title</span><span class=open-line>head</span>"), "{h}");
        assert!(h.starts_with("<details data-key=\"now/sp-w0\" open>"), "AUTO-open nodes start open: {h}");
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
    fn an_expanded_aeon_puts_its_bead_title_first_and_whole() {
        let mut ui = Ui::default();
        ui.set("now", Mode::Open);
        ui.set("now/sp-w0", Mode::Open);
        let f = layout(&busy_view(), &ui, 200, 80);
        let row = |key: &str| f.rows.iter().find(|(_, k)| k == key).map(|(r, _)| strip(&f.lines[*r])).expect(key);
        assert!(!row("now/sp-w0").contains("title of sp-w0"), "the open aeon line drops the title");
        assert!(row("now/sp-w0/title").contains("title of sp-w0"));
        let (r0, _) = f.rows.iter().find(|(_, k)| k == "now/sp-w0").unwrap();
        assert_eq!(f.rows.iter().find(|(r, _)| r == &(r0 + 1)).map(|(_, k)| k.as_str()), Some("now/sp-w0/title"));
        ui.set("now/sp-w0", Mode::Collapsed);
        let f = layout(&busy_view(), &ui, 200, 80);
        let (r, _) = f.rows.iter().find(|(_, k)| k == "now/sp-w0").unwrap();
        assert!(strip(&f.lines[*r]).contains("title of sp-w0"), "a folded aeon keeps the title on its line");
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
