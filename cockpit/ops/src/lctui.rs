//! The ops pane as a navigable TUI (sp-5j35g5, per Ryan 2026-10-09). Every section always shows a
//! one-line header; its body is shown per a mode the operator picks: AUTO (sized by the space left),
//! OPEN (shown whole, first), or COLLAPSED (header only). The frame is cut to the pane exactly:
//! lines are truncated, never wrapped, and nothing scrolls off the top.
//!
//! In AUTO, a section gives up detail only as far as the pane forces it, in this order:
//! RECENT, HOLDS, REFUSED, BLOCKED and DRIFT shrink, then collapse; then STATE MACHINE drops its
//! empty transitions, then its empty states, then its remaining edges; then NEXT, REWORK and PIPE
//! shorten; then NOW drops its output lines. ROUND and NOW never collapse on their own. Anything
//! cut comes back on the next frame once there is room or it is no longer empty.
//!
//! Everything here is pure (view, modes, size in; lines out) so it is tested without a terminal.

use crate::lcview::{cut, View};
use std::collections::HashMap;

const B: &str = "\x1b[1m";
const D: &str = "\x1b[2m";
const R: &str = "\x1b[0m";
const RED: &str = "\x1b[31m";
const YEL: &str = "\x1b[33m";
const GRN: &str = "\x1b[32m";
const CYN: &str = "\x1b[36m";
const REV: &str = "\x1b[7m";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SecId {
    Round,
    Now,
    State,
    Pipe,
    Rework,
    Next,
    Blocked,
    Drift,
    Refused,
    Holds,
    Recent,
}

impl SecId {
    pub fn key(self) -> &'static str {
        match self {
            SecId::Round => "round",
            SecId::Now => "now",
            SecId::State => "state",
            SecId::Pipe => "pipe",
            SecId::Rework => "rework",
            SecId::Next => "next",
            SecId::Blocked => "blocked",
            SecId::Drift => "drift",
            SecId::Refused => "refused",
            SecId::Holds => "holds",
            SecId::Recent => "recent",
        }
    }
    pub fn from_key(k: &str) -> Option<SecId> {
        ORDER.iter().copied().find(|s| s.key() == k)
    }
}

/// Screen order (approved by Ryan, 2026-10-09).
pub const ORDER: [SecId; 11] = [
    SecId::Round,
    SecId::Now,
    SecId::State,
    SecId::Pipe,
    SecId::Rework,
    SecId::Next,
    SecId::Blocked,
    SecId::Drift,
    SecId::Refused,
    SecId::Holds,
    SecId::Recent,
];

/// The order AUTO sections give up space in, first to last.
const SHRINK: [SecId; 11] = [
    SecId::Recent,
    SecId::Holds,
    SecId::Refused,
    SecId::Blocked,
    SecId::Drift,
    SecId::State,
    SecId::Next,
    SecId::Rework,
    SecId::Pipe,
    SecId::Now,
    SecId::Round,
];

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

/// One section: a header and its body at decreasing levels of detail (`levels[0]` is everything).
/// `collapsible` is whether AUTO may reduce it to its header alone.
#[derive(Debug, Clone)]
pub struct Section {
    pub id: SecId,
    pub header: String,
    pub levels: Vec<Vec<String>>,
    pub collapsible: bool,
}

/// Interaction state the operator controls, kept across frames (and persisted by the binary).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Ui {
    pub modes: HashMap<String, Mode>,
    #[serde(skip)]
    pub selected: usize,
    #[serde(skip)]
    pub scroll: usize,
}

impl Ui {
    pub fn mode(&self, id: SecId) -> Mode {
        self.modes.get(id.key()).copied().unwrap_or_default()
    }
    pub fn cycle(&mut self, id: SecId) {
        let m = self.mode(id).next();
        if m == Mode::Auto {
            self.modes.remove(id.key());
        } else {
            self.modes.insert(id.key().into(), m);
        }
    }
}

