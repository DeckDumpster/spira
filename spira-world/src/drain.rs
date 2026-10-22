//! The drain stamp: who drained, until what, and the checks that page about it.
//!
//! A drain lifts only on `resume`. A cap and a dead owner are reported, never acted on.

use std::path::{Path, PathBuf};

pub const PAGED_CAP: &str = "cap";
pub const PAGED_ORPHAN: &str = "orphan";

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Drain {
    pub owner: String,
    pub until: String,
    pub owner_pid: Option<u32>,
    pub owner_unit: Option<String>,
    pub cap_secs: Option<u64>,
    pub since_epoch: Option<u64>,
}

impl Drain {
    pub fn parse(text: &str) -> Drain {
        let field = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)).and_then(|v| v.strip_prefix(' ')).map(|v| v.trim().to_string());
        Drain {
            owner: field("owner").unwrap_or_default(),
            until: field("until").unwrap_or_default(),
            owner_pid: field("owner-pid").and_then(|v| v.parse().ok()),
            owner_unit: field("owner-unit").filter(|v| !v.is_empty()),
            cap_secs: field("cap").and_then(|v| v.parse().ok()),
            since_epoch: field("since-epoch").and_then(|v| v.parse().ok()),
        }
    }

    pub fn render(&self) -> String {
        let mut s = format!("owner {}\nuntil {}\n", self.owner, self.until);
        if let Some(p) = self.owner_pid {
            s.push_str(&format!("owner-pid {p}\n"));
        }
        if let Some(u) = &self.owner_unit {
            s.push_str(&format!("owner-unit {u}\n"));
        }
        if let Some(c) = self.cap_secs {
            s.push_str(&format!("cap {c}\n"));
        }
        if let Some(e) = self.since_epoch {
            s.push_str(&format!("since-epoch {e}\n"));
        }
        s
    }

    pub fn age_secs(&self, now: u64) -> Option<u64> {
        self.since_epoch.map(|s| now.saturating_sub(s))
    }

    pub fn has_owner_handle(&self) -> bool {
        self.owner_pid.is_some() || self.owner_unit.is_some()
    }

    pub fn describe(&self, now: u64) -> String {
        let age = self.age_secs(now).map(|a| format!("{}m", a / 60)).unwrap_or_else(|| "?".into());
        let cap = self.cap_secs.map(|c| format!("{}m", c / 60)).unwrap_or_else(|| "none".into());
        format!(
            "owner {} · until {} · in force {age} · cap {cap}",
            if self.owner.is_empty() { "(unrecorded)" } else { &self.owner },
            if self.until.is_empty() { "world.sh resume" } else { &self.until },
        )
    }
}

pub fn paged_marker(stamp: &Path) -> PathBuf {
    let mut p = stamp.as_os_str().to_owned();
    p.push(".paged");
    PathBuf::from(p)
}

fn already_paged(stamp: &Path, kind: &str) -> bool {
    std::fs::read_to_string(paged_marker(stamp)).map(|t| t.lines().any(|l| l == kind)).unwrap_or(false)
}

fn record_paged(stamp: &Path, kind: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(paged_marker(stamp)) {
        let _ = writeln!(f, "{kind}");
    }
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    NotDraining,
    Quiet,
    PagedCap,
    PagedOrphan,
    PageFailed,
}

/// Pages once per condition and never touches the stamp. `owner_alive` is `None` when the
/// stamp names no owner pid or unit.
pub fn check(
    stamp: &Path,
    now: u64,
    owner_alive: impl Fn(&Drain) -> Option<bool>,
    stragglers: &[String],
    mut page: impl FnMut(&str, &str) -> bool,
) -> Outcome {
    let Ok(text) = std::fs::read_to_string(stamp) else { return Outcome::NotDraining };
    let d = Drain::parse(&text);
    let held = format!("{}\nStill live: {}", d.describe(now), list(stragglers));

    if owner_alive(&d) == Some(false) && !already_paged(stamp, PAGED_ORPHAN) {
        let body = format!(
            "The drain's owner is gone and nothing has lifted it.\n\n{held}\n\nThe drain stays in force: summons are gated. Decide, then lift with: world.sh resume\n"
        );
        return if page("ORPHANED DRAIN: owner is dead, drain still in force", &body) {
            record_paged(stamp, PAGED_ORPHAN);
            Outcome::PagedOrphan
        } else {
            Outcome::PageFailed
        };
    }

    let over = matches!((d.cap_secs, d.age_secs(now)), (Some(c), Some(a)) if a >= c);
    if over && !already_paged(stamp, PAGED_CAP) {
        let body = format!(
            "A drain has been in force past its cap. The cap pages; it does not lift.\n\n{held}\n\nThe drain stays in force: summons are gated. Lift with: world.sh resume\n"
        );
        return if page("DRAIN past its cap — still in force", &body) {
            record_paged(stamp, PAGED_CAP);
            Outcome::PagedCap
        } else {
            Outcome::PageFailed
        };
    }
    Outcome::Quiet
}

