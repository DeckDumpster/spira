//! The Aeons page: what the ops pane shows under NOW, from the same snapshot functions.

use crate::shell::shell;
use crate::stuck::esc;
use cockpit_ops::health::model::Snapshot;
use cockpit_ops::health::sections::{aeon_rows, slots_line, AeonRow};
use serde_json::{json, Value};

pub fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            for n in it.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out.trim().to_string()
}

pub fn slots(snap: &Snapshot) -> String {
    strip_ansi(&slots_line(snap))
}

pub fn rows(snap: &Snapshot, working: &[(String, String)]) -> Vec<AeonRow> {
    aeon_rows(snap, working)
}

pub fn render_json(snap: &Snapshot, working: &[(String, String)]) -> Value {
    let list: Vec<Value> = rows(snap, working)
        .into_iter()
        .map(|r| {
            json!({
                "name": r.name, "persona": r.faith, "model": r.model, "bead": r.bead, "title": r.title,
                "priority": r.priority, "repo": r.repo, "phase": r.phase, "minutes": r.minutes,
                "turns": r.turns, "lease_minutes": r.lease, "quiet_s": r.quiet,
                "trace": if r.trail.is_empty() { vec![r.act.clone()].into_iter().filter(|a| !a.is_empty()).collect() } else { r.trail },
            })
        })
        .collect();
    json!({ "slots": slots(snap), "aeons": list })
}

fn card(r: &AeonRow) -> String {
    let lease = if r.lease == "?" { "lease ?".to_string() } else { format!("lease {}m", esc(&r.lease)) };
    let trace: Vec<&String> = if r.trail.is_empty() { Some(&r.act).filter(|a| !a.is_empty()).into_iter().collect() } else { r.trail.iter().collect() };
    let lines: String = trace.iter().map(|l| format!("<div class=dim>↳ {}</div>", esc(l))).collect();
    format!(
        "<div class=card><div class=head><span class=id>{name}</span><span class=dim>the {faith} on {model}</span></div>\
<div class=head><a class=id href=\"/beads/{bead}\">{bead}</a><span class=t>{title}</span></div>\
<div class=dim>{repo} · P{pri} · {phase} · {min}m · {turns} turns · {lease} · quiet {quiet}</div>{lines}</div>",
        name = esc(&r.name),
        faith = esc(&r.faith),
        model = esc(&r.model),
        bead = esc(&r.bead),
        title = esc(&r.title),
        repo = esc(&r.repo),
        pri = esc(&r.priority),
        phase = esc(&r.phase),
        min = esc(&r.minutes),
        turns = esc(&r.turns),
        quiet = esc(&r.quiet),
    )
}

pub fn render_page(snap: &Snapshot, working: &[(String, String)], note: Option<&str>) -> String {
    let list = rows(snap, working);
    let mut b = format!("<p>{}</p>", esc(&slots(snap)));
    if let Some(n) = note {
        b.push_str(&format!("<p class=\"badge red\">{}</p>", esc(n)));
    }
    if list.is_empty() {
        b.push_str("<p class=dim>no aeon is live</p>");
    }
    for r in &list {
        b.push_str(&card(r));
    }
    shell("aeons", "Aeons", 15, &format!("<header><h1>Aeons <span class=dim>— {} live</span></h1></header><main>{b}</main>", list.len()))
}
