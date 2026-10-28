//! The web-native site (brain design `loom-web-native-2026-10-26`, approved 2026-10-26): one
//! header, each fact once per screen, nothing the lifecycle page showed left without a home.
//! Served at the root since the switch (per Ryan 2026-10-28).
//!
//! Everything here is a pure function of the lifecycle view, the in-flight read and the held
//! units snapshot; the routes do the reading.

use crate::now::UnitsState;
use crate::stuck::{dur, esc};
use crate::units;
use cockpit_ops::health::inflight::Flight;
use cockpit_ops::lcview::{self, Item, RepoRound, Snapshot, View};

pub const BASE: &str = "";

/// Every top-level field of `View`, and where the site draws it. The coverage test fails when
/// `View` grows a field that is not listed here, so nothing the model knows goes undrawn.
pub const VIEW_HOMES: &[(&str, &str)] = &[
    ("clock", "header (age, gather ms) and the More list (clock)"),
    ("release", "header"),
    ("world", "header"),
    ("world_running", "header"),
    ("working", "dwarves heading (working beads)"),
    ("ceiling", "header and dwarves heading"),
    ("errors", "more › sources failing"),
    ("gather_ms", "header (age, gather ms) and the More list (clock)"),
    ("failures", "header chip"),
    ("flow", "states › flow"),
    ("ready", "states › flow"),
    ("ready_held", "states › flow"),
    ("held_pct", "states › flow"),
    ("rework", "states › flow"),
    ("dropped_24h", "states › flow"),
    ("superseded_24h", "states › flow"),
    ("landed_total", "states › flow"),
    ("holds", "more › holds"),
    ("base", "more › drift"),
    ("drift", "more › drift"),
    ("now_items", "dwarves"),
    ("pipe", "states › flow"),
    ("rework_items", "states › flow"),
    ("next_count", "states › flow"),
    ("next_unknown", "states › flow"),
    ("next", "states › flow"),
    ("blocked", "more › blocked"),
    ("recent", "more › recent"),
    ("machine", "states"),
    ("terminal", "states › flow"),
    ("refused", "more › refused"),
    ("batch", "rounds › batches"),
    ("round", "rounds (when no repo round is live)"),
    ("rounds", "rounds"),
    ("vm_runs", "rounds › VM pool"),
    ("progress", "rounds (the one progress bar)"),
    ("now", "header age"),
    ("decide", "more › questions for you"),
    ("decide_error", "more › questions for you"),
    ("menders", "rounds › menders"),
    ("epics", "more › epics"),
    ("inflight", "rounds › in flight"),
];

pub struct Page<'a> {
    pub snap: &'a Snapshot,
    pub view: &'a View,
    pub flight: &'a Flight,
    pub units: &'a UnitsState<'a>,
    pub now: i64,
    /// Seconds since the pane wrote the snapshot.
    pub age_s: i64,
}

