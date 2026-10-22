//! The operator-surface section registry. The ops pane (`lc-view tui`) and loom's `/lifecycle`
//! page and `/api/lifecycle` all draw `lctui::tree` over one `lcview::View`; each top-level
//! section is declared here once, with the `View` field that carries its data. A section a
//! surface does not draw must be listed in `ONE_SURFACE` with the reason, and `violations`
//! fails on any section that exists on one surface and not the other.

use std::collections::BTreeSet;

pub struct Section {
    pub key: &'static str,
    pub view_field: &'static str,
}

pub const SECTIONS: [Section; 12] = [
    Section { key: "decide", view_field: "decide" },
    Section { key: "drift", view_field: "drift" },
    Section { key: "rounds", view_field: "rounds" },
    Section { key: "round", view_field: "round" },
    Section { key: "epics", view_field: "epics" },
    Section { key: "now", view_field: "now_items" },
    Section { key: "inflight", view_field: "inflight" },
    Section { key: "state", view_field: "machine" },
    Section { key: "blocked", view_field: "blocked" },
    Section { key: "refused", view_field: "refused" },
    Section { key: "holds", view_field: "holds" },
    Section { key: "recent", view_field: "recent" },
];

pub struct OneSurface {
    pub key: &'static str,
    pub surface: &'static str,
    pub reason: &'static str,
}

const LEGACY: &str = "drawn only by the legacy `health` binary, which the cockpit no longer runs as the ops pane";

pub const ONE_SURFACE: [OneSurface; 8] = [
    OneSurface { key: "tokens", surface: "health", reason: LEGACY },
    OneSurface { key: "slots", surface: "health", reason: LEGACY },
    OneSurface { key: "next", surface: "health", reason: "folded into state/READY on the lifecycle surfaces" },
    OneSurface { key: "unlanded", surface: "health", reason: LEGACY },
    OneSurface { key: "inflow", surface: "health", reason: LEGACY },
    OneSurface { key: "ci", surface: "health", reason: LEGACY },
    OneSurface { key: "flow", surface: "health", reason: LEGACY },
    OneSurface { key: "standing", surface: "health", reason: LEGACY },
];

