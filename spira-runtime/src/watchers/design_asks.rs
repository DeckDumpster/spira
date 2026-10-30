//! DESIGN WITHOUT ASK for the Concierge inbox: a design page in draft or in-review that no open
//! ask names, told again every `repeat_secs` until an ask exists or the status changes. Each tick
//! asks the world for the pages and the text of every open ask; the decision is `decide`, which
//! does no IO.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{iso8601, INBOX_APPEND};

pub const STREAM: &str = "watch.design-asks";
pub const TICK: EventKindTag = "design-asks.tick";
pub const STATE: EventKindTag = "design-asks.state";
pub const OBSERVE: &str = "design-asks.observe";
pub const NAME: &str = "design-asks";

const DEADLINE_MS: u64 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesignAsks {
    pub interval_ms: u64,
    pub repeat_secs: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub rel: String,
    pub status: String,
    pub writer: String,
    pub urls: Vec<String>,
}

/// `failure` is set when the brain or the ask store could not be read; `asks` is then empty.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub failure: Option<String>,
    pub asks: String,
    pub pages: Vec<Page>,
}

/// `since` maps a page to (first uncovered, last told), both epoch seconds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub since: BTreeMap<String, (u64, u64)>,
    pub failed: Option<String>,
}

fn asked(page: &Page, asks: &str) -> bool {
    let slug = page.rel.rsplit('/').next().unwrap_or("");
    let slug = slug.strip_suffix(".md").unwrap_or(slug);
    asks.contains(&page.rel) || asks.contains(slug) || page.urls.iter().any(|u| asks.contains(u.as_str()))
}

pub fn decide(cfg: &DesignAsks, carry: &Carry, snap: &Snapshot, now: u64) -> (Carry, Vec<String>) {
    if let Some(why) = &snap.failure {
        let said = carry.failed.as_ref() != Some(why);
        let next = Carry { failed: Some(why.clone()), ..carry.clone() };
        return (next, if said { vec![format!("DESIGN-ASKS FAILED: {why}; designs without an ask cannot be listed")] } else { vec![] });
    }
    let mut since = BTreeMap::new();
    let mut say = vec![];
    for p in &snap.pages {
        if !matches!(p.status.as_str(), "draft" | "in-review") || asked(p, &snap.asks) {
            continue;
        }
        let (first, mut last) = carry.since.get(&p.rel).copied().unwrap_or((now, 0));
        if now.saturating_sub(last) >= cfg.repeat_secs {
            let writer = if p.writer.is_empty() { "unknown" } else { p.writer.as_str() };
            say.push(format!("DESIGN WITHOUT ASK: {} is {} and has had no open ask for {}h (written by {writer}); post one with work ask --class design", p.rel, p.status, now.saturating_sub(first) / 3600));
            last = now;
        }
        since.insert(p.rel.clone(), (first, last));
    }
    (Carry { since, failed: None }, say)
}

impl DesignAsks {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("design-asks/boot", STREAM, EventKind::new(TICK, ""))
    }

    fn carry(view: &Projection, stream: &StreamKey) -> Carry {
        view.events(stream).iter().rev().find(|e| e.kind.tag == STATE).and_then(|e| serde_json::from_str(&e.kind.body).ok()).unwrap_or_default()
    }

    fn answers_observe(view: &Projection, ev: &Event) -> bool {
        let Some(cause) = &ev.cause else { return false };
        view.events(&ev.stream).iter().rev().find(|e| &e.id == cause && e.kind.tag == EFFECT_REQUESTED).and_then(|e| serde_json::from_str::<serde_json::Value>(&e.kind.body).ok()).is_some_and(|b| b["spec"]["name"] == OBSERVE)
    }
}