const CSS: &str = ":root{--bg:#0f1318;--row:#151a21;--line:#232a33;--fg:#d8dee9;--dim:#7d8896;--red:#ff6b6d;--redbg:#3a1a1c;--amber:#e0a43a;--green:#4cc38a;--acc:#5aa2e6;--bar:#2a313b}\
*{box-sizing:border-box}html{-webkit-text-size-adjust:100%}body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.3 system-ui,-apple-system,sans-serif;overflow-wrap:anywhere}\
a{color:inherit;text-decoration:none}.id{font-weight:600;font-variant-numeric:tabular-nums;color:var(--acc)}\
header.top{position:sticky;top:0;z-index:5;display:flex;flex-wrap:wrap;gap:.1rem .6rem;align-items:center;padding:.35rem .75rem;background:var(--row);border-bottom:1px solid var(--line);font-size:.82rem}\
nav.tabs{display:flex;gap:.9rem;padding:.3rem .75rem;font-size:.8rem;color:var(--dim);border-bottom:1px solid var(--line)}nav.tabs a.on{color:var(--fg);font-weight:600}\
.chip{display:inline-block;padding:0 .4rem;border-radius:999px;font-size:.75rem;font-weight:600;background:var(--bar)}\
.chip.ok{color:var(--green)}.chip.warn{color:var(--amber)}.chip.bad{color:var(--red);background:var(--redbg)}\
main{padding:0}\
h2{margin:0;padding:.45rem .75rem .2rem;font-size:.72rem;letter-spacing:.06em;text-transform:uppercase;color:var(--dim);font-weight:600}\
details>summary{list-style:none;cursor:pointer}details>summary::-webkit-details-marker{display:none}\
.line{display:flex;flex-wrap:wrap;gap:.1rem .5rem;align-items:baseline;padding:.4rem .75rem;border-bottom:1px solid var(--line)}\
summary.line{flex-wrap:nowrap}summary.line>*{flex:0 0 auto}summary.line>.small,summary.line>.t{flex:1 1 auto;min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}\
details>summary.line:after{content:'›';margin-left:auto;padding-left:.3rem;color:var(--dim)}details[open]>summary.line:after{content:'⌄'}\
.line .n{font-weight:700;font-variant-numeric:tabular-nums;min-width:2.2ch;text-align:right}.line .k{font-weight:600;min-width:7.5em}\
.body{background:var(--row);border-bottom:1px solid var(--line)}\
.row{display:flex;flex-wrap:wrap;gap:0 .5rem;align-items:baseline;padding:.35rem .75rem .35rem 1.25rem;border-top:1px solid var(--line)}.row:first-child{border-top:0}\
.row .t{flex:1 1 100%;min-width:0}.dim{color:var(--dim)}.red{color:var(--red)}.amber{color:var(--amber)}.green{color:var(--green)}.small{font-size:.8rem}.s{font-size:.8rem;color:var(--dim)}\
.bar{display:inline-block;width:5.5rem;height:6px;border-radius:3px;background:var(--bar);overflow:hidden;vertical-align:middle}.bar i{display:block;height:100%;background:var(--acc)}.bar i.red{background:var(--red)}\
pre{white-space:pre-wrap;font-size:.75rem;margin:.2rem 0;color:var(--dim)}\
.badge{display:inline-block;font-size:.72rem;padding:0 .35rem;border-radius:4px;background:var(--bar)}.badge.red{background:var(--redbg);color:var(--red)}";

/// Fragment refresh plus remembered open/closed state. Storage can be blocked; every access is
/// guarded so the page still renders with each disclosure in its default state.
const JS: &str = "(function(){var K='loom-next-open';function load(){try{return JSON.parse(localStorage.getItem(K)||'{}')}catch(e){return{}}}\
function save(m){try{localStorage.setItem(K,JSON.stringify(m))}catch(e){}}\
function apply(){var m=load();document.querySelectorAll('details[data-k]').forEach(function(d){var k=d.getAttribute('data-k');if(k in m)d.open=m[k];d.addEventListener('toggle',function(){var n=load();n[k]=d.open;save(n)})})}\
function tick(){fetch(location.pathname+'?fragment=1',{cache:'no-store'}).then(function(r){return r.json()}).then(function(f){document.getElementById('head').outerHTML=f.head;document.getElementById('main').innerHTML=f.main;apply()}).catch(function(){var h=document.getElementById('head');if(h&&!h.querySelector('.offline'))h.insertAdjacentHTML('beforeend',\"<span class='chip bad offline'>offline</span>\")})}\
apply();setInterval(tick,5000)})();";

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

fn bead(id: &str) -> String {
    format!("<a class=id href=\"/beads/{}\">{}</a>", esc(id), esc(id))
}

/// Bead ids in free text become links; everything else is escaped.
fn linked(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("sp-") {
        out.push_str(&esc(&rest[..i]));
        let tail = &rest[i..];
        let end = tail[3..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '.')).map_or(tail.len(), |e| e + 3);
        let id = tail[..end].trim_end_matches('.');
        if id.len() > 3 {
            out.push_str(&bead(id));
            rest = &tail[id.len()..];
        } else {
            out.push_str("sp-");
            rest = &tail[3..];
        }
    }
    out.push_str(&esc(rest));
    out
}

/// A disclosure that is one line closed and its rows open. `key` makes its state remembered.
fn fold(key: &str, open: bool, summary: &str, body: &str) -> String {
    format!(
        "<details data-k=\"{key}\"{}><summary class=line>{summary}</summary><div class=body>{body}</div></details>",
        if open { " open" } else { "" }
    )
}

fn plain(summary: &str) -> String {
    format!("<div class=line>{summary}</div>")
}

fn progress_bar(done: u64, total: u64, red: bool) -> String {
    let pct = (done.min(total) * 100).checked_div(total).unwrap_or(0);
    format!("<span class=bar><i{} style=\"width:{pct}%\"></i></span> <span class=small>{done}/{total}</span>", if red { " class=red" } else { "" })
}

