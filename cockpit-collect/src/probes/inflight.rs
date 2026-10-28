//! `inflight_keys` — every running unit of work in one place, for the pane's WORK IN FLIGHT
//! section and the status line's segment. Each kind is read from the component that owns it;
//! a kind that cannot be read is `?`, never zero, and the segment says so (`?rounds`).

use super::{push, Cfg, Kv};
use crate::io;
use crate::quoting::sanitize;
use std::path::Path;

const MAX_ROWS: usize = 12;
const KINDS: [(&str, &str); 3] = [("dwarf", "dwarves"), ("round", "rounds"), ("gate", "gates")];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub subject: String,
    pub detail: String,
    pub started: Option<i64>,
    pub progress: Option<(u64, Option<u64>)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum World {
    Running,
    Draining,
    Halted,
}

pub fn inflight_keys(_cfg: &Cfg) -> Kv {
    let run = io::run_dir();
    let sources = [dwarf_rows(&run), round_rows(), gate_rows(&run)];
    compose(&sources, world(&run))
}

fn world(run: &Path) -> Option<World> {
    if run.join("world.halted").exists() {
        Some(World::Halted)
    } else if run.join("world.draining").exists() {
        Some(World::Draining)
    } else if run.is_dir() {
        Some(World::Running)
    } else {
        None
    }
}

fn dwarf_rows(run: &Path) -> Option<Vec<Row>> {
    let mut rows = Vec::new();
    for e in std::fs::read_dir(run).ok()?.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix("dwarf-").and_then(|n| n.strip_suffix(".pid")) else { continue };
        if !strand::probe::dwarf_alive(&path) {
            continue;
        }
        let (persona, bead) = rest.split_once('-').unwrap_or(("?", rest));
        let started = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
        rows.push(Row { subject: bead.to_string(), detail: persona.to_string(), started, progress: None });
    }
    rows.sort_by(|a, b| a.subject.cmp(&b.subject));
    Some(rows)
}

fn round_rows() -> Option<Vec<Row>> {
    let batches = super::round::parse_batches(&super::round::list_batches()?)?;
    Some(
        batches
            .iter()
            .filter(|b| !super::round::terminal(&b.state) && b.state != "DISCARDED")
            .map(|b| Row {
                subject: format!("{}/{}", b.repo, b.id),
                detail: super::round::phase(&b.state).to_string(),
                started: Some(b.opened_at).filter(|t| *t > 0),
                progress: b.progress.as_ref().map(|p| (p.done as u64, p.total.map(|t| t as u64))),
            })
            .collect(),
    )
}

