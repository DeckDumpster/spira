//! The layout every Loom page shares: top bar, seven-section navigation, one stylesheet.

use crate::stuck::esc;

pub const SECTIONS: [(&str, &str, &str); 7] = [
    ("now", "Now", "/"),
    ("dwarves", "Dwarves", "/dwarves"),
    ("rounds", "Rounds", "/rounds"),
    ("stuck", "Stuck", "/stuck"),
    ("units", "Units", "/units"),
    ("states", "States", "/states"),
    ("lifecycle", "Lifecycle", "/lifecycle"),
];

const BAR_SLOT: &str = "<!--lm-bar-->";

const PAGE_CSS: &str = "main{padding:.2rem .8rem}h2{font-size:.95rem;margin:.9rem 0 .3rem}.card{display:block;padding:.5rem .6rem;margin:.3rem 0;background:#161b22;border:1px solid #1d232b;border-radius:4px;overflow-wrap:anywhere}\
body{margin:0;font:14px/1.35 system-ui,sans-serif;background:#10141a;color:#d8dee9}\
header{padding:.6rem .8rem;border-bottom:1px solid #2a323d}\
h1{font-size:1rem;margin:0}.dim{color:#7d8896}a{color:inherit;text-decoration:none}\
.row{display:block;padding:.5rem .8rem;border-bottom:1px solid #1d232b}.row.stuck{background:#1e1416}\
.head{display:flex;gap:.5rem;align-items:baseline;flex-wrap:wrap}.id{font-weight:600}.t{flex:1 1 12rem;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.bar{position:relative;height:16px;margin-top:.3rem;background:#161b22;border-radius:3px}\
.seg{position:absolute;top:0;bottom:0;border-radius:2px;box-sizing:border-box;min-width:3px}\
.seg.over{outline:2px solid #ff4d4f;outline-offset:-1px;z-index:1}\
.s-OPEN{background:#4a5568}.s-READY{background:#3b82c4}.s-WORKING{background:#3aa57a}.s-SUBMITTED{background:#8b6fd0}\
.s-CERTIFIED{background:#d6a032}.s-IN_DELIVERY{background:#d9772b}.s-REWORK{background:#c25a8a}.s-LANDED{background:#2f6f4f}\
.badge{font-size:.75rem;padding:0 .35rem;border-radius:3px;background:#2a323d}.red{color:#ff6b6d}.badge.red{background:#4a1f21}\
table{border-collapse:collapse;width:100%}td{padding:.3rem .5rem;border-bottom:1px solid #1d232b;vertical-align:top;word-break:break-word}\
tr.refused td{color:#ff6b6d}.legend{display:flex;gap:.6rem;flex-wrap:wrap;padding:.4rem .8rem;font-size:.75rem}.legend i{display:inline-block;width:.7rem;height:.7rem;margin-right:.25rem;border-radius:2px}";

const NAV_CSS: &str = ".lm-top{position:sticky;top:0;z-index:5;display:flex;flex-wrap:wrap;gap:.2rem .8rem;align-items:baseline;padding:.4rem .8rem;background:#0b0e13;border-bottom:1px solid #2a323d;font:13px/1.35 system-ui,sans-serif;color:#d8dee9;overflow-wrap:anywhere}\
.lm-top b{letter-spacing:.08em}.lm-top .lm-bad{color:#ff6b6d}.lm-top .lm-dim{color:#7d8896}\
.lm-tabs{display:flex;gap:.2rem;background:#0b0e13;font:14px/1.2 system-ui,sans-serif;padding:0 .8rem;border-bottom:1px solid #2a323d}\
.lm-tabs a{padding:.5rem .9rem;color:#9aa5b1;text-decoration:none;border-bottom:2px solid transparent}\
.lm-tabs a.on{color:#d8dee9;border-bottom-color:#3b82c4}\
@media(max-width:639px){body{overflow-x:hidden;padding-bottom:3.6rem}\
.lm-tabs{position:fixed;left:0;right:0;bottom:0;z-index:6;padding:0;border-top:1px solid #2a323d;border-bottom:0}\
.lm-tabs a{flex:1 1 0;min-width:0;text-align:center;padding:.8rem .1rem;font-size:.8rem;border-bottom:0;border-top:2px solid transparent}\
.lm-tabs a.on{border-top-color:#3b82c4}}";

