//! The Now page and the round pages. Everything here is a pure function of the lifecycle
//! snapshot (and, for units, the held units snapshot); the routes do the reading.

use crate::shell::{shell, Bar};
use crate::stuck::{dur, esc};
use crate::units;
use cockpit_ops::lcview::{self, BatchRow, Snapshot, View};

const TOP: usize = 3;
const LANDED_SHOWN: usize = 5;
const DAY_S: i64 = 86_400;

pub fn aeons(snap: &Snapshot) -> usize {
    snap.live.as_ref().map_or_else(|| snap.rows.iter().filter(|r| r.state == "WORKING").count(), Vec::len)
}

pub fn bar(snap: Option<&Snapshot>, now: i64) -> Bar {
    snap.map_or_else(Bar::default, |s| Bar {
        release: s.release.clone(),
        world: s.world.clone(),
        aeons: Some((aeons(s), s.ceiling)),
        age_s: Some(now - s.now),
    })
}

/// What the unit section knows: nothing yet (first read still running), or a snapshot of some age.
pub enum UnitsState<'a> {
    Pending,
    Held { snap: &'a units::Snapshot, age_s: i64, stale: bool },
}

fn card(r: &lcview::RepoRound) -> String {
    let progress = if r.total > 0 { format!(" · {}/{} suites", r.done, r.total) } else { String::new() };
    let red = if r.red > 0 { format!(" · <span class=red>{} red</span>", r.red) } else { String::new() };
    let stale = if r.stale { " <span class=\"badge red\">STALE</span>" } else { "" };
    format!(
        "<a class=card href=\"/rounds/{repo}/{batch}\"><div class=head><span class=id>{repo}</span><span class=dim>{kind}</span>{stale}</div><div>{batch} · {phase}{progress}{red}</div><div class=dim>{n} members · {age}</div></a>",
        repo = esc(&r.repo),
        batch = esc(&r.batch),
        kind = esc(&r.kind),
        phase = esc(&r.phase),
        n = r.members.len(),
        age = esc(&r.age),
    )
}

fn heading(label: &str, extra: &str) -> String {
    format!("<h2>{label} <span class=dim>{extra}</span></h2>")
}

pub fn render_now(snap: &Snapshot, view: &View, units: &UnitsState) -> String {
    let now = snap.now;
    let mut b = String::new();

    let live: Vec<_> = view.rounds.iter().collect();
    b.push_str(&heading("Live rounds", &format!("{}", live.len())));
    if live.is_empty() {
        b.push_str("<p class=dim>no round or batch is running</p>");
    }
    for r in live {
        b.push_str(&card(r));
    }

    let mut past: Vec<(i64, &lcview::DwellRow)> = snap
        .dwell
        .iter()
        .filter_map(|d| d.p95_s.map(|p| (now - d.entered_at - p, d)))
        .filter(|(over, _)| *over > 0)
        .collect();
    past.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.bead_id.cmp(&b.1.bead_id)));
    b.push_str(&heading("Past p95", &format!("{} · top {TOP}", past.len())));
    if past.is_empty() {
        b.push_str("<p class=dim>no bead is past its state's p95</p>");
    }
    for (_, d) in past.iter().take(TOP) {
        let title = snap.meta.get(&d.bead_id).map(|m| m.title.as_str()).unwrap_or_default();
        b.push_str(&format!(
            "<a class=card href=\"/stuck/{id}\"><div class=head><span class=id>{id}</span><span class=t>{}</span></div><div><span class=\"badge red\">{} in {} · p95 {}</span></div></a>",
            esc(title),
            dur(now - d.entered_at),
            esc(&d.state),
            dur(d.p95_s.unwrap_or(0)),
            id = esc(&d.bead_id),
        ));
    }

    b.push_str(&heading("Units", "failing or stale · top 3"));
    match units {
        UnitsState::Pending => b.push_str("<p class=dim>unit history is loading — not read yet</p>"),
        UnitsState::Held { snap: u, age_s, stale } => {
            if *stale {
                b.push_str(&format!("<p class=\"badge red\">stale: unit history is {} old, refreshing</p>", dur(*age_s)));
            }
            let mut bad: Vec<&units::Row> = u.rows.iter().filter(|r| r.kind != "system" && matches!(r.verdict, units::Verdict::Failing | units::Verdict::Late)).collect();
            bad.sort_by(|a, b| (a.verdict as u8, &a.unit).cmp(&(b.verdict as u8, &b.unit)));
            if bad.is_empty() {
                b.push_str("<p class=dim>every unit is running to schedule</p>");
            }
            for r in bad.iter().take(TOP) {
                let why = if r.verdict == units::Verdict::Failing { "failing".to_string() } else { format!("late — last success {}", r.last_success.map_or("never".into(), |t| format!("{} ago", dur(u.now - t)))) };
                b.push_str(&format!("<a class=card href=\"/units\"><div class=head><span class=id>{}</span><span class=\"badge red\">{}</span></div></a>", esc(&r.unit), esc(&why)));
            }
        }
    }

    b.push_str(&heading("Aeons", &format!("{}/{}", aeons(snap), snap.ceiling)));
    if view.now_items.is_empty() {
        b.push_str("<p class=dim>no aeon is live</p>");
    }
    for i in &view.now_items {
        b.push_str(&format!(
            "<div class=card><div class=head><span class=id>{}</span><span class=dim>{}</span></div><div class=dim>{}</div></div>",
            esc(&i.id),
            esc(&i.who),
            esc(&i.note),
        ));
    }

    let mut landed: Vec<_> = snap.rows.iter().filter(|r| r.state == "LANDED" && now - r.updated_at < DAY_S).collect();
    landed.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
    b.push_str(&heading("Landed", &format!("{} in 24 h", landed.len())));
    for r in landed.iter().take(LANDED_SHOWN) {
        let title = snap.meta.get(&r.id).map(|m| m.title.as_str()).unwrap_or_default();
        b.push_str(&format!(
            "<a class=card href=\"/stuck/{id}\"><div class=head><span class=id>{id}</span><span class=t>{}</span><span class=dim>{} ago</span></div></a>",
            esc(title),
            dur(now - r.updated_at),
            id = esc(&r.id),
        ));
    }
    if landed.is_empty() {
        b.push_str("<p class=dim>nothing landed in the last 24 h</p>");
    }

    shell("now", "Now", 15, &format!("<header><h1>Now <span class=dim>— {} · world {}</span></h1></header><main>{b}</main>", esc(&snap.release), esc(&snap.world)))
}

