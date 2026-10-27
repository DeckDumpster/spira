//! The web-native site (brain design `loom-web-native-2026-10-26`, approved 2026-10-26): one
//! header, each fact once per screen, nothing the lifecycle page showed left without a home.
//! Served under `/next` beside the old pages until the operator says "switch".
//!
//! Everything here is a pure function of the lifecycle view, the in-flight read and the held
//! units snapshot; the routes do the reading.

use crate::now::UnitsState;
use crate::stuck::{dur, esc};
use crate::units;
use cockpit_ops::health::inflight::{Flight, FlightRow};
use cockpit_ops::lcview::{self, Item, RepoRound, Snapshot, View};

pub const BASE: &str = "/next";

/// Every top-level field of `View`, and where the site draws it. The coverage test fails when
/// `View` grows a field that is not listed here, so nothing the model knows goes undrawn.
pub const VIEW_HOMES: &[(&str, &str)] = &[
    ("clock", "footer › diagnostics"),
    ("release", "header"),
    ("world", "header"),
    ("world_running", "header"),
    ("working", "aeons heading (working beads)"),
    ("ceiling", "header and aeons heading"),
    ("errors", "attention › sources"),
    ("gather_ms", "footer › diagnostics"),
    ("failures", "header chip and footer"),
    ("flow", "pipeline summary"),
    ("ready", "pipeline summary"),
    ("ready_held", "pipeline summary"),
    ("held_pct", "pipeline summary"),
    ("rework", "pipeline summary"),
    ("dropped_24h", "pipeline › terminal"),
    ("superseded_24h", "pipeline › terminal"),
    ("landed_total", "pipeline › terminal"),
    ("holds", "holds"),
    ("base", "attention › drift"),
    ("drift", "attention › drift"),
    ("now_items", "aeons"),
    ("pipe", "pipeline › oldest per state"),
    ("rework_items", "pipeline › rework"),
    ("next_count", "pipeline › up next"),
    ("next_unknown", "pipeline › up next"),
    ("next", "pipeline › up next"),
    ("blocked", "blocked"),
    ("recent", "recent"),
    ("machine", "pipeline › states"),
    ("terminal", "pipeline › terminal"),
    ("refused", "refused"),
    ("batch", "rounds › batches"),
    ("round", "rounds (when no repo round is live)"),
    ("rounds", "rounds"),
    ("vm_runs", "rounds › VM pool"),
    ("progress", "rounds (the one progress bar)"),
    ("now", "header age"),
    ("decide", "attention › waiting on you"),
    ("decide_error", "attention › waiting on you"),
    ("menders", "rounds › menders"),
    ("epics", "epics"),
    ("inflight", "in flight"),
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

const CSS: &str = ":root{--bg:#f6f7f9;--card:#fff;--line:#e3e6ea;--fg:#1c2330;--dim:#6b7684;--red:#c8333a;--redbg:#fde8e9;--amber:#a86500;--green:#1f7a4d;--acc:#2f6fb3;--bar:#e9edf2}\
@media(prefers-color-scheme:dark){:root:not([data-theme=light]){--bg:#10141a;--card:#161b22;--line:#252c36;--fg:#d8dee9;--dim:#7d8896;--red:#ff6b6d;--redbg:#3a1a1c;--amber:#e0a43a;--green:#4cc38a;--acc:#5aa2e6;--bar:#232a33}}\
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.4 system-ui,-apple-system,sans-serif;padding-bottom:4rem;overflow-wrap:anywhere}\
a{color:inherit}a.id,.id{font-weight:600;font-variant-numeric:tabular-nums;text-decoration:none;color:var(--acc)}\
header.top{position:sticky;top:0;z-index:5;display:flex;flex-wrap:wrap;gap:.25rem .7rem;align-items:center;padding:.55rem 1rem;background:var(--card);border-bottom:1px solid var(--line);font-size:.9rem}\
.chip{display:inline-block;padding:.05rem .5rem;border-radius:999px;font-size:.8rem;font-weight:600;background:var(--bar)}\
.chip.ok{color:var(--green)}.chip.warn{color:var(--amber)}.chip.bad{color:var(--red);background:var(--redbg)}\
main{max-width:46rem;margin:0 auto;padding:.4rem 1rem}\
section{margin:.9rem 0}section>h2,details.sec>summary{font-size:.78rem;letter-spacing:.06em;text-transform:uppercase;color:var(--dim);margin:0 0 .35rem;font-weight:600}\
details.sec>summary{cursor:pointer;list-style:none;padding:.2rem 0}details.sec>summary::-webkit-details-marker{display:none}details.sec>summary:before{content:'▸ ';}details.sec[open]>summary:before{content:'▾ '}\
.card{background:var(--card);border:1px solid var(--line);border-radius:10px;padding:.6rem .75rem;margin:.4rem 0}\
.card.alarm{border-left:4px solid var(--red)}\
.row{display:flex;flex-wrap:wrap;gap:.1rem .5rem;align-items:baseline;padding:.35rem 0;border-top:1px solid var(--line)}.row:first-child{border-top:0}\
.row .t{flex:1 1 14rem;min-width:0}.dim{color:var(--dim)}.red{color:var(--red)}.amber{color:var(--amber)}.green{color:var(--green)}.small{font-size:.85rem}\
.prog{display:flex;align-items:center;gap:.5rem;margin:.35rem 0}.prog .track{flex:1 1 auto;height:8px;border-radius:4px;background:var(--bar);overflow:hidden}.prog .fill{height:100%;background:var(--acc)}.prog .fill.red{background:var(--red)}\
.states{display:grid;grid-template-columns:repeat(auto-fill,minmax(9.5rem,1fr));gap:.4rem}\
.state{display:block;background:var(--card);border:1px solid var(--line);border-radius:8px;padding:.4rem .55rem}.state b{font-size:1.15rem}\
details.inner{margin:.15rem 0}details.inner>summary{cursor:pointer}\
pre{white-space:pre-wrap;font-size:.8rem;margin:.3rem 0;color:var(--dim)}\
.badge{display:inline-block;font-size:.75rem;padding:0 .4rem;border-radius:4px;background:var(--bar)}.badge.red{background:var(--redbg);color:var(--red)}\
footer{max-width:46rem;margin:1rem auto;padding:0 1rem;font-size:.8rem;color:var(--dim)}\
nav.tabs{position:fixed;left:0;right:0;bottom:0;z-index:6;display:flex;background:var(--card);border-top:1px solid var(--line)}\
nav.tabs a{flex:1 1 0;min-width:0;text-align:center;padding:.75rem .1rem;font-size:.82rem;text-decoration:none;color:var(--dim)}nav.tabs a.on{color:var(--fg);font-weight:600;box-shadow:inset 0 2px 0 var(--acc)}\
@media(min-width:720px){nav.tabs{position:sticky;top:2.6rem;bottom:auto;justify-content:center;border-top:0;border-bottom:1px solid var(--line)}nav.tabs a{flex:0 0 auto;padding:.6rem 1.2rem}body{padding-bottom:0}}";

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

fn section(key: &str, title: &str, extra: &str, open: bool, body: &str) -> String {
    format!(
        "<details class=sec data-k=\"{key}\"{}><summary>{title} <span class=dim>{extra}</span></summary>{body}</details>",
        if open { " open" } else { "" }
    )
}

fn progress_bar(done: u64, total: u64, red: bool) -> String {
    let pct = if total == 0 { 0 } else { (done.min(total) * 100) / total };
    format!(
        "<div class=prog><div class=track><div class=\"fill{}\" style=\"width:{pct}%\"></div></div><span class=small>{done}/{total}</span></div>",
        if red { " red" } else { "" }
    )
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

pub fn header(p: &Page) -> String {
    let v = p.view;
    let mut chips = String::new();
    if p.age_s > crate::LIFECYCLE_STALE_S {
        chips.push_str(&format!("<span class=\"chip bad\">stale {} — collector not running?</span>", dur(p.age_s)));
    }
    if v.failures > 0 {
        chips.push_str(&format!("<span class=\"chip bad\">{} source failure{}</span>", v.failures, if v.failures == 1 { "" } else { "s" }));
    }
    format!(
        "<header class=top id=head>{}<span class=dim title=\"{}\">{}</span><span>aeons {}/{}</span><span class=dim>{}</span>{chips}</header>",
        world_chip(v),
        esc(&v.release),
        esc(short(&v.release)),
        crate::now::aeons(p.snap),
        v.ceiling,
        if p.age_s <= 1 { "now".to_string() } else { format!("{} ago", dur(p.age_s)) },
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
        let c = if i.claim.starts_with('#') { format!("<span class=green>{}</span>", esc(&i.claim)) } else { format!("<span class=amber>{}</span>", esc(&i.claim)) };
        meta.push(c);
    }
    if !i.holds.is_empty() {
        meta.push(format!("<span class=red>held {}</span>", esc(&i.holds.join(","))));
    }
    let note = if i.note.is_empty() { String::new() } else { format!("<div class=\"small dim\">{}</div>", linked(&i.note)) };
    let why = if i.why.is_empty() || i.why == i.note { String::new() } else { format!("<details class=inner><summary class=\"small dim\">why</summary><pre>{}</pre></details>", linked(&i.why)) };
    let out = if i.out.is_empty() {
        String::new()
    } else {
        let level = match i.out_level.as_str() { "bad" => "red", "warn" => "amber", _ => "dim" };
        format!("<details class=inner><summary class=\"small {level}\">output · {}</summary><pre>{}</pre></details>", esc(&i.out_age), esc(&i.out.join("\n")))
    };
    format!("<div class=row>{}<span class=t>{}</span><span class=\"small dim\">{}</span>{note}{why}{out}</div>", bead(&i.id), esc(&i.title), meta.join(" · "))
}

fn items(list: &[Item]) -> String {
    list.iter().map(item_row).collect()
}

fn attention(p: &Page) -> String {
    let v = p.view;
    let mut b = String::new();
    // Waiting on you: every ask, never a top five.
    if let Some(e) = &v.decide_error {
        b.push_str(&format!("<div class=\"card alarm\"><b>Waiting on you · ?</b><div class=red>{}</div></div>", esc(e)));
    } else if !v.decide.is_empty() {
        b.push_str(&format!("<div class=\"card alarm\"><b>Waiting on you · {}</b>{}</div>", v.decide.len(), items(&v.decide)));
    }
    if !v.errors.is_empty() {
        b.push_str(&format!("<div class=\"card alarm\"><b>Sources failing</b>{}</div>", v.errors.iter().map(|e| format!("<div class=\"row red small\">{}</div>", esc(e))).collect::<String>()));
    }
    if !v.drift.is_empty() {
        b.push_str(&format!(
            "<div class=\"card alarm\"><b>Drift · {}</b> <span class=\"small dim\">READY or REWORK, but already on {}</span>{}</div>",
            v.drift.len(),
            esc(&v.base),
            v.drift.iter().map(|d| format!("<div class=row>{}</div>", linked(d))).collect::<String>()
        ));
    }
    let mut past: Vec<(i64, &lcview::DwellRow)> = p.snap.dwell.iter().filter_map(|d| d.p95_s.map(|q| (p.now - d.entered_at - q, d))).filter(|(o, _)| *o > 0).collect();
    past.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.bead_id.cmp(&b.1.bead_id)));
    if !past.is_empty() {
        let rows: String = past
            .iter()
            .map(|(_, d)| {
                let title = p.snap.meta.get(&d.bead_id).map(|m| m.title.as_str()).unwrap_or_default();
                format!("<div class=row>{}<span class=t>{}</span><span class=\"badge red\">{} in {} · p95 {}</span></div>", bead(&d.bead_id), esc(title), dur(p.now - d.entered_at), esc(&d.state), dur(d.p95_s.unwrap_or(0)))
            })
            .collect();
        b.push_str(&format!("<div class=\"card alarm\"><b>Past p95 · {}</b> <a class=\"small dim\" href=\"/stuck\">where work is stuck ›</a>{rows}</div>", past.len()));
    }
    match p.units {
        UnitsState::Pending => {}
        UnitsState::Held { snap: u, age_s, stale } => {
            let mut bad: Vec<&units::Row> = u.rows.iter().filter(|r| r.kind != "system" && matches!(r.verdict, units::Verdict::Failing | units::Verdict::Late)).collect();
            bad.sort_by(|a, b| (a.verdict as u8, &a.unit).cmp(&(b.verdict as u8, &b.unit)));
            if !bad.is_empty() || *stale {
                let mut rows = String::new();
                if *stale {
                    rows.push_str(&format!("<div class=\"row red small\">unit history is {} old, refreshing</div>", dur(*age_s)));
                }
                for r in bad {
                    let why = if r.verdict == units::Verdict::Failing { "failing".to_string() } else { format!("late — last success {}", r.last_success.map_or("never".into(), |t| format!("{} ago", dur(u.now - t)))) };
                    rows.push_str(&format!("<div class=row><a class=id href=\"/units\">{}</a><span class=\"badge red\">{}</span></div>", esc(&r.unit), esc(&why)));
                }
                b.push_str(&format!("<div class=\"card alarm\"><b>Units</b>{rows}</div>"));
            }
        }
    }
    if !p.flight.readable || p.flight.unread() {
        b.push_str(&format!("<div class=\"card alarm\"><b>Work in flight unreadable</b><div class=\"red small\">{}</div></div>", esc(&p.flight.segment)));
    }
    if b.is_empty() {
        return String::new();
    }
    format!("<section id=attention><h2>Attention</h2>{b}</section>")
}

