//! The classifier behind `detect_unclaimable_ready`: the ready set (a `bd --json` payload, list
//! or single object) in, one `UNCLAIMABLE <id> — <reason>` line per bead no persona can claim.
//!
//! `parts` is the active roster, `all_parts` the full runes, so a bead claimable only by a
//! parked persona reads as waiting rather than unclaimable. Kept a pure function of its inputs
//! so a fixture table can drive it (`sentinel --classify-unclaimable`).

use std::collections::{BTreeSet, HashSet};

use serde_json::Value;

pub struct Labels {
    pub scope: String,
    pub ci: String,
    pub ask: String,
    pub groom_ask: String,
}

type Parts = Vec<(String, BTreeSet<String>, BTreeSet<String>)>;

pub fn parse_parts(text: &str) -> Parts {
    let mut out: Parts = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut f = line.splitn(3, '|');
        let (Some(name), Some(inc), Some(exc)) = (f.next(), f.next(), f.next()) else { continue };
        let set = |s: &str| s.split(',').filter(|x| !x.is_empty()).map(str::to_string).collect::<BTreeSet<_>>();
        let row = (name.to_string(), set(inc), set(exc));
        match out.iter_mut().find(|r| r.0 == name) {
            Some(slot) => *slot = row,
            None => out.push(row),
        }
    }
    out
}

fn py_list(v: &BTreeSet<String>) -> String {
    format!("[{}]", v.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(", "))
}

pub fn classify(beads_json: &str, parts: &str, all_parts: &str, held_ids: &str, labels: &Labels) -> String {
    let Ok(d) = serde_json::from_str::<Value>(beads_json) else { return String::new() };
    let beads: Vec<&Value> = match &d {
        Value::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    let parts = parse_parts(parts);
    let all_parts = parse_parts(all_parts);
    let partition_labels: BTreeSet<&String> =
        all_parts.iter().flat_map(|(_, inc, _)| inc.iter()).filter(|l| **l != labels.scope).collect();
    let held: HashSet<&str> = held_ids.split(',').filter(|s| !s.is_empty()).collect();
    let mut out: Vec<String> = Vec::new();

    for bead in beads {
        let l: BTreeSet<String> = bead
            .get("labels")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let bid = bead.get("id").and_then(Value::as_str).unwrap_or("?");
        let kind = bead.get("issue_type").and_then(Value::as_str).unwrap_or("");
        if matches!(kind, "decision" | "event" | "epic") || l.contains(&labels.ask) {
            continue;
        }
        if held.contains(bid) || l.contains(&labels.groom_ask) {
            continue;
        }
        if bead.get("external_ref").and_then(Value::as_str).is_some_and(|r| r.starts_with("unclaimable:")) {
            continue;
        }
        if !labels.ci.is_empty() && l.contains(&labels.ci) {
            continue;
        }
        if !l.contains(&labels.scope) {
            out.push(format!(
                "UNCLAIMABLE {bid} — missing scope label ({s}); no persona can claim a bead without this label; add {s} or remove from the ready queue",
                s = labels.scope
            ));
            continue;
        }

        let pref: BTreeSet<String> = l.iter().filter_map(|x| x.strip_prefix("rune:").map(str::to_string)).collect();
        let matches = |inc: &BTreeSet<String>, exc: &BTreeSet<String>| inc.is_subset(&l) && l.is_disjoint(exc);
        if parts.iter().any(|(name, inc, exc)| matches(inc, exc) && (pref.is_empty() || pref.contains(name))) {
            continue;
        }
        if all_parts
            .iter()
            .filter(|(name, _, _)| !parts.iter().any(|p| &p.0 == name))
            .any(|(name, inc, exc)| matches(inc, exc) && (pref.is_empty() || pref.contains(name)))
        {
            continue;
        }

        if !pref.is_empty() {
            let reasons: Vec<String> = pref
                .iter()
                .map(|p| match all_parts.iter().find(|r| &r.0 == p) {
                    None => format!("{p} (not in runes)"),
                    Some((_, inc, _)) if !inc.is_subset(&l) => {
                        let missing: BTreeSet<String> = inc.difference(&l).cloned().collect();
                        format!("{p} (partition {}, missing {})", py_list(inc), py_list(&missing))
                    }
                    Some((_, _, exc)) if !l.is_disjoint(exc) => {
                        let blocked: BTreeSet<String> = l.intersection(exc).cloned().collect();
                        format!("{p} (excluded by own labels {})", py_list(&blocked))
                    }
                    Some(_) => format!("{p} (unknown reason)"),
                })
                .collect();
            let names = pref.iter().cloned().collect::<Vec<_>>().join(", ");
            out.push(format!(
                "UNCLAIMABLE {bid} — rune:{names} narrows to {names}, but none can claim it: {}; fix: drop the rune: label or add the named persona's partition labels",
                reasons.join("; ")
            ));
        } else {
            out.push(format!(
                "UNCLAIMABLE {bid} — spira with no matching partition; no persona's partition labels are all present; add one of: {}",
                partition_labels.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
    }
    if out.is_empty() {
        String::new()
    } else {
        format!("{}\n", out.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels() -> Labels {
        Labels { scope: "myscope".into(), ci: "ci-parked".into(), ask: "needs-decision".into(), groom_ask: "parked".into() }
    }
    const PARTS: &str = "builder|myscope,plan|spira-poison\nops|myscope,incident|spira-poison\n";

    #[test]
    fn a_claimable_bead_is_silent_and_an_unmatched_one_is_named() {
        let beads = r#"[{"id":"a","labels":["myscope","plan"]},{"id":"b","labels":["myscope","other"]}]"#;
        let got = classify(beads, PARTS, PARTS, "", &labels());
        assert_eq!(got, "UNCLAIMABLE b — spira with no matching partition; no persona's partition labels are all present; add one of: incident, plan\n");
    }

    #[test]
    fn missing_scope_and_rune_preference_and_exemptions() {
        let l = labels();
        let got = classify(r#"{"id":"c","labels":["plan"]}"#, PARTS, PARTS, "", &l);
        assert!(got.starts_with("UNCLAIMABLE c — missing scope label (myscope);"));
        let got = classify(r#"[{"id":"d","labels":["myscope","plan","rune:ops"]}]"#, PARTS, PARTS, "", &l);
        assert!(got.contains("rune:ops narrows to ops, but none can claim it: ops (partition ['incident', 'myscope'], missing ['incident'])"), "{got}");
        for (b, held) in [
            (r#"[{"id":"e","issue_type":"epic","labels":["x"]}]"#, ""),
            (r#"[{"id":"f","labels":["x","ci-parked"]}]"#, ""),
            (r#"[{"id":"g","labels":["x"]}]"#, "g"),
            (r#"[{"id":"h","labels":["x"],"external_ref":"unclaimable:z"}]"#, ""),
        ] {
            assert_eq!(classify(b, PARTS, PARTS, held, &l), "", "{b}");
        }
    }

    #[test]
    fn a_parked_persona_makes_the_bead_wait_not_unclaimable() {
        let all = format!("{PARTS}spike|myscope,spike|\n");
        let got = classify(r#"[{"id":"s","labels":["myscope","spike"]}]"#, PARTS, &all, "", &labels());
        assert_eq!(got, "");
    }
}