pub fn world_chip(v: &View) -> String {
    let w = v.world.to_ascii_uppercase();
    let (class, label) = if w.contains("HALT") || w.contains("STOP") {
        ("bad", "HALTED")
    } else if w.contains("DRAIN") {
        ("warn", "DRAINING")
    } else if v.world_running {
        ("ok", "RUNNING")
    } else {
        ("bad", "NOT RUNNING")
    };
    format!("<span class=\"chip {class}\" title=\"{}\">● {label}</span>", esc(&v.world))
}

/// One line: world, release, dwarves, data age, and the pane's own diagnostics in grey. Red chips
/// appear only when something is wrong.
pub fn header(p: &Page) -> String {
    let v = p.view;
    let mut chips = String::new();
    if p.age_s > crate::LIFECYCLE_STALE_S {
        chips.push_str(&format!("<span class=\"chip bad\">stale {}</span>", dur(p.age_s)));
    }
    if v.failures > 0 {
        chips.push_str(&format!("<span class=\"chip bad\">{} source failure{}</span>", v.failures, if v.failures == 1 { "" } else { "s" }));
    }
    format!(
        "<header class=top id=head>{}<span class=dim title=\"{}\">{}</span><span>dwarves {}/{}</span><span class=dim>{} · {} ms</span>{chips}</header>",
        world_chip(v),
        esc(&v.release),
        esc(short(&v.release)),
        crate::now::dwarves(p.snap),
        v.ceiling,
        if p.age_s <= 1 { "now".to_string() } else { format!("{} ago", dur(p.age_s)) },
        v.gather_ms,
    )
}

fn item_row(i: &Item) -> String {
    let mut meta = Vec::new();
    for s in [&i.prio, &i.repo, &i.class, &i.who, &i.age] {
        if !s.is_empty() {
            meta.push(esc(s));
        }
    }
    if i.rework {
        meta.push("<span class=amber>REWORK</span>".into());
    }
    if !i.claim.is_empty() {
        meta.push(if i.claim.starts_with('#') { format!("<span class=green>{}</span>", linked(&i.claim)) } else { format!("<span class=amber>{}</span>", linked(&i.claim)) });
    }
    if !i.holds.is_empty() {
        meta.push(format!("<span class=red>held {}</span>", esc(&i.holds.join(","))));
    }
    let note = if i.note.is_empty() { String::new() } else { format!("<span class=\"t small dim\">{}</span>", linked(&i.note)) };
    let why = if i.why.is_empty() || i.why == i.note { String::new() } else { format!("<details class=t><summary class=\"small dim\">why ›</summary><pre>{}</pre></details>", linked(&i.why)) };
    let out = if i.out.is_empty() {
        String::new()
    } else {
        let level = match i.out_level.as_str() { "bad" => "red", "warn" => "amber", _ => "dim" };
        format!("<details class=t><summary class=\"small {level}\">output · {} ›</summary><pre>{}</pre></details>", esc(&i.out_age), linked(&i.out.join("\n")))
    };
    format!("<div class=row>{}<span class=\"small dim\">{}</span><span class=t>{}</span>{note}{why}{out}</div>", bead(&i.id), meta.join(" · "), linked(&i.title))
}

fn items(list: &[Item]) -> String {
    list.iter().map(item_row).collect()
}