/// The round's progress, from the one place that has it: the live pass file when it names this
/// round, else the batch row's own count. Drawn once, here, and nowhere else on the page.
/// Whether the pass file is this round's. round-vm writes the round's name or, in its other
/// shape, the head sha; a sha can only be placed when exactly one VM round is live.
fn progress_names(p: &lcview::PassProgress, r: &RepoRound, only_vm: bool) -> bool {
    let sha = p.round.len() == 40 && p.round.bytes().all(|b| b.is_ascii_hexdigit());
    if sha {
        return only_vm && r.kind == "VM round";
    }
    r.batch == p.round || r.batch.ends_with(&format!("/{}", p.round))
}

fn round_card(r: &RepoRound, prog: Option<&lcview::PassProgress>, now: i64, only_vm: bool) -> String {
    let link = format!("/rounds/{}/{}", esc(&r.repo), r.batch.rsplit('/').next().map(esc).unwrap_or_default());
    if r.stale {
        return format!("<div class=\"card alarm\"><a class=id href=\"{link}\">{} · {}</a> <span class=\"badge red\">STALE</span><div class=\"small dim\">OPEN {} with no live run and no PR</div></div>", esc(&r.repo), esc(&r.batch), esc(&r.age));
    }
    let mine = prog.filter(|p| p.running(now) && progress_names(p, r, only_vm));
    let mut body = String::new();
    match mine {
        Some(p) if p.phase == "suites" => {
            let el = now - p.suites_started;
            let class = if el > p.cap { "red" } else if el > 720 { "amber" } else { "dim" };
            body.push_str(&format!("<div class=small>pass {} · suites · <span class={class}>{} of {}m</span></div>", p.pass.max(1), dur(el.max(0)), p.cap / 60));
            body.push_str(&progress_bar(p.done as u64, p.total as u64, !p.red.is_empty()));
            if !p.red.is_empty() {
                body.push_str(&format!("<div class=\"small red\">{} red: {}</div>", p.red.len(), esc(&p.red.join(", "))));
            }
        }
        Some(p) => {
            body.push_str(&format!("<div class=small>pass {} · {} {} <span class=dim>(outside the cap)</span></div>", p.pass.max(1), esc(&p.phase), dur((now - p.build_started).max(0))));
        }
        None if r.total > 0 => {
            body.push_str(&format!("<div class=small>{}</div>", esc(&r.phase)));
            body.push_str(&progress_bar(r.done as u64, r.total as u64, r.red > 0));
            if r.red > 0 {
                body.push_str(&format!("<div class=\"small red\">{} red</div>", r.red));
            }
        }
        None => body.push_str(&format!("<div class=small>{}</div>", esc(&r.phase))),
    }
    let mut links = Vec::new();
    if !r.pr_url.is_empty() {
        links.push(format!("<a href=\"{}\">PR</a>", esc(&r.pr_url)));
    }
    if !r.run_url.is_empty() && r.red > 0 {
        links.push(format!("<a class=red href=\"{}\">failing run</a>", esc(&r.run_url)));
    }
    if !r.round_url.is_empty() {
        links.push(format!("<a href=\"{}\">log</a>", esc(&r.round_url)));
    }
    let members: String = r.round.members.iter().map(|m| format!("<div class=row>{}<span class=t>{}</span><span class=\"small dim\">{} {}{}</span></div>", bead(&m.id), esc(&m.title), esc(&m.prio), esc(&m.repo), if m.ejects > 0 { format!(" · <span class=amber>ejected {}× before</span>", m.ejects) } else { String::new() })).collect();
    let members = if members.is_empty() { r.members.iter().map(|m| format!("<div class=row>{}</div>", bead(m))).collect() } else { members };
    let ejected = if r.round.ejected.is_empty() { String::new() } else { format!("<div class=\"small red\">ejected this round: {}</div>", r.round.ejected.iter().map(|e| bead(e)).collect::<Vec<_>>().join(" ")) };
    let staged: String = r.round.staged.iter().map(|s| format!("<div class=\"small dim\">staged behind it: {} · {} · {} members</div>", esc(&s.name), esc(&s.state), s.members.len())).collect();
    format!(
        "<div class=card><div><a class=id href=\"{link}\">{} · {}</a> <span class=\"small dim\">{} · {}</span></div>{body}<details class=inner data-k=\"m-{}\"><summary class=\"small dim\">{} members{}</summary>{members}</details>{ejected}{staged}</div>",
        esc(&r.repo),
        esc(&r.batch),
        esc(&r.kind),
        esc(&r.age),
        esc(&r.batch),
        r.members.len(),
        if links.is_empty() { String::new() } else { format!(" · {}", links.join(" · ")) },
    )
}

