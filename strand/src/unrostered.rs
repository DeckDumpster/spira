//! A claimable bead no summonable persona can claim: its labels match no rostered persona's
//! partition, so the pool never counts it and no strand report names it.

use std::collections::HashSet;

use crate::classify::Store;
use crate::config::Config;
use crate::model::{Disposition, Row};

pub const PART: &str = "unrostered";

#[derive(Debug, Clone)]
pub struct Persona {
    pub name: String,
    pub labels: Vec<String>,
    pub exclude: Vec<String>,
    pub rostered: bool,
    pub summon: String,
    pub lane: String,
}

impl Persona {
    fn matches(&self, labels: &[String]) -> bool {
        !self.labels.is_empty()
            && self.labels.iter().all(|l| labels.contains(l))
            && !self.exclude.iter().any(|x| labels.contains(x))
    }

    fn absent_because(&self, lanes: &[String]) -> Option<String> {
        if !self.rostered {
            return Some("not in spira.fayths".into());
        }
        if self.summon != "auto" {
            return Some(format!("FAYTH_SUMMON={}", self.summon));
        }
        if !self.lane.is_empty() && !lanes.contains(&self.lane) {
            return Some(format!("lane {} not in spira.lanes", self.lane));
        }
        None
    }
}

pub fn rows(claimable: &HashSet<String>, store: &Store, personas: &[Persona], lanes: &[String], shared_exclude: &[String]) -> Vec<Row> {
    let mut ids: Vec<&String> = claimable.iter().collect();
    ids.sort();
    let mut out = Vec::new();
    for id in ids {
        let Some(b) = store.get(id) else { continue };
        if b.is_closed() || b.is_epic() || b.status == crate::model::Status::Deferred {
            continue;
        }
        if shared_exclude.iter().any(|x| b.has(x)) {
            continue;
        }
        if b.blocks_targets().any(|t| store.get(t).is_some_and(|x| !x.is_closed())) {
            continue;
        }
        let matching: Vec<&Persona> = personas.iter().filter(|p| p.matches(&b.labels)).collect();
        if matching.iter().any(|p| p.absent_because(lanes).is_none()) {
            continue;
        }
        let detail = if matching.is_empty() {
            "READY but no persona in the chamber matches its labels".to_string()
        } else {
            let why: Vec<String> =
                matching.iter().map(|p| format!("{} ({})", p.name, p.absent_because(lanes).unwrap_or_default())).collect();
            format!("READY but no rostered persona can claim it; would match: {}", why.join(", "))
        };
        out.push(Row::new(
            "unrostered",
            id,
            Disposition::Escalate,
            detail,
            "roster the persona (spira-config set spira.fayths) or relabel the bead".into(),
        ));
    }
    out
}

pub fn personas(cfg: &Config) -> Result<Vec<Persona>, String> {
    use spira_config::chamber::{fayth_get, fayth_names, spira_fayths};
    let home = cfg.home.as_ref().ok_or("neither SPIRA_HOME nor SPIRA_RELEASE is set")?;
    let roster: HashSet<String> =
        spira_fayths(home, cfg.fayths_override.as_deref()).split_whitespace().map(str::to_string).collect();
    let split = |s: String| -> Vec<String> { s.split(',').map(str::trim).filter(|x| !x.is_empty()).map(str::to_string).collect() };
    Ok(fayth_names(home)
        .into_iter()
        .map(|n| Persona {
            labels: split(fayth_get(home, &n, "FAYTH_LABELS", "")),
            exclude: split(fayth_get(home, &n, "FAYTH_EXCLUDE_LABELS", "")),
            rostered: roster.contains(&n),
            summon: fayth_get(home, &n, "FAYTH_SUMMON", "auto"),
            lane: fayth_get(home, &n, "FAYTH_LANE", ""),
            name: n,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::parse_beads;

    fn store(json: &str) -> Store {
        Store::new(parse_beads(json).unwrap())
    }
    fn persona(name: &str, label: &str, rostered: bool) -> Persona {
        Persona { name: name.into(), labels: vec![label.into()], exclude: vec![], rostered, summon: "auto".into(), lane: String::new() }
    }
    fn one(id: &str) -> HashSet<String> {
        [id.to_string()].into()
    }

    #[test]
    fn a_ready_bead_for_an_unrostered_persona_is_reported_naming_it() {
        let s = store(r#"[{"id":"sp-a","status":"open","labels":["warden-sweep"]}]"#);
        let ps = [persona("builder", "plan", true), persona("warden", "warden-sweep", false)];
        let r = rows(&one("sp-a"), &s, &ps, &[], &[]);
        assert_eq!(r.len(), 1);
        assert!(r[0].detail.contains("warden (not in spira.fayths)"), "{}", r[0].detail);
    }

    #[test]
    fn a_rostered_match_is_not_reported() {
        let s = store(r#"[{"id":"sp-a","status":"open","labels":["warden-sweep"]}]"#);
        let ps = [persona("warden", "warden-sweep", true)];
        assert!(rows(&one("sp-a"), &s, &ps, &[], &[]).is_empty());
    }

    #[test]
    fn a_lane_persona_in_an_undeclared_lane_or_a_non_auto_summon_is_named() {
        let s = store(r#"[{"id":"sp-a","status":"open","labels":["x"]},{"id":"sp-b","status":"open","labels":["y"]}]"#);
        let mut lane = persona("laned", "x", true);
        lane.lane = "nolane".into();
        let mut manual = persona("manual", "y", true);
        manual.summon = "manual".into();
        let both: HashSet<String> = ["sp-a".to_string(), "sp-b".to_string()].into();
        let r = rows(&both, &s, &[lane, manual], &[], &[]);
        assert!(r[0].detail.contains("lane nolane not in spira.lanes"), "{}", r[0].detail);
        assert!(r[1].detail.contains("FAYTH_SUMMON=manual"), "{}", r[1].detail);
    }

    #[test]
    fn a_bead_no_persona_matches_is_reported_and_a_blocked_one_is_not() {
        let s = store(
            r#"[{"id":"sp-a","status":"open","labels":["orphan"]},
                {"id":"sp-b","status":"open","labels":["orphan"],"dependencies":[{"depends_on_id":"sp-a","type":"blocks"}]}]"#,
        );
        let both: HashSet<String> = ["sp-a".to_string(), "sp-b".to_string()].into();
        let r = rows(&both, &s, &[persona("builder", "plan", true)], &[], &[]);
        assert!(r.iter().any(|x| x.id == "sp-a" && x.detail.contains("no persona in the chamber")));
        assert_eq!(r.len(), 1, "sp-b waits on open sp-a");
    }
}
