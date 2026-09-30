//! Regenerates `wiki/notes/standard-operating-procedures.md` whole from the shelf — never
//! patched (law-regenerate-derived-summaries). Editing that page does nothing; amend the
//! SOP instead.

use std::collections::BTreeMap;

const FIELDS: &[&str] = &["MATCH", "SYMPTOM", "CHECK", "METRIC", "FIX", "ESCALATE", "REF"];

/// Splits one SOP's text into its named fields. Mirrors the bash's lookahead regex
/// (`^\s*NAME:\s*(.*?)(?=^\s*(?:MATCH|SYMPTOM|...):|\Z)`) without lookahead, which the
/// `regex` crate's engine does not support: a field's text runs from right after its own
/// header to the start of the next recognised header line, by walking lines in order.
fn fields(text: &str) -> BTreeMap<&'static str, String> {
    let mut out: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    let mut current: Option<&'static str> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let mut matched = None;
        for name in FIELDS {
            if let Some(rest) = trimmed.strip_prefix(&format!("{name}:")) {
                matched = Some((*name, rest));
                break;
            }
        }
        if let Some((name, rest)) = matched {
            current = Some(name);
            out.entry(name).or_default().push(rest.to_string());
        } else if let Some(name) = current {
            out.entry(name).or_default().push(line.to_string());
        }
    }
    out.into_iter()
        .map(|(k, v)| (k, v.join("\n").trim().to_string()))
        .collect()
}

fn wrap_or_code(label: &str, val: &str) -> Vec<String> {
    if val.is_empty() {
        return Vec::new();
    }
    let looks_like_command = val.contains('\n')
        || ["$", "sudo", "systemctl", "bd ", "git "].iter().any(|p| val.trim_start().starts_with(p));
    if looks_like_command {
        let mut v = vec![format!("**{label}**"), String::new(), "```".to_string()];
        v.extend(val.lines().map(String::from));
        v.push("```".to_string());
        v.push(String::new());
        v
    } else {
        vec![format!("**{label}** — {val}"), String::new()]
    }
}

/// `today` is `YYYY-MM-DD`. Returns the full page text.
pub fn render(sops: &BTreeMap<String, String>, today: &str) -> String {
    let mut b: Vec<String> = Vec::new();
    b.push("---".into());
    b.push("type: note".into());
    b.push("created: 2026-09-05".into());
    b.push(format!("updated: {today}"));
    b.push("tags: [spira, ops, sop, runbook, generated]".into());
    b.push("aliases: [SOPs, Standard operating procedures, The shelf]".into());
    b.push("---".into());
    b.push(String::new());
    b.push("# Standard operating procedures".into());
    b.push(String::new());
    b.push(
        "**Generated — do not edit.** Regenerated whole by the harness's `sop synth` from \
the Spira beads database, which is the source of truth. Editing this page has no effect; \
the next run overwrites it. Amend an SOP instead:"
            .into(),
    );
    b.push(String::new());
    b.push("```bash".into());
    b.push("sop write <slug> -   # text on stdin".into());
    b.push("```".into());
    b.push(String::new());
    b.push(
        "Statutes are how to behave; SOPs are how to fix. They share one mechanism, split by \
prefix — `law-` and `sop-` — so the [[spira]] Ops persona reads its runbooks exactly the way \
every agent already reads [[common-law]]. Ops is summoned by an incident bead filed from a \
failed systemd unit, matches the payload against the `MATCH:` lines below, and executes the \
first one that fires."
            .into(),
    );
    b.push(String::new());
    b.push(format!("**{} SOP(s)** on the shelf as of {today}.", sops.len()));
    b.push(String::new());
    b.push("## The closing rule".into());
    b.push(String::new());
    b.push(
        "**An incident resolved without an SOP must produce one.** This is \
`law-bake-rules-into-tools` applied to production, and it is enforced rather than asked for: \
writing an SOP is what regenerates this page, the regenerated page is the commit that names \
the incident bead, and a bead closed with no commit naming it is reopened by `aeon.sh`. An \
incident fixed by hand and forgotten does not close."
            .into(),
    );
    b.push(String::new());
    b.push("## The shelf".into());
    b.push(String::new());
    if sops.is_empty() {
        b.push("*Empty.* The first incident to be resolved fills it.".into());
        b.push(String::new());
    }
    for (k, v) in sops {
        let title = k.trim_start_matches("sop-").replace('-', " ");
        let title = capitalize(&title);
        b.push(format!("### {title}"));
        b.push(String::new());
        b.push(format!("`{k}`"));
        b.push(String::new());
        let f = fields(v);
        for (name, label) in [
            ("SYMPTOM", "Symptom"),
            ("CHECK", "Check"),
            ("METRIC", "Metric"),
            ("FIX", "Fix"),
            ("ESCALATE", "Escalate"),
            ("REF", "Reference"),
        ] {
            if let Some(val) = f.get(name) {
                b.extend(wrap_or_code(label, val));
            }
        }
        match f.get("MATCH") {
            Some(m) if !m.is_empty() => b.push(format!("**Matches** `{m}`")),
            _ => b.push(
                "**Matches** — no `MATCH:` line; this SOP is found by key tokens only, which is \
weak. Add one."
                    .to_string(),
            ),
        }
        b.push(String::new());
    }
    b.push("Related: [[spira]], [[common-law]], [[codified-judgement]]".into());
    b.push(String::new());
    b.join("\n")
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_shelf_renders_the_empty_notice() {
        let out = render(&BTreeMap::new(), "2026-09-30");
        assert!(out.contains("*Empty.*"));
        assert!(out.contains("**0 SOP(s)**"));
    }

    #[test]
    fn one_sop_renders_its_fields() {
        let mut m = BTreeMap::new();
        m.insert(
            "sop-disk-full".to_string(),
            "MATCH: disk.*full\nSYMPTOM: disk is full\nCHECK: df -h\nFIX: clear /tmp".to_string(),
        );
        let out = render(&m, "2026-09-30");
        assert!(out.contains("### Disk full"));
        assert!(out.contains("`sop-disk-full`"));
        assert!(out.contains("**Symptom** — disk is full"));
        assert!(out.contains("**Matches** `disk.*full`"));
        assert!(out.contains("**1 SOP(s)**"));
    }

    #[test]
    fn a_missing_match_line_says_so() {
        let mut m = BTreeMap::new();
        m.insert("sop-x".to_string(), "SYMPTOM: s\nCHECK: c\nFIX: f".to_string());
        let out = render(&m, "2026-09-30");
        assert!(out.contains("no `MATCH:` line"));
    }

    #[test]
    fn a_multiline_fix_renders_as_a_code_block() {
        let mut m = BTreeMap::new();
        m.insert("sop-x".to_string(), "SYMPTOM: s\nCHECK: c\nFIX: systemctl restart foo\nsystemctl status foo".to_string());
        let out = render(&m, "2026-09-30");
        assert!(out.contains("```\nsystemctl restart foo\nsystemctl status foo\n```"));
    }

    #[test]
    fn fields_parses_out_of_order_and_multiline_bodies() {
        let text = "FIX: line one\nline two\nSYMPTOM: sym one";
        let f = fields(text);
        assert_eq!(f.get("FIX").unwrap(), "line one\nline two");
        assert_eq!(f.get("SYMPTOM").unwrap(), "sym one");
    }
}