/// The pipeline, first on the page: one full-width line per state with its count, detail and
/// moves; tapping a line opens its edges and beads. Red states say so on the line itself.
fn pipeline(p: &Page) -> String {
    let v = p.view;
    let mut b = String::from("<h2>States <span class=dim>count · moves in/out per hour</span></h2>");
    for s in &v.machine {
        let out: i64 = s.edges.iter().map(|e| e.rate_1h).sum();
        let inn: i64 = v.machine.iter().flat_map(|m| m.edges.iter()).filter(|e| e.to == s.name).map(|e| e.rate_1h).sum();
        let refused: i64 = s.edges.iter().flat_map(|e| e.events.iter()).map(|(_, n, _)| *n).sum();
        let out_chip = format!(" · {inn}/h in · {out}/h out");
        let refused_chip = if refused > 0 { format!(" · <span class=red>{refused} refused</span>") } else { String::new() };
        let summary = format!(
            "<span class=\"n{c}\">{count}</span><span class=\"k{c}\">{name}</span><span class=\"small dim\">{detail}{out}{refused}</span>{dot}",
            c = if s.red { " red" } else { "" },
            count = s.count,
            name = esc(&s.name),
            detail = esc(&s.detail),
            out = out_chip,
            refused = refused_chip,
            dot = if s.red { " <span class=red>●</span>" } else { "" },
        );
        let edges: String = s
            .edges
            .iter()
            .map(|e| {
                let refusals: String = e.events.iter().filter(|(_, n, _)| *n > 0).map(|(ev, n, why)| format!(" <span class=red>{ev} refused {n}/h — {}</span>", linked(why))).collect();
                format!("<div class=\"row small\"><span>{}{}</span><span class=dim>{}/h</span>{refusals}</div>", esc(&e.event), if e.to.is_empty() { String::new() } else { format!(" ▸ {}", esc(&e.to)) }, e.rate_1h)
            })
            .collect();
        let rework = if s.no_rework_exit { "<div class=\"row small red\">no exit to REWORK ●</div>" } else { "" };
        if s.beads.is_empty() && edges.is_empty() {
            b.push_str(&plain(&summary));
        } else {
            b.push_str(&fold(&format!("st-{}", s.name), false, &summary, &format!("{}{edges}{rework}", items(&s.beads))));
        }
    }
    let flow = v.flow.iter().map(|(s, n)| format!("{} {n}", esc(s))).collect::<Vec<_>>().join(" → ");
    let mut more = format!(
        "<div class=\"row small dim\"><span class=t>{flow}</span><span>ready {} · held {} ({}%) · rework {}</span><span>24 h: dropped {} · superseded {} · landed all-time {}</span><span>terminal 24 h: {}</span></div>",
        v.ready, v.ready_held, v.held_pct, v.rework, v.dropped_24h, v.superseded_24h, v.landed_total, esc(&v.terminal)
    );
    if v.next_unknown {
        more.push_str("<div class=\"row small amber\">up next: unknown — the claim tool did not answer</div>");
    } else if !v.next.is_empty() {
        more.push_str(&format!("<div class=\"row small dim\">up next · {}</div>{}", v.next_count, items(&v.next)));
    }
    for l in v.pipe.iter().filter(|l| l.count > 0) {
        more.push_str(&format!("<div class=\"row small dim\">{} {} · oldest {}</div>{}", esc(&l.state), l.count, esc(&l.oldest), items(&l.items)));
    }
    if !v.rework_items.is_empty() {
        more.push_str(&format!("<div class=\"row small amber\">rework · {}</div>{}", v.rework_items.len(), items(&v.rework_items)));
    }
    b.push_str(&fold("flow", false, "<span class=\"small dim\">flow · up next · oldest · rework</span>", &more));
    b
}

/// Whether the pass file is this round's. round-vm writes the round's name or, in its other
/// shape, the head sha; a sha can only be placed when exactly one VM round is live.
fn progress_names(p: &lcview::PassProgress, r: &RepoRound, only_vm: bool) -> bool {
    let sha = p.round.len() == 40 && p.round.bytes().all(|b| b.is_ascii_hexdigit());
    if sha {
        return only_vm && r.kind == "VM round";
    }
    r.batch == p.round || r.batch.ends_with(&format!("/{}", p.round))
}