fn rounds(p: &Page) -> String {
    let v = p.view;
    let mut b = String::new();
    let only_vm = v.rounds.iter().filter(|r| !r.stale && r.kind == "VM round").count() == 1;
    for r in &v.rounds {
        b.push_str(&round_card(r, v.progress.as_ref(), p.now, only_vm));
    }
    if v.rounds.is_empty() {
        if let Some(r) = &v.round {
            b.push_str(&format!("<div class=card><b>{}</b> <span class=\"small dim\">{} · {} · {} pass{}</span>{}</div>", esc(&r.name), esc(&r.state), esc(&r.age), r.passes, if r.passes == 1 { "" } else { "es" }, r.members.iter().map(|m| format!("<div class=row>{}<span class=t>{}</span></div>", bead(&m.id), esc(&m.title))).collect::<String>()));
        } else {
            b.push_str("<p class=\"small dim\">no round or batch is running</p>");
        }
    }
    let mut extra = String::new();
    if !v.menders.is_empty() {
        extra.push_str(&format!("<div class=\"small\"><b>Menders</b>{}</div>", v.menders.iter().map(|m| format!("<div class=\"row small{}\">{} pass {} · {} · {}</div>", if m.red { " red" } else { "" }, esc(&m.batch), m.pass, esc(&m.suite), esc(&m.detail))).collect::<String>()));
    }
    if !v.batch.is_empty() {
        extra.push_str(&format!("<div class=\"small\"><b>Batches</b>{}</div>", v.batch.iter().map(|x| format!("<div class=\"row small{}\">{} · {} · {}</div>", if x.red { " red" } else { "" }, esc(&x.name), x.count, esc(&x.detail))).collect::<String>()));
    }
    if !v.vm_runs.is_empty() {
        extra.push_str(&format!("<div class=\"small\"><b>VM pool</b>{}</div>", v.vm_runs.iter().map(|r| format!("<div class=\"row small dim\">{}</div>", esc(&r.line()))).collect::<String>()));
    }
    if !extra.is_empty() {
        b.push_str(&format!("<details class=inner data-k=rounds-more><summary class=\"small dim\">menders · batches · VM pool</summary>{extra}</details>"));
    }
    format!("<section id=rounds><h2>Rounds · {} live <a class=\"small dim\" href=\"/rounds\">all ›</a></h2>{b}</section>", v.rounds.iter().filter(|r| !r.stale).count())
}

