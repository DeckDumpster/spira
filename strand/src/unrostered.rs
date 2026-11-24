//! A claimable bead no callable persona can claim: its labels match no rostered persona's
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
    pub call: String,
}

impl Persona {
    fn matches(&self, labels: &[String]) -> bool {
        !self.labels.is_empty()
            && self.labels.iter().all(|l| labels.contains(l))
            && !self.exclude.iter().any(|x| labels.contains(x))
    }

    fn absent_because(&self) -> Option<String> {
        if !self.rostered {
            return Some("not in spira.runes".into());
        }
        if self.call != "auto" {
            return Some(format!("RUNE_CALL={}", self.call));
        }
        None
    }
}

pub fn rows(
    claimable: &HashSet<String>,
    store: &Store,
    personas: &[Persona],
    shared_exclude: &[String],
) -> Vec<Row> {
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
        if b.blocks_targets()
            .any(|t| store.get(t).is_some_and(|x| !x.is_closed()))
        {
            continue;
        }
        let matching: Vec<&Persona> = personas.iter().filter(|p| p.matches(&b.labels)).collect();
        if matching.iter().any(|p| p.absent_because().is_none()) {
            continue;
        }
        let detail = if matching.is_empty() {
            "READY but no persona in the runes matches its labels".to_string()
        } else {
            let why: Vec<String> = matching
                .iter()
                .map(|p| format!("{} ({})", p.name, p.absent_because().unwrap_or_default()))
                .collect();
            format!(
                "READY but no rostered persona can claim it; would match: {}",
                why.join(", ")
            )
        };
        out.push(Row::new(
            "unrostered",
            id,
            Disposition::Escalate,
            detail,
            "roster the persona (spira-config set spira.runes) or relabel the bead".into(),
        ));
    }
    out
}

pub fn personas(cfg: &Config) -> Result<Vec<Persona>, String> {
    use spira_config::runes::{rune_get, rune_names, spira_runes};
    let home = cfg
        .home
        .as_ref()
        .ok_or("neither SPIRA_HOME nor SPIRA_RELEASE is set")?;
    let roster: HashSet<String> = spira_runes(home, cfg.runes_override.as_deref())
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let split = |s: String| -> Vec<String> {
        s.split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(str::to_string)
            .collect()
    };
    Ok(rune_names(home)
        .into_iter()
        .map(|n| Persona {
            labels: split(rune_get(home, &n, "RUNE_LABELS", "")),
            exclude: split(rune_get(home, &n, "RUNE_EXCLUDE_LABELS", "")),
            rostered: roster.contains(&n),
            call: rune_get(home, &n, "RUNE_CALL", "auto"),
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
        Persona {
            name: name.into(),
            labels: vec![label.into()],
            exclude: vec![],
            rostered,
            call: "auto".into(),
        }
    }
    fn one(id: &str) -> HashSet<String> {
        [id.to_string()].into()
    }

    #[test_id(t8muo5x)]
    #[test]
    fn a_ready_bead_for_an_unrostered_persona_is_reported_naming_it() {
        let s = store(r#"[{"id":"sp-a","status":"open","labels":["warden-sweep"]}]"#);
        let ps = [
            persona("builder", "plan", true),
            persona("warden", "warden-sweep", false),
        ];
        let r = rows(&one("sp-a"), &s, &ps, &[]);
        assert_eq!(r.len(), 1);
        assert!(
            r[0].detail.contains("warden (not in spira.runes)"),
            "{}",
            r[0].detail
        );
    }

    #[test_id(tkemaup)]
    #[test]
    fn a_rostered_match_is_not_reported() {
        let s = store(r#"[{"id":"sp-a","status":"open","labels":["warden-sweep"]}]"#);
        let ps = [persona("warden", "warden-sweep", true)];
        assert!(rows(&one("sp-a"), &s, &ps, &[]).is_empty());
    }

    #[test_id(te43113)]
    #[test]
    fn a_non_auto_call_persona_is_named() {
        let s = store(r#"[{"id":"sp-b","status":"open","labels":["y"]}]"#);
        let mut manual = persona("manual", "y", true);
        manual.call = "manual".into();
        let r = rows(&one("sp-b"), &s, &[manual], &[]);
        assert!(r[0].detail.contains("RUNE_CALL=manual"), "{}", r[0].detail);
    }

    #[test_id(tpa1or0)]
    #[test]
    fn a_bead_no_persona_matches_is_reported_and_a_blocked_one_is_not() {
        let s = store(
            r#"[{"id":"sp-a","status":"open","labels":["orphan"]},
                {"id":"sp-b","status":"open","labels":["orphan"],"dependencies":[{"depends_on_id":"sp-a","type":"blocks"}]}]"#,
        );
        let both: HashSet<String> = ["sp-a".to_string(), "sp-b".to_string()].into();
        let r = rows(&both, &s, &[persona("builder", "plan", true)], &[]);
        assert!(r
            .iter()
            .any(|x| x.id == "sp-a" && x.detail.contains("no persona in the runes")));
        assert_eq!(r.len(), 1, "sp-b waits on open sp-a");
    }
}
