//! The statute check every decision-recording tool runs on its own text before the write.
//!
//! File format, one row per line: `slug | refuse|show|plan | regex`; `#` comments and blank
//! lines skipped. A `refuse` row names a statute the text contradicts; a `show` row names one
//! the text touches. Both are recorded on the bead; only `refuse` stops the write. A `plan`
//! row names a statute a builder's plan must read when it matches a governed decision; the
//! decision-recording check ignores it and [`plan_check`] reads only it. The registry is a
//! file, so the check never waits on the store and an unreadable registry refuses.

use regex::RegexBuilder;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Refuse,
    Show,
    Plan,
}

pub const OVERRIDE_ENV: &str = "BEAD_STATUTE_CONSIDERED";

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub slug: String,
    pub refuses: bool,
    pub matched: String,
}

pub fn parse(text: &str) -> Result<Vec<(String, Mode, regex::Regex)>, String> {
    let mut rows = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let mut f = t.splitn(3, '|').map(str::trim);
        let (Some(slug), Some(mode), Some(pat)) = (f.next(), f.next(), f.next()) else {
            return Err(format!("line {}: want `slug | refuse|show | regex`", n + 1));
        };
        let mode = match mode {
            "refuse" => Mode::Refuse,
            "show" => Mode::Show,
            "plan" => Mode::Plan,
            other => return Err(format!("line {}: mode {other:?} is not refuse, show or plan", n + 1)),
        };
        let re = RegexBuilder::new(pat)
            .case_insensitive(true)
            .build()
            .map_err(|e| format!("line {}: {e}", n + 1))?;
        rows.push((slug.to_string(), mode, re));
    }
    Ok(rows)
}

/// Every row `text` matches, refusing rows first; one hit per slug.
pub fn check(registry: &str, text: &str) -> Result<Vec<Hit>, String> {
    let mut hits: Vec<Hit> = Vec::new();
    for (slug, mode, re) in parse(registry)? {
        if mode == Mode::Plan {
            continue;
        }
        let refuses = mode == Mode::Refuse;
        let Some(m) = re.find(text) else { continue };
        match hits.iter_mut().find(|h| h.slug == slug) {
            Some(h) => h.refuses |= refuses,
            None => hits.push(Hit { slug, refuses, matched: m.as_str().to_string() }),
        }
    }
    hits.sort_by_key(|h| !h.refuses);
    Ok(hits)
}

/// The statutes a builder's plan must read: every `plan` row `plan` matches, one hit per slug.
/// Empty means the plan makes no governed decision and no check is owed.
pub fn plan_check(registry: &str, plan: &str) -> Result<Vec<Hit>, String> {
    let mut hits: Vec<Hit> = Vec::new();
    for (slug, mode, re) in parse(registry)? {
        if mode != Mode::Plan || hits.iter().any(|h| h.slug == slug) {
            continue;
        }
        if let Some(m) = re.find(plan) {
            hits.push(Hit { slug, refuses: false, matched: m.as_str().to_string() });
        }
    }
    Ok(hits)
}

pub fn shown(hits: &[Hit]) -> String {
    let mut m = String::from("bead: statute check matched:\n");
    for h in hits {
        let kind = if h.refuses { "contradicts" } else { "touches" };
        m.push_str(&format!("  {} ({kind}: {:?})\n", h.slug, h.matched));
    }
    m
}

pub fn refusal(hits: &[Hit]) -> String {
    let mut m = shown(hits);
    m.push_str(&format!(
        "bead: refusing: the text contradicts a statute named above. Read it with `rule.sh show <slug>`; if the statute does not apply here, set {OVERRIDE_ENV}=\"<why>\" and the reason is recorded."
    ));
    m
}

pub fn override_reason(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

pub fn recorded_line(hits: &[Hit], overridden: Option<&str>) -> String {
    let slugs = hits.iter().map(|h| h.slug.as_str()).collect::<Vec<_>>().join(", ");
    match overridden {
        Some(why) => format!("Statute check: {slugs} (override: {why})"),
        None => format!("Statute check: {slugs}"),
    }
}

/// What a decision-recording tool shows its actor and writes into the record.
#[derive(Debug, PartialEq)]
pub struct Admitted {
    pub shown: String,
    pub line: String,
}

/// The whole check for tools that hold the registry text: `Ok(None)` when nothing matched,
/// `Ok(Some)` when the text is admitted (touching only, or a contradiction with an override
/// reason), `Err(message)` when it is refused or the registry is malformed.
pub fn gate(registry: &str, text: &str, override_value: Option<String>) -> Result<Option<Admitted>, String> {
    let hits = check(registry, text).map_err(|e| format!("statute registry: {e}"))?;
    if hits.is_empty() {
        return Ok(None);
    }
    let why = if hits.iter().any(|h| h.refuses) {
        match override_reason(override_value) {
            Some(w) => Some(w),
            None => return Err(refusal(&hits)),
        }
    } else {
        None
    };
    Ok(Some(Admitted { shown: shown(&hits), line: recorded_line(&hits, why.as_deref()) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REG: &str = "# c\nlaw-alloc | refuse | pick \\w+ from a configured range\nlaw-touch | show | allocator\n";

    #[test]
    fn a_contradicting_text_is_a_refusing_hit_listed_first() {
        let h = check(REG, "Pick VMIDs from a configured range via the allocator").unwrap();
        assert_eq!(h.iter().map(|h| (h.slug.as_str(), h.refuses)).collect::<Vec<_>>(), vec![("law-alloc", true), ("law-touch", false)]);
    }

    #[test]
    fn plan_rows_are_read_only_by_the_plan_check() {
        let reg = format!("{REG}law-lang | plan | new \\w+ program\n");
        assert!(check(&reg, "add a new python program").unwrap().is_empty());
        let h = plan_check(&reg, "Add a new python program").unwrap();
        assert_eq!(h.iter().map(|h| h.slug.as_str()).collect::<Vec<_>>(), vec!["law-lang"]);
        assert!(plan_check(&reg, "edit inside an existing function").unwrap().is_empty());
        assert!(plan_check("law-x | plan | (", "x").is_err());
    }

    #[test]
    fn silence_is_believed_only_after_a_hit() {
        assert!(check(REG, "tidy the README").unwrap().is_empty());
    }

    #[test]
    fn a_malformed_registry_is_an_error_never_an_empty_pass() {
        assert!(check("law-x | maybe | y", "y").is_err());
        assert!(check("law-x | refuse | (", "y").is_err());
        assert!(check("law-x only", "y").is_err());
    }

    #[test]
    fn the_recorded_line_names_every_slug_and_the_override() {
        let h = check(REG, "pick vmids from a configured range").unwrap();
        assert_eq!(recorded_line(&h, None), "Statute check: law-alloc");
        assert!(recorded_line(&h, Some("why")).contains("override: why"));
    }

    #[test]
    fn gate_refuses_admits_with_a_reason_and_records_a_touch() {
        let bad = "pick vmids from a configured range";
        assert!(gate(REG, bad, None).unwrap_err().contains("law-alloc"));
        assert!(gate(REG, bad, Some("  ".into())).is_err());
        let a = gate(REG, bad, Some("why".into())).unwrap().unwrap();
        assert!(a.line.contains("override: why") && a.shown.contains("law-alloc"));
        let t = gate(REG, "the allocator", None).unwrap().unwrap();
        assert_eq!(t.line, "Statute check: law-touch");
        assert_eq!(gate(REG, "tidy", None).unwrap(), None);
        assert!(gate("law-x | maybe | y", "y", None).is_err());
    }
}