fn in_flight(p: &Page) -> String {
    // Round rows are already drawn as round cards; only other kinds of work are listed here.
    let rows: Vec<&FlightRow> = p.flight.rows.iter().filter(|r| r.kind != "round").collect();
    if rows.is_empty() {
        return String::new();
    }
    let body: String = rows
        .iter()
        .map(|r| {
            let age = r.started.map_or("?".to_string(), |t| dur((p.now - t).max(0)));
            let prog = match (r.done, r.total) {
                (Some(d), Some(t)) => progress_bar(d, t, false),
                (Some(d), None) => format!("<span class=small>{d}/?</span>"),
                _ => String::new(),
            };
            format!("<div class=row><b>{}</b><span class=t>{}</span><span class=\"small dim\">{} · {age}</span>{prog}</div>", esc(&r.kind), linked(&r.subject), esc(&r.detail))
        })
        .collect();
    format!("<section id=inflight><h2>In flight <span class=dim>{}</span></h2><div class=card>{body}</div></section>", esc(&p.flight.world))
}

fn aeons(p: &Page) -> String {
    let v = p.view;
    let body = if v.now_items.is_empty() { "<p class=\"small dim\">no aeon is live</p>".to_string() } else { format!("<div class=card>{}</div>", items(&v.now_items)) };
    format!(
        "<section id=aeons><h2>Aeons · {} of {} <span class=dim>· {} working bead{}</span> <a class=\"small dim\" href=\"/aeons\">detail ›</a></h2>{body}</section>",
        crate::now::aeons(p.snap),
        v.ceiling,
        v.working,
        if v.working == 1 { "" } else { "s" }
    )
}