/// One line per round with its single progress bar; members, links and staging open under it.
fn round_line(r: &RepoRound, prog: Option<&lcview::PassProgress>, now: i64, only_vm: bool) -> String {
    let link = format!("/rounds/{}/{}", esc(&r.repo), r.batch.rsplit('/').next().map(esc).unwrap_or_default());
    if r.stale {
        return plain(&format!("<a class=id href=\"{link}\">{}</a><span class=\"badge red\">STALE</span><span class=\"small dim\">OPEN {} with no live run and no PR</span>", esc(&r.batch), esc(&r.age)));
    }
    let mine = prog.filter(|p| p.running(now) && progress_names(p, r, only_vm));
    let state = match mine {
        Some(p) if p.phase == "suites" => {
            let el = now - p.suites_started;
            let class = if el > p.cap { "red" } else if el > 720 { "amber" } else { "dim" };
            format!("{} <span class=\"small {class}\">{} of {}m</span>{}", progress_bar(p.done as u64, p.total as u64, !p.red.is_empty()), dur(el.max(0)), p.cap / 60, if p.red.is_empty() { String::new() } else { format!(" <span class=\"small red\">{} red</span>", p.red.len()) })
        }
        Some(p) => format!("<span class=small>{} {}</span>", esc(&p.phase), dur((now - p.build_started).max(0))),
        None if r.total > 0 => format!("{}{}", progress_bar(r.done as u64, r.total as u64, r.red > 0), if r.red > 0 { format!(" <span class=\"small red\">{} red</span>", r.red) } else { String::new() }),
        None => format!("<span class=small>{}</span>", esc(&r.phase)),
    };
    let summary = format!("<span class=s>{}</span><span class=id>{}</span>{state}<span class=s>{}</span>", esc(&r.repo), esc(r.batch.rsplit('/').next().unwrap_or(&r.batch)), esc(&r.age));
    let mut body = format!("<div class=\"row small\"><a class=id href=\"{link}\">{}</a><span class=dim>{} · {}</span></div>", esc(&r.batch), esc(&r.kind), esc(&r.phase));
    let mut links = Vec::new();
    if !r.pr_url.is_empty() {
        links.push(format!("<a class=id href=\"{}\">PR</a>", esc(&r.pr_url)));
    }
    if !r.run_url.is_empty() && r.red > 0 {
        links.push(format!("<a class=red href=\"{}\">failing run</a>", esc(&r.run_url)));
    }
    if !r.round_url.is_empty() {
        links.push(format!("<a class=id href=\"{}\">log</a>", esc(&r.round_url)));
    }
    if !links.is_empty() {
        body.push_str(&format!("<div class=\"row small\">{}</div>", links.join(" · ")));
    }
    if r.round.members.is_empty() {
        body.extend(r.members.iter().map(|m| format!("<div class=row>{}</div>", bead(m))));
    }
    for m in &r.round.members {
        body.push_str(&format!("<div class=row>{}<span class=\"small dim\">{} {}{}</span><span class=t>{}</span></div>", bead(&m.id), esc(&m.prio), esc(&m.repo), if m.ejects > 0 { format!(" · <span class=amber>ejected {}× before</span>", m.ejects) } else { String::new() }, linked(&m.title)));
    }
    if !r.round.ejected.is_empty() {
        body.push_str(&format!("<div class=\"row small red\">ejected this round: {}</div>", r.round.ejected.iter().map(|e| bead(e)).collect::<Vec<_>>().join(" ")));
    }
    for s in &r.round.staged {
        body.push_str(&format!("<div class=\"row small dim\">staged behind it: {} · {} · {} members</div>", esc(&s.name), esc(&s.state), s.members.len()));
    }
    fold(&format!("r-{}", r.batch), false, &summary, &body)
}

fn rounds(p: &Page) -> String {
    let v = p.view;
    let live = v.rounds.iter().filter(|r| !r.stale).count();
    let only_vm = v.rounds.iter().filter(|r| !r.stale && r.kind == "VM round").count() == 1;
    let mut b = format!("<h2>Rounds <span class=dim>{live} live · <a href=\"/rounds\">all ›</a></span></h2>");
    for r in &v.rounds {
        b.push_str(&round_line(r, v.progress.as_ref(), p.now, only_vm));
    }
    if v.rounds.is_empty() {
        match &v.round {
            Some(r) => b.push_str(&fold(
                "last-round",
                false,
                &format!("<span class=\"small dim\">last</span><span class=id>{}</span><span class=small>{}</span><span class=\"small dim\">{} · {} pass{}</span>", esc(&r.name), esc(&r.state), esc(&r.age), r.passes, if r.passes == 1 { "" } else { "es" }),
                &r.members.iter().map(|m| format!("<div class=row>{}<span class=t>{}</span></div>", bead(&m.id), linked(&m.title))).collect::<String>(),
            )),
            None => b.push_str(&plain("<span class=\"small dim\">no round or batch is running</span>")),
        }
    }
    let mut extra = String::new();
    extra.extend(v.menders.iter().map(|m| format!("<div class=\"row small{}\">mender · {} pass {} · {} · {}</div>", if m.red { " red" } else { "" }, esc(&m.batch), m.pass, esc(&m.suite), esc(&m.detail))));
    extra.extend(v.batch.iter().map(|x| format!("<div class=\"row small{}\">batch · {} · {} · {}</div>", if x.red { " red" } else { "" }, esc(&x.name), x.count, esc(&x.detail))));
    extra.extend(v.vm_runs.iter().map(|r| format!("<div class=\"row small dim\">{}</div>", esc(&r.line()))));
    for r in p.flight.rows.iter().filter(|r| r.kind != "round") {
        let age = r.started.map_or("?".to_string(), |t| dur((p.now - t).max(0)));
        let prog = match (r.done, r.total) {
            (Some(d), Some(t)) => progress_bar(d, t, false),
            (Some(d), None) => format!("<span class=small>{d}/?</span>"),
            _ => String::new(),
        };
        extra.push_str(&format!("<div class=\"row small\"><b>{}</b><span>{}</span><span class=dim>{} · {age}</span>{prog}</div>", esc(&r.kind), linked(&r.subject), esc(&r.detail)));
    }
    if !extra.is_empty() {
        b.push_str(&fold("rounds-more", false, &format!("<span class=\"small dim\">in flight · menders · batches · VM pool <span class=dim>({})</span></span>", esc(&p.flight.world)), &extra));
    }
    b
}