fn list(v: &[String]) -> String {
    if v.is_empty() {
        "none".into()
    } else {
        v.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp_with(tag: &str, d: &Drain) -> (testkit::TempDir, PathBuf) {
        let dir = testkit::TempDir::new(tag);
        let p = dir.join("world.draining");
        std::fs::write(&p, format!("then\ngated\nholds until resume\n{}", d.render())).unwrap();
        (dir, p)
    }

    fn drain() -> Drain {
        Drain {
            owner: "concierge".into(),
            until: "podman storage copy done".into(),
            owner_pid: Some(4242),
            owner_unit: None,
            cap_secs: Some(1800),
            since_epoch: Some(1000),
        }
    }

    #[test]
    fn a_stamp_round_trips_through_render_and_parse() {
        assert_eq!(Drain::parse(&drain().render()), drain());
    }

    #[test]
    fn a_drain_past_its_cap_pages_once_naming_owner_and_stragglers_and_leaves_the_stamp() {
        let (_dir, p) = stamp_with("cap", &drain());
        let mut pages = Vec::new();
        let live = vec!["sp-abc".to_string()];
        let alive = |_: &Drain| Some(true);
        assert_eq!(check(&p, 1000 + 1799, alive, &live, |s, b| { pages.push((s.into(), b.into())); true }), Outcome::Quiet);
        assert!(pages.is_empty(), "positive control: under the cap nothing is paged");
        let got = check(&p, 1000 + 4000, alive, &live, |s, b| { pages.push((s.to_string(), b.to_string())); true });
        assert_eq!(got, Outcome::PagedCap);
        let (subject, body): &(String, String) = &pages[0];
        assert!(subject.contains("past its cap") && body.contains("concierge") && body.contains("sp-abc"), "{body}");
        assert!(body.contains("podman storage copy done") && body.contains("in force 66m"), "{body}");
        assert_eq!(check(&p, 1000 + 9000, alive, &live, |_, _| panic!("a second page")), Outcome::Quiet);
        assert!(p.is_file(), "the cap never lifts the drain");
    }

    #[test]
    fn a_dead_owner_is_reported_as_orphaned_once_and_the_drain_stays() {
        let (_dir, p) = stamp_with("orphan", &drain());
        let mut pages = Vec::new();
        let got = check(&p, 1010, |_| Some(false), &[], |s, b| { pages.push((s.to_string(), b.to_string())); true });
        assert_eq!(got, Outcome::PagedOrphan);
        assert!(pages[0].0.contains("ORPHANED") && pages[0].1.contains("concierge"), "{pages:?}");
        assert_eq!(check(&p, 1020, |_| Some(false), &[], |_, _| panic!("a second page")), Outcome::Quiet);
        assert!(p.is_file());
    }

    #[test]
    fn a_failed_page_is_retried_not_recorded() {
        let (_dir, p) = stamp_with("retry", &drain());
        assert_eq!(check(&p, 99_999, |_| None, &[], |_, _| false), Outcome::PageFailed);
        assert_eq!(check(&p, 99_999, |_| None, &[], |_, _| true), Outcome::PagedCap);
    }

    #[test]
    fn no_stamp_is_not_draining() {
        assert_eq!(check(Path::new("/nonexistent/world.draining"), 5, |_| None, &[], |_, _| true), Outcome::NotDraining);
    }
}