fn pipeline(p: &Page) -> String {
    let v = p.view;
    let mut tiles = String::new();
    for s in &v.machine {
        let edges: String = s
            .edges
            .iter()
            .map(|e| {
                let refusals: String = e.events.iter().filter(|(_, n, _)| *n > 0).map(|(ev, n, why)| format!(" <span class=red>· {ev} refused {n}/h {}</span>", esc(why))).collect();
                format!("<div class=\"small\">{}{} <span class=dim>{}/h</span>{refusals}</div>", esc(&e.event), if e.to.is_empty() { String::new() } else { format!(" ▸ {}", esc(&e.to)) }, e.rate_1h)
            })
            .collect();
        let rework = if s.no_rework_exit { "<div class=\"small red\">no exit to REWORK ●</div>" } else { "" };
        tiles.push_str(&format!(
            "<details class=\"state inner\" data-k=\"st-{n}\"><summary><span class=small>{n}</span><br><b class=\"{c}\">{count}</b> <span class=\"small dim\">{detail}</span>{dot}</summary>{edges}{rework}{beads}</details>",
            n = esc(&s.name),
            c = if s.red { "red" } else { "" },
            count = s.count,
            detail = esc(&s.detail),
            dot = if s.red { " <span class=red>●</span>" } else { "" },
            beads = items(&s.beads),
        ));
    }
    let flow = v.flow.iter().map(|(s, n)| format!("{} {n}", esc(s))).collect::<Vec<_>>().join(" → ");
    let summary = format!(
        "<div class=\"small dim\">{flow}<br>ready {} · held {} ({}%) · rework {} · 24 h: dropped {} · superseded {} · landed all-time {}</div>",
        v.ready, v.ready_held, v.held_pct, v.rework, v.dropped_24h, v.superseded_24h, v.landed_total
    );
    let next = if v.next_unknown {
        "<div class=\"small amber\">up next: unknown — the claim tool did not answer</div>".to_string()
    } else if v.next.is_empty() {
        String::new()
    } else {
        format!("<details class=inner data-k=next><summary class=small>up next · {}</summary>{}</details>", v.next_count, items(&v.next))
    };
    let oldest: String = v.pipe.iter().filter(|l| l.count > 0).map(|l| format!("<details class=inner data-k=\"pipe-{s}\"><summary class=small>{s} {} · oldest {}</summary>{}</details>", l.count, esc(&l.oldest), items(&l.items), s = esc(&l.state))).collect();
    let rework = if v.rework_items.is_empty() { String::new() } else { format!("<details class=inner data-k=rework><summary class=\"small amber\">rework · {}</summary>{}</details>", v.rework_items.len(), items(&v.rework_items)) };
    format!(
        "<section id=pipeline><h2>Pipeline <span class=dim>count · moves per hour</span></h2><div class=states>{tiles}</div>{summary}{next}{rework}<details class=inner data-k=oldest><summary class=\"small dim\">oldest per state</summary>{oldest}</details><div class=\"small dim\">terminal 24 h: {}</div></section>",
        esc(&v.terminal)
    )
}