/// A laid-out frame: the lines to draw, and which row holds which section's header (for clicks).
#[derive(Debug, Clone, Default)]
pub struct Frame {
    pub lines: Vec<String>,
    pub headers: Vec<(usize, usize)>,
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

/// Every section of the view, in screen order, with its detail levels. `w` is the pane width.
pub fn sections(v: &View, w: usize) -> Vec<Section> {
    let w = w.max(40);
    let mut out = Vec::new();

    // ROUND: name, pass, age, state; each live member with how often it has been ejected before.
    let (header, body) = match &v.round {
        None => (format!("{B}ROUND{R}  {D}none open{R}"), Vec::new()),
        Some(r) => {
            let mut b: Vec<String> = r
                .members
                .iter()
                .map(|m| {
                    let ej = if m.ejects > 0 { format!("{YEL}ejected {}× before{R} ", m.ejects) } else { String::new() };
                    format!("  {:<14} {} {ej}{}", m.id, m.prio, m.title)
                })
                .collect();
            if !r.ejected.is_empty() {
                b.push(format!("  {D}ejected this round: {}{R}", r.ejected.join(" ")));
            }
            (
                format!(
                    "{B}ROUND{R}  {B}{}{R} · pass {B}{}{R} · {} · {} · {} member(s)",
                    r.name,
                    r.passes,
                    r.age,
                    r.state,
                    r.members.len()
                ),
                b,
            )
        }
    };
    out.push(Section { id: SecId::Round, header, levels: vec![body], collapsible: false });

    // NOW: the working aeons. Full: holder line, persona, output tail. Compact: holder line only.
    let mut full = Vec::new();
    let mut compact = Vec::new();
    for i in &v.now_items {
        let tag = if i.rework { format!("{YEL}{B}REWORK{R} ") } else { String::new() };
        let name = i.who.split('@').next().unwrap_or("");
        let who = match (i.persona.as_str(), name) {
            ("—" | "", n) => n.to_string(),
            (p, "") => p.to_string(),
            (p, n) => format!("{p}/{n}"),
        };
        let line = format!("  {CYN}{:<14}{R} {:<12} {tag}{} {D}{} · {}{R} {}", cut(&who, 14), i.id, i.prio, i.age, i.note, i.title);
        compact.push(line.clone());
        full.push(line);
        let c = match i.out_level.as_str() {
            "bad" => RED,
            "warn" => YEL,
            _ => D,
        };
        let age = if i.out_age.is_empty() { String::new() } else { format!(" {} ago", i.out_age) };
        for (k, l) in i.out.iter().enumerate() {
            let tail = if k + 1 == i.out.len() { age.as_str() } else { "" };
            full.push(format!("      {c}{}{tail}{R}", cut(l, w.saturating_sub(8 + tail.chars().count()))));
        }
    }
    if v.now_items.is_empty() {
        full.push(format!("  {D}nothing is being worked{R}"));
        compact = full.clone();
    }
    out.push(Section {
        id: SecId::Now,
        header: format!("{B}NOW{R}    {B}{}{R} working {D}of {} — holder · bead · lease{R}", v.now_items.len(), v.ceiling),
        levels: vec![full, compact],
        collapsible: false,
    });

    // STATE MACHINE: full; without empty transitions; without empty states too; states only.
    let state_level = |drop_empty_edges: bool, drop_empty_states: bool, edges: bool| -> Vec<String> {
        let mut l = Vec::new();
        for st in &v.machine {
            let empty = st.count == 0 && !st.red && !st.no_rework_exit;
            if drop_empty_states && empty {
                continue;
            }
            let c = if st.name == "REWORK" { YEL } else { "" };
            let detail = if st.detail.is_empty() { String::new() } else { format!("  {}", cut(&st.detail, w.saturating_sub(22))) };
            l.push(format!("{c}{B}{:<14}{R}{B}{:>4}{R}{detail}{}", st.name, st.count, dot(st.red)));
            if !edges {
                continue;
            }
            for e in &st.edges {
                if drop_empty_edges && e.rate_1h == 0 {
                    continue;
                }
                let glyph = if e.main { "│" } else { "└▶" };
                let label = if e.main { e.event.clone() } else { format!("{} ▶ {}", e.event, e.to) };
                l.push(format!("  {D}{glyph}{R} {:<27}{:>4}/h", cut(&label, 27), e.rate_1h));
            }
            if st.no_rework_exit {
                l.push(format!("  {RED}└▶ (no exit to REWORK){R}  {RED}●{R}"));
            }
        }
        l.push(format!("{D}terminal 24h  {}{R}", v.terminal));
        l
    };
    out.push(Section {
        id: SecId::State,
        header: format!("{B}STATE MACHINE{R}  {D}counts now · edge rates per hour{R}"),
        levels: vec![state_level(false, false, true), state_level(true, false, true), state_level(true, true, true), state_level(true, true, false)],
        collapsible: false,
    });

    // PIPE
    let pipe_line = |n: usize| -> Vec<String> {
        v.pipe
            .iter()
            .map(|p| {
                let ids = p.items.iter().take(n).map(|i| format!("{} {D}{}{R}", i.id, i.age)).collect::<Vec<_>>().join("  ");
                format!("  {:<11} {B}{:>3}{R} {D}oldest {}{R}  {ids}", p.state, p.count, p.oldest)
            })
            .collect()
    };
    let pipe_sum = v.pipe.iter().map(|p| format!("{} {}", p.state, p.count)).collect::<Vec<_>>().join(" · ");
    out.push(Section {
        id: SecId::Pipe,
        header: format!("{B}PIPE{R}   {}", if pipe_sum.is_empty() { format!("{D}empty{R}") } else { pipe_sum }),
        levels: vec![pipe_line(6), pipe_line(2), pipe_line(0)],
        collapsible: true,
    });

    // REWORK, NEXT, BLOCKED: lists that shorten, with "+N more".
    let list = |items: Vec<String>, caps: &[usize]| -> Vec<Vec<String>> {
        caps.iter()
            .map(|&n| {
                let mut l: Vec<String> = items.iter().take(n).cloned().collect();
                if items.len() > n {
                    l.push(format!("  {D}… {} more{R}", items.len() - n));
                }
                l
            })
            .collect()
    };
    let rework: Vec<String> =
        v.rework_items.iter().map(|i| format!("  {YEL}{:<12}{R} {} {D}{}{R} {}", i.id, i.prio, i.age, cut(&i.note, w.saturating_sub(30)))).collect();
    out.push(Section {
        id: SecId::Rework,
        header: format!("{B}REWORK{R} {B}{}{R} {D}sent back — bead · why{R}", v.rework_items.len()),
        levels: list(rework, &[usize::MAX, 3, 1]),
        collapsible: true,
    });
    let next: Vec<String> = v.next.iter().map(|i| format!("  {} {:<12} {}", i.prio, i.id, cut(&i.title, w.saturating_sub(20)))).collect();
    out.push(Section {
        id: SecId::Next,
        header: format!("{B}NEXT{R}   {B}{}{R} claimable {D}(express, REWORK, then priority){R}", v.next_count),
        levels: list(next, &[usize::MAX, 5, 2]),
        collapsible: true,
    });
    let blocked: Vec<String> = v.blocked.iter().map(|i| format!("  {} {:<26} {}", i.prio, i.note, cut(&i.title, w.saturating_sub(36)))).collect();
    out.push(Section {
        id: SecId::Blocked,
        header: format!("{B}BLOCKED{R} {B}{}{R} {D}READY, waiting on a dependency{R}", v.blocked.len()),
        levels: list(blocked, &[usize::MAX, 3]),
        collapsible: true,
    });
    let drift: Vec<String> = v.drift.iter().map(|d| format!("  {RED}{d}{R}")).collect();
    out.push(Section {
        id: SecId::Drift,
        header: if v.drift.is_empty() {
            format!("{B}DRIFT{R}  {D}none{R}")
        } else {
            format!("{RED}{B}DRIFT{R} {RED}{} READY/REWORK bead(s) whose commit is already on {}{R}", v.drift.len(), v.base)
        },
        levels: list(drift, &[usize::MAX, 3]),
        collapsible: true,
    });

    // REFUSED, HOLDS, RECENT: lowest priority; they go first.
    let refused: Vec<String> = v.refused.iter().map(|r| format!("  {:<24}{B}{:>4}{R}  {}{}", cut(&r.what, 24), r.n, cut(&r.why, w.saturating_sub(34)), dot(r.red))).collect();
    out.push(Section {
        id: SecId::Refused,
        header: format!("{B}REFUSED{R} {D}(1h){R} {B}{}{R}", v.refused.iter().map(|r| r.n).sum::<i64>()),
        levels: list(refused, &[usize::MAX, 2]),
        collapsible: true,
    });
    let holds: Vec<String> = v
        .holds
        .iter()
        .map(|g| {
            let c = if g.kind == "poison" { RED } else { YEL };
            format!("  {c}{:<9}{R}{B}{:>4}{R}  {}", g.kind, g.count, g.top.iter().take(2).map(|(n, r)| format!("{n}× {r}")).collect::<Vec<_>>().join(" · "))
        })
        .collect();
    let hold_sum = v.holds.iter().map(|g| format!("{} {}", g.kind, g.count)).collect::<Vec<_>>().join(" · ");
    out.push(Section {
        id: SecId::Holds,
        header: format!("{B}HOLDS{R}  {}", if hold_sum.is_empty() { format!("{D}none{R}") } else { hold_sum }),
        levels: vec![holds],
        collapsible: true,
    });
    let recent: Vec<String> = v
        .recent
        .iter()
        .map(|i| {
            let c = match i.state.as_str() {
                "LANDED" => GRN,
                "REWORK" | "DROPPED" => YEL,
                _ => "",
            };
            format!("  {D}{:>4}{R} {:<12} {c}{:<11}{R} {}", i.age, i.id, i.state, cut(&i.title, w.saturating_sub(34)))
        })
        .collect();
    out.push(Section {
        id: SecId::Recent,
        header: format!("{B}RECENT{R} {D}last transitions{R}"),
        levels: list(recent, &[usize::MAX, 3]),
        collapsible: true,
    });
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

/// Lay the sections out into exactly `h` rows of at most `w` columns.
pub fn layout(v: &View, ui: &Ui, w: usize, h: usize) -> Frame {
    let secs = sections(v, w);
    let top = banner(v);
    // Per section: Some(level) shows that level's body; None shows the header alone.
    let mut lvl: Vec<Option<usize>> = secs
        .iter()
        .map(|s| match ui.mode(s.id) {
            Mode::Collapsed => None,
            _ => Some(0),
        })
        .collect();
    let height = |lvl: &[Option<usize>]| -> usize {
        top.len() + secs.iter().zip(lvl).map(|(s, l)| 1 + l.map(|i| s.levels.get(i).map_or(0, Vec::len)).unwrap_or(0)).sum::<usize>()
    };
    'shrink: for id in SHRINK {
        let Some(i) = secs.iter().position(|s| s.id == id) else { continue };
        if ui.mode(id) != Mode::Auto {
            continue;
        }
        while height(&lvl) > h {
            match lvl[i] {
                Some(l) if l + 1 < secs[i].levels.len() => lvl[i] = Some(l + 1),
                Some(_) if secs[i].collapsible => lvl[i] = None,
                _ => continue 'shrink,
            }
        }
        if height(&lvl) <= h {
            break;
        }
    }

    let sel = ui.selected.min(secs.len().saturating_sub(1));
    let mut lines = top.clone();
    let mut headers = Vec::new();
    let mut sel_row = 0;
    for (k, (s, l)) in secs.iter().zip(&lvl).enumerate() {
        let body = l.and_then(|i| s.levels.get(i)).cloned().unwrap_or_default();
        let full_len = s.levels.first().map_or(0, Vec::len);
        let marker = match (ui.mode(s.id), l) {
            (Mode::Collapsed, _) => "▸",
            (Mode::Open, _) => "▼",
            (Mode::Auto, None) => "▹",
            (Mode::Auto, Some(_)) => "▿",
        };
        let hidden = full_len.saturating_sub(body.len());
        let note = if l.is_some() && hidden > 0 { format!(" {D}(+{hidden} hidden){R}") } else { String::new() };
        let head = format!("{marker} {}{note}", s.header);
        if k == sel {
            sel_row = lines.len();
            lines.push(format!("{REV}{}", clip(&head, w)));
        } else {
            lines.push(head);
        }
        headers.push((lines.len() - 1, k));
        lines.extend(body);
    }
    // Too tall even after shrinking (OPEN sections are never cut): window around the selection.
    let start = if lines.len() > h {
        let max_start = lines.len() - h;
        let want = sel_row.saturating_sub(top.len()).saturating_add(ui.scroll);
        want.min(max_start)
    } else {
        0
    };
    let mut out: Vec<String> = lines.iter().skip(start).take(h).map(|l| clip(l, w)).collect();
    if start > 0 && !out.is_empty() {
        out[0] = clip(&format!("{D}↑ {start} more above (k / PgUp){R}"), w);
    }
    let headers = headers.into_iter().filter(|(r, _)| *r >= start && *r < start + h).map(|(r, k)| (r - start, k)).collect();
    Frame { lines: out, headers }
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
            edges: rates.iter().map(|&r| SmEdge { event: "ev".into(), to: "X".into(), rate_1h: r, main: false }).collect(),
            ..Default::default()
        }
    }

    fn busy_view() -> View {
        let item = |id: &str| Item { id: id.into(), prio: "P1".into(), title: format!("title of {id}"), who: "anima".into(), age: "5m".into(), persona: "builder".into(), out: vec!["▸ cargo test".into(), "▸ git commit".into()], ..Default::default() };
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
            machine: vec![st("OPEN", 4, &[0]), st("READY", 10, &[3, 0]), st("WORKING", 2, &[6, 0, 0]), st("SUBMITTED", 0, &[0, 0]), st("CERTIFIED", 0, &[0]), st("IN_DELIVERY", 4, &[6, 1]), st("LANDED", 106, &[]), st("REWORK", 1, &[5])],
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

    #[test]
    fn a_70x50_pane_shows_round_and_every_aeon_and_never_wraps() {
        let f = layout(&busy_view(), &Ui::default(), 70, 50);
        let t = text(&f);
        assert!(t.len() <= 50, "{} rows", t.len());
        assert!(t.iter().all(|l| l.chars().count() <= 70), "{t:#?}");
        assert!(t.iter().any(|l| l.contains("ROUND") && l.contains("r-auto-96") && l.contains("pass 2")), "{t:#?}");
        for k in 0..3 {
            assert!(t.iter().any(|l| l.contains(&format!("sp-w{k}"))), "aeon sp-w{k} missing: {t:#?}");
        }
        assert!(t.iter().any(|l| l.contains("STATE MACHINE")));
    }

    #[test]
    fn a_120x81_pane_fits_and_keeps_more_than_a_70x50_one() {
        let v = busy_view();
        let big = text(&layout(&v, &Ui::default(), 120, 81));
        let small = text(&layout(&v, &Ui::default(), 70, 50));
        assert!(big.len() <= 81 && big.iter().all(|l| l.chars().count() <= 120), "{big:#?}");
        assert!(big.len() > small.len(), "the taller pane shows more: {} vs {}", big.len(), small.len());
        assert!(big.iter().any(|l| l.starts_with("SUBMITTED")), "room for empty states at 81 rows: {big:#?}");
    }

    #[test]
    fn a_pane_shorter_than_the_essentials_still_fits_exactly() {
        let t = text(&layout(&busy_view(), &Ui::default(), 70, 12));
        assert_eq!(t.len(), 12, "{t:#?}");
        assert!(t[0].contains("LIFECYCLE"));
    }

    #[test]
    fn the_state_machine_drops_empty_edges_then_empty_states() {
        let v = busy_view();
        let secs = sections(&v, 80);
        let sm = secs.iter().find(|s| s.id == SecId::State).unwrap();
        let l0: Vec<String> = sm.levels[0].iter().map(|l| strip(l)).collect();
        let l1: Vec<String> = sm.levels[1].iter().map(|l| strip(l)).collect();
        let l2: Vec<String> = sm.levels[2].iter().map(|l| strip(l)).collect();
        assert!(l0.iter().any(|l| l.contains("0/h")));
        assert!(!l1.iter().any(|l| l.contains("   0/h")), "{l1:#?}");
        assert!(l1.iter().any(|l| l.starts_with("SUBMITTED")), "empty states stay at level 1");
        assert!(!l2.iter().any(|l| l.starts_with("SUBMITTED") || l.starts_with("CERTIFIED")), "{l2:#?}");
    }

    #[test]
    fn an_elided_state_comes_back_once_it_is_not_empty() {
        let mut v = busy_view();
        let f = layout(&v, &Ui::default(), 70, 30);
        assert!(!text(&f).iter().any(|l| l.starts_with("SUBMITTED")), "elided while empty on a short pane");
        v.machine.iter_mut().find(|s| s.name == "SUBMITTED").unwrap().count = 3;
        let f = layout(&v, &Ui::default(), 70, 30);
        assert!(text(&f).iter().any(|l| l.starts_with("SUBMITTED")), "{:#?}", text(&f));
    }

    #[test]
    fn low_priority_sections_give_way_first_and_a_tall_pane_shows_everything() {
        let v = busy_view();
        let short = text(&layout(&v, &Ui::default(), 70, 40));
        assert!(!short.iter().any(|l| l.contains("sp-r5")), "RECENT body is the first to go");
        let tall = text(&layout(&v, &Ui::default(), 120, 200));
        assert!(tall.iter().any(|l| l.contains("sp-r9")) && tall.iter().any(|l| l.contains("sp-n11")), "{tall:#?}");
    }

    #[test]
    fn collapse_and_open_are_the_operators_choice() {
        let v = busy_view();
        let mut ui = Ui::default();
        ui.cycle(SecId::Recent); // Auto -> Open
        let f = text(&layout(&v, &ui, 70, 50));
        assert!(f.iter().any(|l| l.contains("sp-r9")), "an OPEN section is shown whole: {f:#?}");
        ui.cycle(SecId::Now); // Open
        ui.cycle(SecId::Now); // Collapsed
        let f = text(&layout(&v, &ui, 70, 50));
        assert!(!f.iter().any(|l| l.contains("sp-w0")), "a COLLAPSED section shows its header only");
        assert!(f.iter().any(|l| l.contains("NOW")));
    }

    #[test]
    fn every_header_row_is_reported_for_clicks() {
        let f = layout(&busy_view(), &Ui::default(), 70, 50);
        let t = text(&f);
        assert!(!f.headers.is_empty());
        for (row, k) in &f.headers {
            assert!(t[*row].contains(strip(&sections(&busy_view(), 70)[*k].header).split_whitespace().next().unwrap()));
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