fn gate_rows(run: &Path) -> Option<Vec<Row>> {
    let (rows, _, _) = super::gate_run_scan(run);
    Some(
        rows.into_iter()
            .map(|(slug, age, phase, _)| Row { subject: slug, detail: phase, started: age.parse::<i64>().ok().map(|a| io::now() - a), progress: None })
            .collect(),
    )
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The compact status-line text. Same facts as the keys, composed once so the pane and the
/// status line cannot disagree about what is running.
pub fn segment(sources: &[Option<Vec<Row>>], world: Option<World>) -> String {
    let mut parts = Vec::new();
    for (i, (one, many)) in KINDS.iter().enumerate() {
        match &sources[i] {
            None => parts.push(format!("?{many}")),
            Some(rows) if rows.is_empty() => {}
            Some(rows) => {
                let mut s = plural(rows.len(), one, many);
                let prog: Vec<(u64, Option<u64>)> = rows.iter().filter_map(|r| r.progress).collect();
                if !prog.is_empty() {
                    let done: u64 = prog.iter().map(|p| p.0).sum();
                    let total = if prog.iter().all(|p| p.1.is_some()) { prog.iter().map(|p| p.1.unwrap_or(0)).sum::<u64>().to_string() } else { "?".to_string() };
                    s.push_str(&format!(" ({done}/{total})"));
                }
                parts.push(s);
            }
        }
    }
    let busy = parts.iter().any(|p| !p.starts_with('?'));
    let tail = match (world, busy) {
        (Some(World::Halted), _) => "halted",
        (Some(World::Draining), true) => "draining",
        (Some(World::Draining), false) => "drained",
        (None, _) => "?world",
        (Some(World::Running), true) => "",
        (Some(World::Running), false) if parts.is_empty() => "idle",
        (Some(World::Running), false) => "",
    };
    if !tail.is_empty() {
        parts.push(tail.to_string());
    }
    format!("\u{2699} {}", parts.join(" \u{b7} "))
}

pub fn compose(sources: &[Option<Vec<Row>>; 3], world: Option<World>) -> Kv {
    let mut out = Kv::new();
    let mut total = 0;
    for (i, (one, _)) in KINDS.iter().enumerate() {
        let key = format!("SP_INFLIGHT_{}_N", one.to_uppercase());
        match &sources[i] {
            Some(rows) => {
                push(&mut out, &key, rows.len().to_string());
                for r in rows {
                    if total >= MAX_ROWS {
                        break;
                    }
                    let (d, t) = r.progress.map_or((String::new(), String::new()), |(d, t)| (d.to_string(), t.map_or("?".to_string(), |t| t.to_string())));
                    let started = r.started.map_or("?".to_string(), |s| s.to_string());
                    push(&mut out, &format!("SP_INFLIGHT_ROW{total}"), format!("{one}|{}|{}|{started}|{d}|{t}", sanitize(&r.subject).replace('|', "/"), sanitize(&r.detail).replace('|', "/")));
                    total += 1;
                }
            }
            None => push(&mut out, &key, "?"),
        }
    }
    let w = match world {
        Some(World::Running) => "running",
        Some(World::Draining) => "draining",
        Some(World::Halted) => "halted",
        None => "?",
    };
    push(&mut out, "SP_INFLIGHT_WORLD", w);
    push(&mut out, "SP_INFLIGHT_SEG", segment(sources, world));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(s: &str, d: &str, p: Option<(u64, Option<u64>)>) -> Row {
        Row { subject: s.into(), detail: d.into(), started: Some(100), progress: p }
    }
    fn get<'a>(kv: &'a Kv, k: &str) -> &'a str {
        kv.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str()).unwrap_or("<absent>")
    }

    #[test]
    fn one_dwarf_one_round_one_gate_all_show_with_progress() {
        let src = [
            Some(vec![row("sp-1", "warden", None)]),
            Some(vec![row("spira/b1", "certifying", Some((412, Some(475))))]),
            Some(vec![row("spira-x", "pre-merge", None)]),
        ];
        let kv = compose(&src, Some(World::Running));
        assert_eq!(get(&kv, "SP_INFLIGHT_SEG"), "\u{2699} 1 dwarf \u{b7} 1 round (412/475) \u{b7} 1 gate");
        assert_eq!(get(&kv, "SP_INFLIGHT_ROUND_N"), "1");
        assert_eq!(get(&kv, "SP_INFLIGHT_ROW1"), "round|spira/b1|certifying|100|412|475");
    }

    #[test]
    fn everything_stopped_is_idle() {
        let src = [Some(vec![]), Some(vec![]), Some(vec![])];
        assert_eq!(get(&compose(&src, Some(World::Running)), "SP_INFLIGHT_SEG"), "\u{2699} idle");
    }

    #[test]
    fn an_unreadable_source_is_a_question_mark_never_idle() {
        let src = [Some(vec![]), None, Some(vec![])];
        let kv = compose(&src, Some(World::Running));
        assert_eq!(get(&kv, "SP_INFLIGHT_SEG"), "\u{2699} ?rounds");
        assert_eq!(get(&kv, "SP_INFLIGHT_ROUND_N"), "?");
        let all_dark = [None, None, None];
        assert!(!get(&compose(&all_dark, Some(World::Running)), "SP_INFLIGHT_SEG").contains("idle"));
        assert!(get(&compose(&src, None), "SP_INFLIGHT_SEG").contains("?world"));
    }

    #[test]
    fn drain_reads_draining_with_work_and_drained_without() {
        let busy = [Some(vec![row("sp-1", "g", None)]), Some(vec![]), Some(vec![])];
        assert_eq!(get(&compose(&busy, Some(World::Draining)), "SP_INFLIGHT_SEG"), "\u{2699} 1 dwarf \u{b7} draining");
        let idle = [Some(vec![]), Some(vec![]), Some(vec![])];
        assert_eq!(get(&compose(&idle, Some(World::Draining)), "SP_INFLIGHT_SEG"), "\u{2699} drained");
    }

    #[test]
    fn an_unknown_total_is_shown_as_unknown() {
        let src = [Some(vec![]), Some(vec![row("a", "certifying", Some((3, None)))]), Some(vec![])];
        assert!(get(&compose(&src, Some(World::Running)), "SP_INFLIGHT_SEG").contains("(3/?)"));
    }
}