fn dwarves(p: &Page) -> String {
    let v = p.view;
    let mut b = format!("<h2>Dwarves <span class=dim>{} of {} · {} working bead{} · <a href=\"/dwarves\">detail ›</a></span></h2>", crate::now::dwarves(p.snap), v.ceiling, v.working, if v.working == 1 { "" } else { "s" });
    if v.now_items.is_empty() {
        b.push_str(&plain("<span class=\"small dim\">no dwarf is live</span>"));
    }
    for i in &v.now_items {
        let name = i.who.split('@').next().unwrap_or_default();
        let id = if i.id == "?" { "<span class=dim>?</span>".to_string() } else { bead(&i.id) };
        let summary = format!("<span class=s><b>{}</b> {}</span>{id}<span class=s>{}</span><span class=t>{}</span>", esc(name), esc(&i.persona), esc(&i.age), linked(&i.title));
        b.push_str(&fold(&format!("a-{}", i.id), false, &summary, &item_row(i)));
    }
    b
}

/// Everything below the work in progress: one line each, counts on the line, rows on a tap.
/// Questions for the operator live here, not at the top: he cannot answer from this page.
fn below(p: &Page) -> String {
    let v = p.view;
    let mut b = String::from("<h2>More</h2>");
    // Things that are wrong come first among these lines and are red on the line.
    if !v.errors.is_empty() {
        b.push_str(&fold("errors", true, &format!("<span class=\"n red\">{}</span><span class=\"k red\">sources failing</span>", v.errors.len()), &v.errors.iter().map(|e| format!("<div class=\"row small red\">{}</div>", linked(e))).collect::<String>()));
    }
    if !p.flight.readable || p.flight.unread() {
        b.push_str(&plain(&format!("<span class=\"k red\">in flight unreadable</span><span class=\"small red\">{}</span>", esc(&p.flight.segment))));
    }
    if !v.drift.is_empty() {
        b.push_str(&fold("drift", false, &format!("<span class=\"n red\">{}</span><span class=\"k red\">drift</span><span class=\"small dim\">ready or rework but already on {}</span>", v.drift.len(), esc(&v.base)), &v.drift.iter().map(|d| format!("<div class=row>{}</div>", linked(d))).collect::<String>()));
    }
    let mut past: Vec<(i64, &lcview::DwellRow)> = p.snap.dwell.iter().filter_map(|d| d.p95_s.map(|q| (p.now - d.entered_at - q, d))).filter(|(o, _)| *o > 0).collect();
    past.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.bead_id.cmp(&b.1.bead_id)));
    if !past.is_empty() {
        let rows: String = past
            .iter()
            .map(|(_, d)| {
                let title = p.snap.meta.get(&d.bead_id).map(|m| m.title.as_str()).unwrap_or_default();
                format!("<div class=row>{}<span class=\"badge red\">{} in {} · p95 {}</span><span class=t>{}</span></div>", bead(&d.bead_id), dur(p.now - d.entered_at), esc(&d.state), dur(d.p95_s.unwrap_or(0)), linked(title))
            })
            .collect();
        b.push_str(&fold("p95", false, &format!("<span class=\"n red\">{}</span><span class=k>past p95</span><a class=\"small dim\" href=\"/stuck\">stuck ›</a>", past.len()), &rows));
    }
    if let UnitsState::Held { snap: u, age_s, stale } = p.units {
        let mut bad: Vec<&units::Row> = u.rows.iter().filter(|r| r.kind != "system" && matches!(r.verdict, units::Verdict::Failing | units::Verdict::Late)).collect();
        bad.sort_by(|a, b| (a.verdict as u8, &a.unit).cmp(&(b.verdict as u8, &b.unit)));
        if !bad.is_empty() {
            let mut rows = String::new();
            if *stale {
                rows.push_str(&format!("<div class=\"row small dim\">unit history is {} old, refreshing</div>", dur(*age_s)));
            }
            for r in &bad {
                let why = if r.verdict == units::Verdict::Failing { "failing".to_string() } else { format!("late — last success {}", r.last_success.map_or("never".into(), |t| format!("{} ago", dur(u.now - t)))) };
                rows.push_str(&format!("<div class=row><a class=id href=\"/units\">{}</a><span class=\"badge red\">{}</span></div>", esc(&r.unit), esc(&why)));
            }
            b.push_str(&fold("units", false, &format!("<span class=\"n red\">{}</span><span class=k>units failing or late</span>", bad.len()), &rows));
        }
    } else {
        b.push_str(&plain("<span class=\"n dim\">…</span><span class=k>units</span><span class=\"small dim\">unit history is loading</span>"));
    }
    match &v.decide_error {
        Some(e) => b.push_str(&plain(&format!("<span class=\"n red\">?</span><span class=k>questions for you</span><span class=\"small red\">{}</span>", linked(e)))),
        None => b.push_str(&fold("asks", false, &format!("<span class=n>{}</span><span class=k>questions for you</span><span class=\"small dim\">answer in chat or mail</span>", v.decide.len()), &items(&v.decide))),
    }
    let epics: String = v
        .epics
        .iter()
        .map(|e| {
            let total = (e.landed + e.certified + e.rework + e.building + e.waiting).max(0) as u64;
            let holds: String = e.holds.iter().map(|h| format!(" · <span class=amber>held {}: {}</span>", esc(&h.label), h.n)).collect();
            format!("<div class=row><span class=t><b>{}</b> <span class=dim>{}</span></span>{}<span class=\"small dim\">{} certified · {} rework · {} building · {} waiting{holds}</span></div>", esc(&e.epic), esc(&e.repo), progress_bar(e.landed as u64, total, false), e.certified, e.rework, e.building, e.waiting)
        })
        .collect();
    b.push_str(&fold("more › epics", false, &format!("<span class=n>{}</span><span class=k>epics</span>", v.epics.len()), &epics));
    b.push_str(&fold("more › blocked", false, &format!("<span class=n>{}</span><span class=k>blocked</span>", v.blocked.len()), &items(&v.blocked)));
    let refused_n: i64 = v.refused.iter().map(|r| r.n).sum();
    let refused: String = v.refused.iter().map(|r| format!("<div class=\"row small{}\"><span>{} <b>{}</b></span><span class=dim>{}</span></div>", if r.red { " red" } else { "" }, esc(&r.what), r.n, esc(&r.why))).collect();
    let refused_red = v.refused.iter().any(|r| r.red);
    b.push_str(&fold("more › refused", false, &format!("<span class=\"n{}\">{refused_n}</span><span class=k>refused (1 h)</span>", if refused_red { " red" } else { "" }), &refused));
    let holds: String = v.holds.iter().map(|h| format!("<div class=row><b class=\"{}\">{}</b> {}<span class=\"t small dim\">{}</span></div>", if h.kind == "poison" { "red" } else { "amber" }, esc(&h.kind), h.count, h.top.iter().map(|(n, r)| format!("{n}× {}", linked(r))).collect::<Vec<_>>().join(" · "))).collect();
    b.push_str(&fold("more › holds", false, &format!("<span class=n>{}</span><span class=k>holds</span>", v.holds.iter().map(|h| h.count).sum::<usize>()), &holds));
    b.push_str(&fold("more › recent", false, &format!("<span class=n>{}</span><span class=k>recent</span>", v.recent.len()), &items(&v.recent)));
    b.push_str(&format!("<div class=\"line small dim\">snapshot {} · <a href=\"/api/lifecycle\">raw JSON</a> · <a href=\"/api/ops\">ops JSON</a> · <a href=\"/lifecycle\">old page</a></div>", esc(&v.clock)));
    b
}