pub fn render_rounds(view: &View) -> String {
    let mut b = heading("Rounds", &format!("{} live", view.rounds.len()));
    for r in &view.rounds {
        b.push_str(&card(r));
    }
    if view.rounds.is_empty() {
        b.push_str("<p class=dim>no round or batch is running</p>");
    }
    shell("rounds", "Rounds", 15, &format!("<header><h1>Rounds</h1></header><main>{b}</main>"))
}

pub fn render_round(snap: &Snapshot, b: &BatchRow) -> String {
    let now = snap.now;
    let members = |ids: &[String]| ids.iter().map(|m| format!("<li>{}</li>", esc(m))).collect::<String>();
    let mut body = format!(
        "<header><h1><a href=/rounds>← rounds</a> <span class=dim>/</span> {}</h1><div class=dim>{} · {} · {}</div></header><main>",
        esc(&b.id),
        esc(&b.repo),
        esc(&b.state),
        if b.phase.is_empty() { String::new() } else { esc(&b.phase) },
    );
    if b.pr > 0 && !snap.forge_base.is_empty() && !b.repo.is_empty() {
        let url = format!("{}/{}/pull/{}", snap.forge_base, b.repo, b.pr);
        body.push_str(&format!("<p>forge batch PR <a href=\"{0}\" style=\"text-decoration:underline\">#{1}</a></p>", esc(&url), b.pr));
    } else if b.pr > 0 {
        body.push_str(&format!("<p>forge batch PR #{}</p>", b.pr));
    }
    let opened = if b.opened_at > 0 { b.opened_at } else { b.last_at };
    body.push_str(&format!("<p class=dim>opened {} ago · last change {} ago</p>", dur(now - opened), dur(now - b.last_at)));
    if let Some(p) = &b.progress {
        body.push_str(&format!("<p>{} · {}/{} suites{}</p>", esc(&p.phase), p.done, p.total, if p.red.is_empty() { String::new() } else { format!(" · <span class=red>red: {}</span>", esc(&p.red.join(", "))) }));
    }
    if !b.run_url.is_empty() {
        body.push_str(&format!("<p>failing check: <a href=\"{0}\" style=\"text-decoration:underline\">{0}</a></p>", esc(&b.run_url)));
    }
    let carried: Vec<String> = b.members.iter().filter(|m| !b.ejected.contains(m)).cloned().collect();
    body.push_str(&format!("<h2>Members <span class=dim>{}</span></h2><ul>{}</ul>", carried.len(), members(&carried)));
    if !b.ejected.is_empty() {
        let why: String = b.ejected.iter().map(|m| format!("<li>{} <span class=dim>{}</span></li>", esc(m), esc(snap.eject_why.get(m).map(String::as_str).unwrap_or("no reason recorded")))).collect();
        body.push_str(&format!("<h2>Ejected <span class=dim>{}</span></h2><ul>{why}</ul>", b.ejected.len()));
    }
    body.push_str("</main>");
    shell("rounds", &b.id, 15, &body)
}