fn epics(v: &View) -> String {
    if v.epics.is_empty() {
        return String::new();
    }
    let rows: String = v
        .epics
        .iter()
        .map(|e| {
            let total = e.landed + e.certified + e.rework + e.building + e.waiting;
            let holds: String = e.holds.iter().map(|h| format!(" · <span class=amber>held {}: {}</span>", esc(&h.label), h.n)).collect();
            format!(
                "<div class=row><span class=t><b>{}</b> <span class=dim>{}</span></span>{}<span class=\"small dim\">{}/{} landed · {} certified · {} rework · {} building · {} waiting{holds}</span></div>",
                esc(&e.epic),
                esc(&e.repo),
                progress_bar(e.landed as u64, total.max(0) as u64, false),
                e.landed,
                total,
                e.certified,
                e.rework,
                e.building,
                e.waiting
            )
        })
        .collect();
    section("epics", "Epics", &v.epics.len().to_string(), false, &format!("<div class=card>{rows}</div>"))
}

fn lists(v: &View) -> String {
    let mut b = String::new();
    b.push_str(&section("blocked", "Blocked", &v.blocked.len().to_string(), false, &format!("<div class=card>{}</div>", if v.blocked.is_empty() { "<span class=\"small dim\">nothing blocked</span>".into() } else { items(&v.blocked) })));
    let refused_n: i64 = v.refused.iter().map(|r| r.n).sum();
    let refused: String = v.refused.iter().map(|r| format!("<div class=\"row small{}\">{} <b>{}</b> <span class=dim>{}</span>{}</div>", if r.red { " red" } else { "" }, esc(&r.what), r.n, esc(&r.why), if r.red { " ●" } else { "" })).collect();
    b.push_str(&section("refused", "Refused (1 h)", &refused_n.to_string(), false, &format!("<div class=card>{}</div>", if refused.is_empty() { "<span class=\"small dim\">none</span>".into() } else { refused })));
    let holds: String = v.holds.iter().map(|h| format!("<div class=row><b class=\"{}\">{}</b> {}<span class=\"small dim\">{}</span></div>", if h.kind == "poison" { "red" } else { "amber" }, esc(&h.kind), h.count, h.top.iter().map(|(n, r)| format!(" · {n}× {}", linked(r))).collect::<String>())).collect();
    b.push_str(&section("holds", "Holds", &v.holds.iter().map(|h| h.count).sum::<usize>().to_string(), false, &format!("<div class=card>{}</div>", if holds.is_empty() { "<span class=\"small dim\">none</span>".into() } else { holds })));
    b.push_str(&section("recent", "Recent", &v.recent.len().to_string(), false, &format!("<div class=card>{}</div>", items(&v.recent))));
    b
}

