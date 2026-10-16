//! `/states`: the ops pane's STATES section as a page — every lifecycle state with its count,
//! edge rates and beads. Built from the same `lcview::View` the pane draws.

use crate::shell::shell;
use crate::stuck::esc;
use cockpit_ops::lcview::{SmState, View};
use serde_json::{json, Value};

const CSS: &str = "<style>details.st{border-bottom:1px solid #1d232b}details.st>summary{display:flex;gap:.6rem;align-items:baseline;flex-wrap:wrap;padding:.5rem .8rem;cursor:pointer}\
.st .n{font-weight:700;min-width:2.5rem;text-align:right}.edge{display:flex;justify-content:space-between;gap:.5rem;padding:.1rem .8rem .1rem 1.6rem}\
.bead{display:block;padding:.35rem .8rem .35rem 1.6rem;border-top:1px solid #1d232b;overflow-wrap:anywhere}.bead .meta{font-size:.75rem}</style>";

fn edge_json(e: &cockpit_ops::lcview::SmEdge) -> Value {
    json!({ "event": e.event, "to": e.to, "rate_1h": e.rate_1h, "main": e.main })
}

fn bead_json(i: &cockpit_ops::lcview::Item) -> Value {
    json!({ "id": i.id, "title": i.title, "repo": i.repo, "age": i.age, "holds": i.holds, "prio": i.prio })
}

pub fn api_json(v: &View, age_s: i64) -> Value {
    let states: Vec<Value> = v
        .machine
        .iter()
        .map(|st| {
            json!({
                "name": st.name, "count": st.count, "detail": st.detail, "red": st.red, "no_rework_exit": st.no_rework_exit,
                "edges": st.edges.iter().map(edge_json).collect::<Vec<_>>(),
                "beads": st.beads.iter().map(bead_json).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({ "age_s": age_s, "terminal": v.terminal, "states": states })
}

fn state_html(st: &SmState) -> String {
    let red = if st.red { " <span class=red>●</span>" } else { "" };
    let detail = if st.detail.is_empty() { String::new() } else { format!(" <span class=dim>{}</span>", esc(&st.detail)) };
    let mut h = format!("<details class=st><summary><span class=\"badge s-{n}\">{n}</span><span class=n>{c}</span>{detail}{red}</summary>", n = esc(&st.name), c = st.count);
    for e in &st.edges {
        let label = if e.main { esc(&e.event) } else { format!("{} ▶ {}", esc(&e.event), esc(&e.to)) };
        h.push_str(&format!("<div class=edge><span class=dim>{label}</span><span>{}/h</span></div>", e.rate_1h));
    }
    if st.no_rework_exit {
        h.push_str("<div class=edge><span class=red>no exit to REWORK ●</span></div>");
    }
    for i in &st.beads {
        let holds = if i.holds.is_empty() { String::new() } else { format!(" · <span class=red>{}</span>", esc(&i.holds.join(", "))) };
        h.push_str(&format!(
            "<a class=bead href=\"/beads/{id}\"><span class=id>{id}</span> {prio} {title}<div class=\"meta dim\">{repo} · {age} in state{holds}</div></a>",
            id = esc(&i.id),
            prio = esc(&i.prio),
            title = esc(&i.title),
            repo = if i.repo.is_empty() { "?".to_string() } else { esc(&i.repo) },
            age = esc(&i.age),
        ));
    }
    h.push_str("</details>");
    h
}

pub fn render_page(v: &View) -> String {
    let mut b = format!("{CSS}<header><h1>States <span class=dim>— counts now · moves per hour</span></h1></header><main>");
    for st in &v.machine {
        b.push_str(&state_html(st));
    }
    b.push_str(&format!("<p class=dim>terminal 24h {}</p></main>", esc(&v.terminal)));
    shell("states", "States", 30, &b)
}