/// What the top bar says. Every field is read from the lifecycle snapshot the cockpit pane writes.
#[derive(Debug, Clone, Default)]
pub struct Bar {
    pub release: String,
    pub world: String,
    pub dwarves: Option<(usize, usize)>,
    pub age_s: Option<i64>,
}

pub fn tabs(active: &str) -> String {
    let mut t = String::from("<nav class=lm-tabs>");
    for (key, label, href) in SECTIONS {
        t.push_str(&format!("<a href=\"{href}\"{}>{label}</a>", if key == active { " class=on aria-current=page" } else { "" }));
    }
    t.push_str("</nav>");
    t
}

pub fn bar_html(b: &Bar) -> String {
    let dash = |s: &str| if s.is_empty() { "?".to_string() } else { esc(s) };
    let dwarves = b.dwarves.map_or("?".to_string(), |(n, cap)| format!("{n}/{cap}"));
    let age = match b.age_s {
        Some(a) if a > crate::LIFECYCLE_STALE_S => format!("<span class=lm-bad>stale {}</span>", crate::stuck::dur(a)),
        Some(a) => format!("{} ago", crate::stuck::dur(a)),
        None => "<span class=lm-bad>no snapshot</span>".to_string(),
    };
    format!(
        "<div class=lm-top><b>SPIRA</b><span>{}</span><span>world {}</span><span>dwarves {dwarves}</span><span class=lm-dim>refreshed {age}</span></div>",
        dash(&b.release),
        dash(&b.world),
    )
}

/// A full page: the bar slot, the tabs, then `body`. Fill the slot with `with_bar`.
pub fn shell(active: &str, title: &str, refresh_s: u32, body: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\"><meta http-equiv=refresh content={refresh_s}><title>{} · Loom</title><style>{PAGE_CSS}{NAV_CSS}</style><body>{BAR_SLOT}{}{body}</body>",
        esc(title),
        tabs(active),
    )
}

pub fn with_bar(html: &str, bar: &Bar) -> String {
    html.replacen(BAR_SLOT, &bar_html(bar), 1)
}

/// A page rendered elsewhere (the lifecycle view) put inside the shell: bar and tabs after its `<body>`.
pub fn wrap_foreign(html: &str, active: &str, bar: &Bar) -> String {
    let head = format!("<style>{NAV_CSS}</style>{}{}", bar_html(bar), tabs(active));
    match html.find("<body>") {
        Some(i) => format!("{}<body>{head}{}", &html[..i], &html[i + 6..]),
        None => format!("{head}{html}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_is_one_tab_and_only_the_active_one_is_marked() {
        let t = tabs("stuck");
        for (_, label, href) in SECTIONS {
            assert!(t.contains(&format!("<a href=\"{href}\"")) && t.contains(&format!(">{label}</a>")), "{t}");
        }
        assert_eq!(t.matches("aria-current").count(), 1);
        assert!(t.contains("<a href=\"/stuck\" class=on"));
        assert_eq!(tabs("nowhere").matches("aria-current").count(), 0, "positive control: an unknown section marks nothing");
    }

    #[test]
    fn the_bar_names_what_it_does_not_know_and_flags_an_old_snapshot() {
        let blank = bar_html(&Bar::default());
        assert!(blank.contains("no snapshot") && blank.contains("dwarves ?"), "{blank}");
        let old = bar_html(&Bar { release: "abc".into(), world: "RUNNING".into(), dwarves: Some((2, 4)), age_s: Some(600) });
        assert!(old.contains("stale 10m") && old.contains("dwarves 2/4") && old.contains("abc"), "{old}");
    }

    #[test]
    fn a_foreign_page_gets_the_tabs_inside_its_body() {
        let w = wrap_foreign("<html><body><p>x</p></body></html>", "lifecycle", &Bar::default());
        assert!(w.find("<body><style>").is_some() && w.find("lm-tabs").unwrap() < w.find("<p>x</p>").unwrap(), "{w}");
    }
}