impl Handler for DesignAsks {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return vec![
                Command::Probe(EffectSpec { name: OBSERVE.into(), input: String::new(), deadline_ms: DEADLINE_MS }),
                Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") },
            ];
        }
        if ev.kind.tag != EFFECT_SUCCEEDED || !Self::answers_observe(view, ev) {
            return vec![];
        }
        let Ok(snap) = serde_json::from_str::<Snapshot>(&ev.kind.body) else { return vec![] };
        let now = ev.at / 1000;
        let carry = Self::carry(view, &ev.stream);
        let (next, say) = decide(self, &carry, &snap, now);
        ctx.event().set("design_asks.said", say.len() as u64);
        let mut cmds = vec![];
        if next != carry {
            cmds.push(Command::Emit(EventKind::new(STATE, &serde_json::to_string(&next).unwrap_or_default())));
        }
        for m in say {
            cmds.push(Command::Effect(EffectSpec { name: INBOX_APPEND.into(), input: format!("{} [watch:{NAME}] {m}", iso8601(now)), deadline_ms: DEADLINE_MS }));
        }
        cmds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> DesignAsks {
        DesignAsks { interval_ms: 600_000, repeat_secs: 7200 }
    }

    fn page(rel: &str, status: &str) -> Page {
        Page { rel: rel.into(), status: status.into(), writer: "sp-w".into(), urls: vec![] }
    }

    fn snap(pages: Vec<Page>, asks: &str) -> Snapshot {
        Snapshot { failure: None, asks: asks.into(), pages }
    }

    const T0: u64 = 1_800_000_000;
    const REL: &str = "wiki/projects/zed/designs/alpha.md";

    #[test]
    fn an_uncovered_draft_is_told_once_per_repeat_window_with_its_age() {
        let s = snap(vec![page(REL, "draft")], "");
        let (c, say) = decide(&cfg(), &Carry::default(), &s, T0);
        assert_eq!(say, [format!("DESIGN WITHOUT ASK: {REL} is draft and has had no open ask for 0h (written by sp-w); post one with work ask --class design")]);
        assert!(decide(&cfg(), &c, &s, T0 + 7199).1.is_empty());
        let again = decide(&cfg(), &c, &s, T0 + 3 * 3600 + 7200).1;
        assert!(again[0].contains("no open ask for 5h"), "{again:?}");
    }

    #[test]
    fn a_page_is_covered_by_path_slug_or_artifact_url_and_only_draft_or_review_counts() {
        let mut with_url = page(REL, "in-review");
        with_url.urls = vec!["https://claude.ai/artifact/AAAA".into()];
        assert!(decide(&cfg(), &Carry::default(), &snap(vec![with_url.clone()], "see https://claude.ai/artifact/AAAA"), T0).1.is_empty());
        assert_eq!(decide(&cfg(), &Carry::default(), &snap(vec![with_url], "https://claude.ai/artifact/BBBB"), T0).1.len(), 1);
        assert!(decide(&cfg(), &Carry::default(), &snap(vec![page(REL, "draft")], &format!("approve {REL}")), T0).1.is_empty());
        assert!(decide(&cfg(), &Carry::default(), &snap(vec![page(REL, "draft")], "approve alpha please"), T0).1.is_empty());
        assert!(decide(&cfg(), &Carry::default(), &snap(vec![page(REL, "approved")], ""), T0).1.is_empty());
    }

    #[test]
    fn a_page_that_gains_an_ask_loses_its_clock() {
        let (c, _) = decide(&cfg(), &Carry::default(), &snap(vec![page(REL, "draft")], ""), T0);
        let (c, _) = decide(&cfg(), &c, &snap(vec![page(REL, "draft")], "alpha"), T0 + 1000);
        assert!(c.since.is_empty());
    }

    #[test]
    fn a_failure_is_said_once_and_never_read_as_no_designs() {
        let bad = Snapshot { failure: Some("the brain is gone".into()), ..Snapshot::default() };
        let (c, say) = decide(&cfg(), &Carry::default(), &bad, T0);
        assert_eq!(say, ["DESIGN-ASKS FAILED: the brain is gone; designs without an ask cannot be listed"]);
        assert!(decide(&cfg(), &c, &bad, T0 + 1).1.is_empty());
        let (c, _) = decide(&cfg(), &c, &snap(vec![], ""), T0 + 2);
        assert_eq!(decide(&cfg(), &c, &bad, T0 + 3).1.len(), 1, "a recovered watcher says its next failure");
    }

    #[test]
    fn a_failed_tick_keeps_the_clocks_of_pages_already_told() {
        let (c, _) = decide(&cfg(), &Carry::default(), &snap(vec![page(REL, "draft")], ""), T0);
        let bad = Snapshot { failure: Some("x".into()), ..Snapshot::default() };
        let (c, _) = decide(&cfg(), &c, &bad, T0 + 100);
        assert!(decide(&cfg(), &c, &snap(vec![page(REL, "draft")], ""), T0 + 200).1.is_empty());
    }
}