/// What is wrong with the sections each surface draws, against the registry and the allowlist.
pub fn violations(registry: &[&str], pane: &BTreeSet<String>, web: &BTreeSet<String>, allow: &[(&str, &str)]) -> Vec<String> {
    let mut out = Vec::new();
    for k in pane.symmetric_difference(web) {
        let (on, off) = if pane.contains(k) { ("pane", "web") } else { ("web", "pane") };
        out.push(format!("section {k} is on the {on} and not the {off}"));
    }
    for k in pane.union(web) {
        if !registry.contains(&k.as_str()) {
            out.push(format!("section {k} is drawn but not in the registry"));
        }
    }
    for k in registry {
        if !pane.contains(*k) && !web.contains(*k) {
            out.push(format!("section {k} is in the registry and on neither surface"));
        }
    }
    for (k, reason) in allow {
        if reason.trim().is_empty() {
            out.push(format!("allowlisted section {k} has no reason"));
        }
        if registry.contains(k) {
            out.push(format!("allowlisted section {k} is also in the registry"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lcview::{HoldGroup, Inflight, Item, RepoRound, SmRefusal, View};
    use crate::lctui::{layout, render_fragment, Ui};

    fn item() -> Item {
        Item { id: "sp-1".into(), ..Default::default() }
    }

    fn full_view(rounds: bool) -> View {
        let mut v = View { now: 1000, ..Default::default() };
        v.decide = vec![item()];
        v.drift = vec!["sp-d".into()];
        v.epics = vec![crate::lcview::EpicRow { repo: "r".into(), epic: "e".into(), landed: 1, ..Default::default() }];
        v.now_items = vec![item()];
        v.inflight = Inflight { seg: "\u{2699} 1 aeon".into(), rows: vec!["aeon|sp-1|guardian|900||".into()] };
        v.machine = vec![crate::lcview::SmState { name: "READY".into(), ..Default::default() }];
        v.blocked = vec![item()];
        v.refused = vec![SmRefusal { what: "w".into(), n: 1, ..Default::default() }];
        v.holds = vec![HoldGroup { kind: "manual".into(), count: 1, top: vec![] }];
        v.recent = vec![item()];
        if rounds {
            v.rounds = vec![RepoRound { repo: "r".into(), batch: "b".into(), ..Default::default() }];
        }
        v
    }

    fn top(keys: impl Iterator<Item = String>) -> BTreeSet<String> {
        keys.filter(|k| !k.contains('/')).collect()
    }

    fn pane_keys(v: &View) -> BTreeSet<String> {
        top(layout(v, &Ui::default(), 200, 10_000).all_keys.into_iter())
    }

    fn web_keys(v: &View) -> BTreeSet<String> {
        let frag: serde_json::Value = serde_json::from_str(&render_fragment(v, None)).unwrap();
        let tree = frag["tree"].as_str().unwrap();
        top(tree.split("data-key=\"").skip(1).filter_map(|p| p.split('"').next()).map(String::from))
    }

    fn registry() -> Vec<&'static str> {
        SECTIONS.iter().map(|s| s.key).collect()
    }

    fn allow() -> Vec<(&'static str, &'static str)> {
        ONE_SURFACE.iter().map(|o| (o.key, o.reason)).collect()
    }

    fn both(f: impl Fn(&View) -> BTreeSet<String>) -> BTreeSet<String> {
        f(&full_view(true)).union(&f(&full_view(false))).cloned().collect()
    }

    #[test]
    fn the_pane_and_the_page_draw_exactly_the_registry() {
        let (pane, web) = (both(pane_keys), both(web_keys));
        assert_eq!(violations(&registry(), &pane, &web, &allow()), Vec::<String>::new());
    }

    #[test]
    fn a_section_on_one_surface_only_is_a_violation() {
        let (pane, web) = (both(pane_keys), both(web_keys));
        let mut extra = pane.clone();
        extra.insert("planted".into());
        let v = violations(&registry(), &extra, &web, &allow());
        assert!(v.iter().any(|m| m.contains("planted is on the pane and not the web")), "{v:?}");
        let mut extra = web.clone();
        extra.insert("planted".into());
        let v = violations(&registry(), &pane, &extra, &allow());
        assert!(v.iter().any(|m| m.contains("planted is on the web and not the pane")), "{v:?}");
        let v = violations(&registry(), &pane, &web, &[("tokens", "")]);
        assert!(v.iter().any(|m| m.contains("no reason")), "{v:?}");
    }

    #[test]
    fn every_section_names_a_view_field_the_api_serves() {
        let api = serde_json::to_value(full_view(true)).unwrap();
        for s in &SECTIONS {
            assert!(api.get(s.view_field).is_some(), "{} has no field {} in /api/lifecycle", s.key, s.view_field);
        }
    }

    #[test]
    fn work_in_flight_is_drawn_on_both_surfaces_and_unread_is_not_idle() {
        let v = full_view(true);
        let lines: Vec<String> = layout(&v, &Ui::default(), 200, 10_000).lines.iter().map(|l| crate::lctui::strip_for_test(l)).collect();
        assert!(lines.iter().any(|l| l.contains("IN FLIGHT") && l.contains("1 aeon")), "{lines:#?}");
        assert!(render_fragment(&v, None).contains("sp-1"));
        let unread = View { now: 1000, ..Default::default() };
        let lines: Vec<String> = layout(&unread, &Ui::default(), 200, 10_000).lines.iter().map(|l| crate::lctui::strip_for_test(l)).collect();
        assert!(lines.iter().any(|l| l.contains("work in flight unread")) && !lines.iter().any(|l| l.contains("idle")), "{lines:#?}");
    }
}