pub fn main_html(p: &Page) -> String {
    format!("{}{}{}{}", pipeline(p), rounds(p), dwarves(p), below(p))
}

pub const TABS: [(&str, &str); 5] = [("overview", "Overview"), ("rounds", "Rounds"), ("dwarves", "Dwarves"), ("stuck", "Stuck"), ("units", "Units")];

fn tabs(active: &str) -> String {
    let href = |k: &str| if k == "overview" { format!("{BASE}/") } else { format!("/{k}") };
    let mut t = String::from("<nav class=tabs>");
    for (k, label) in TABS {
        t.push_str(&format!("<a href=\"{}\"{}>{label}</a>", href(k), if k == active { " class=on aria-current=page" } else { "" }));
    }
    t.push_str("</nav>");
    t
}

pub fn page(p: &Page) -> String {
    format!(
        "<!doctype html><html lang=en><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\"><meta name=color-scheme content=dark><title>Spira</title><style>{CSS}</style><body>{}{}<main id=main>{}</main><script>{JS}</script></body></html>",
        header(p),
        tabs("overview"),
        main_html(p),
    )
}

pub fn fragment(p: &Page) -> String {
    serde_json::json!({ "head": header(p), "main": main_html(p) }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cockpit_ops::health::model::Snapshot as Env;

    fn page_for(snap: &Snapshot, env: &str) -> String {
        let view = lcview::view(snap);
        let flight = Flight::from_snapshot(&Env::parse(env), snap.now, 60);
        let units = UnitsState::Pending;
        page(&Page { snap, view: &view, flight: &flight, units: &units, now: snap.now, age_s: 5 })
    }

    #[test]
    fn every_view_field_has_a_home() {
        let v = serde_json::to_value(View::default()).expect("the view serialises");
        let fields: Vec<String> = v.as_object().expect("an object").keys().cloned().collect();
        let homes: Vec<&str> = VIEW_HOMES.iter().map(|(k, _)| *k).collect();
        let missing: Vec<&String> = fields.iter().filter(|f| !homes.contains(&f.as_str())).collect();
        assert!(missing.is_empty(), "View fields with no home on the site: {missing:?}");
        let extra: Vec<&&str> = homes.iter().filter(|h| !fields.iter().any(|f| f == *h)).collect();
        assert!(extra.is_empty(), "homes listed for fields View no longer has: {extra:?}");
    }

    #[test]
    fn the_header_is_one_line_with_no_progress_bar() {
        let snap = Snapshot { now: 1_000, release: "049592eccd670c2c860c0aee800a1d60be0a6fbf".into(), world: "plane work: RUNNING".into(), ceiling: 4, ..Default::default() };
        let html = page_for(&snap, "SP_AT='995'\nSP_INFLIGHT_SEG='⚙ idle'\nSP_INFLIGHT_WORLD='running'\n");
        assert_eq!(html.matches("<header").count(), 1, "{html}");
        let head = &html[html.find("<header").unwrap()..html.find("</header>").unwrap()];
        assert!(head.contains("049592e") && head.contains("dwarves 0/4") && !head.contains("prog"), "{head}");
    }

    #[test]
    fn a_running_round_draws_its_progress_exactly_once() {
        let progress = lcview::PassProgress { round: "r-auto-582".into(), pass: 1, phase: "suites".into(), done: 99, total: 502, suites_started: 900, cap: 900, updated_at: 995, ..Default::default() };
        let snap = Snapshot {
            now: 1_000,
            ceiling: 4,
            progress: Some(progress),
            batches: vec![lcview::BatchRow { id: "r-auto-582".into(), state: "OPEN".into(), repo: "spira".into(), members: vec!["sp-a".into()], opened_at: 500, last_at: 990, phase: "certifying".into(), ..Default::default() }],
            ..Default::default()
        };
        let env = "SP_AT='995'\nSP_INFLIGHT_WORLD='running'\nSP_INFLIGHT_SEG='⚙ 1 round (95/502)'\nSP_INFLIGHT_ROW0='round|spira/r-auto-582|certifying|500|95|502'\n";
        let html = page_for(&snap, env);
        assert_eq!(html.matches("class=bar>").count(), 1, "one bar for one round: {html}");
        assert!(html.contains("99/502"), "the live pass count wins: {html}");
        assert!(!html.contains("95/502"), "the collector's older count is not drawn a second time: {html}");
    }

    #[test]
    fn asks_are_listed_in_full_and_absence_is_not_an_alarm() {
        let quiet = page_for(&Snapshot { now: 1_000, ..Default::default() }, "SP_AT='995'\nSP_INFLIGHT_SEG='⚙ idle'\nSP_INFLIGHT_WORLD='running'\n");
        assert!(!quiet.contains("class=\"n red\""), "nothing is wrong, so nothing is red: {quiet}");
        assert!(quiet.find("States").unwrap() < quiet.find("questions for you").unwrap(), "states come before the questions he cannot answer here");
        let dark = page_for(&Snapshot { now: 1_000, ..Default::default() }, "SP_AT='10'\n");
        assert!(dark.contains("in flight unreadable"), "positive control: an unreadable read is an alarm, not quiet: {dark}");
    }

    #[test]
    fn bead_ids_in_text_become_links() {
        let h = linked("waits on sp-q75rm0 (master key) and sp-1.2.");
        assert!(h.contains("href=\"/beads/sp-q75rm0\"") && h.contains("href=\"/beads/sp-1.2\"") && h.ends_with("."), "{h}");
        assert_eq!(linked("no ids here"), "no ids here");
    }
}