pub fn main_html(p: &Page) -> String {
    let v = p.view;
    format!("{}{}{}{}{}{}{}", attention(p), rounds(p), in_flight(p), aeons(p), pipeline(p), epics(v), lists(v))
}

fn footer(v: &View) -> String {
    format!(
        "<footer>snapshot {} · gather {} ms · {} source failure{} since the pane started · release <span title=\"{r}\">{r}</span> · <a href=\"/api/lifecycle\">raw JSON</a> · <a href=\"/api/ops\">ops JSON</a> · <a href=\"/lifecycle\">old page</a></footer>",
        esc(&v.clock),
        v.gather_ms,
        v.failures,
        if v.failures == 1 { "" } else { "s" },
        r = esc(&v.release)
    )
}

pub const TABS: [(&str, &str); 5] = [("overview", "Overview"), ("rounds", "Rounds"), ("aeons", "Aeons"), ("stuck", "Stuck"), ("units", "Units")];

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
        "<!doctype html><html lang=en><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\"><title>Spira</title><style>{CSS}</style><body>{}{}<main id=main>{}</main>{}<script>{JS}</script></body></html>",
        header(p),
        tabs("overview"),
        main_html(p),
        footer(p.view)
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
        assert!(head.contains("049592e") && head.contains("aeons 0/4") && !head.contains("prog"), "{head}");
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
        assert_eq!(html.matches("class=prog").count(), 1, "one bar for one round: {html}");
        assert!(html.contains("99/502"), "the live pass count wins: {html}");
        assert!(!html.contains("95/502"), "the collector's older count is not drawn a second time: {html}");
    }

    #[test]
    fn asks_are_listed_in_full_and_absence_is_not_an_alarm() {
        let quiet = page_for(&Snapshot { now: 1_000, ..Default::default() }, "SP_AT='995'\nSP_INFLIGHT_SEG='⚙ idle'\nSP_INFLIGHT_WORLD='running'\n");
        assert!(!quiet.contains("id=attention"), "nothing needs him, so no attention section: {quiet}");
        let dark = page_for(&Snapshot { now: 1_000, ..Default::default() }, "SP_AT='10'\n");
        assert!(dark.contains("Work in flight unreadable"), "positive control: an unreadable read is an alarm, not quiet: {dark}");
    }

    #[test]
    fn bead_ids_in_text_become_links() {
        let h = linked("waits on sp-q75rm0 (master key) and sp-1.2.");
        assert!(h.contains("href=\"/beads/sp-q75rm0\"") && h.contains("href=\"/beads/sp-1.2\"") && h.ends_with("."), "{h}");
        assert_eq!(linked("no ids here"), "no ids here");
    }
}
