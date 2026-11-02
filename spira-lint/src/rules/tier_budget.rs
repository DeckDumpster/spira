//! The tier-budget ratchets (sp-5m133), ported from `spira/tier-budget.sh lint-allowlist` and
//! `check-areas` (sp-ufbkh). Contract: DESIGN.md.
//!
//! * `tier-budget-allowlist`, `tier-budget-area-allowlist` ([`Ledger`]): each ledger may only
//!   shrink against the base — no new key, no raised value. The bash passed with exit 0 when
//!   it found no prior copy ("this file's own introducing commit"), which also passed an
//!   unresolvable base having compared nothing; that is a refusal here.
//! * `tier-budget-areas` ([`Areas`]): at most one T3 suite per use-case area, unless the area
//!   ledger grandfathers more.

use crate::SyncCell as Cell;
use std::collections::{BTreeMap, BTreeSet};

use crate::rules::covers_entries::covers_of;
use crate::rules::plan_matrix::tier_of;
use crate::{direct_child, Entry, Finding, LintError, Rule, Tree};

pub const SUITE_LEDGER: &str = "spira/tier-budget-allowlist";
pub const AREA_LEDGER: &str = "spira/tier-budget-area-allowlist";

/// A ledger's entries: key (column 1) → value (the last column), tab-separated, `#` comments
/// and blank lines skipped; the first entry for a key wins, as the bash's `awk … exit`.
pub fn ledger(text: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for l in text.lines() {
        let t = l.trim_start();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = l.split('\t').collect();
        let (k, v) = (cols[0], cols[cols.len() - 1]);
        if k.is_empty() {
            continue;
        }
        m.entry(k.to_string()).or_insert_with(|| v.to_string());
    }
    m
}

/// awk's `now > was`: numerically when both read as numbers, else as strings.
fn raised(now: &str, was: &str) -> bool {
    match (now.trim().parse::<f64>(), was.trim().parse::<f64>()) {
        (Ok(a), Ok(b)) => a > b,
        _ => now > was,
    }
}

/// One shrink-only ledger.
pub struct Ledger {
    name: &'static str,
    path: &'static str,
    checked: Cell<Option<(usize, usize)>>,
}

impl Ledger {
    pub fn suites() -> Ledger {
        Ledger { name: "tier-budget-allowlist", path: SUITE_LEDGER, checked: Cell::new(None) }
    }
    pub fn areas() -> Ledger {
        Ledger { name: "tier-budget-area-allowlist", path: AREA_LEDGER, checked: Cell::new(None) }
    }
}

/// Keys a ledger admits above its base: a newly added `# exposure: <key> <bead-id>` comment line, for a
/// suite that already existed at T3 and was only counted once tagged — exposure, not growth.
fn exposures(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("# exposure:"))
        .filter_map(|r| {
            let mut w = r.split_whitespace();
            let (key, bead) = (w.next()?, w.next()?);
            bead.starts_with("sp-").then(|| key.to_string())
        })
        .collect()
}

impl Rule for Ledger {
    fn name(&self) -> &'static str {
        self.name
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path == self.path
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        let base = tree.base_commit()?;
        let now = std::fs::read_to_string(tree.root.join(self.path))
            .map_err(|_| LintError::Refused(format!("no ledger at {} — nothing to lint", self.path)))?;
        let prior = tree.show(&base, self.path).ok_or_else(|| {
            LintError::Refused(format!("no {} at the base {base} — cannot judge shrink-only", self.path))
        })?;
        let exposed: BTreeSet<String> = exposures(&now).difference(&exposures(&prior)).cloned().collect();
        let prior: BTreeMap<String, String> =
            ledger(&prior).into_iter().map(|(k, v)| (crate::rules::ffx_vocabulary::renamed(&k), v)).collect();
        let now = ledger(&now);
        let mut out = Vec::new();
        for (k, v) in &now {
            if exposed.contains(k) {
                continue;
            }
            let message = match prior.get(k) {
                None => format!("new entry {k} — the allowlist may only shrink"),
                Some(was) if raised(v, was) => format!("{k} raised {was} -> {v} — the allowlist may only shrink"),
                Some(_) => continue,
            };
            out.push(Finding { rule: self.name, path: self.path.into(), line: None, message });
        }
        if out.is_empty() {
            self.checked.set(Some((now.len(), prior.len())));
        }
        Ok(out)
    }

    /// One ledger compared, whatever its size: an allowlist that has shrunk to nothing is the
    /// goal, not a silent check.
    fn checked(&self) -> Option<(usize, String)> {
        self.checked
            .get()
            .map(|(n, was)| (1, format!("ledger ({n} entries, {was} at the base)")))
    }

    fn hint(&self) -> &'static str {
        "the tier-budget allowlists only shrink: fix the suite, don't grandfather it"
    }
}

/// `UC-<area>-NN` → `<area>` (the bash's `${uc#UC-}` then `${area%-*}`), for tokens shaped
/// `UC-*-[0-9][0-9]`.
pub fn area_of(tok: &str) -> Option<&str> {
    let rest = tok.strip_prefix("UC-")?;
    let (area, nn) = rest.rsplit_once('-')?;
    let two = nn.len() == 2 && nn.bytes().all(|b| b.is_ascii_digit());
    (two && !area.is_empty()).then_some(area)
}

/// At most one T3 suite per area.
#[derive(Default)]
pub struct Areas {
    checked: Cell<Option<usize>>,
}

impl Rule for Areas {
    fn name(&self) -> &'static str {
        "tier-budget-areas"
    }

    /// `spira/test-*.sh`, directly in spira/.
    fn applies_to(&self, e: &Entry) -> bool {
        direct_child(&e.path, "spira").is_some_and(|b| b.starts_with("test-") && b.ends_with(".sh"))
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        let scope = crate::scope(tree, self)?;
        let mut by_area: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for e in &scope {
            let Some(c) = tree.content(e) else { continue };
            let text = String::from_utf8_lossy(c);
            if tier_of(&text).as_deref() != Some("T3") {
                continue;
            }
            let base = e.path.rsplit('/').next().unwrap_or(&e.path).to_string();
            for tok in covers_of(&text).unwrap_or_default() {
                if let Some(a) = area_of(&tok) {
                    by_area.entry(a.to_string()).or_default().insert(base.clone());
                }
            }
        }
        let allow = ledger(&tree.read_text(AREA_LEDGER));
        let mut out = Vec::new();
        for (area, suites) in &by_area {
            let allowed: usize = allow.get(area).and_then(|v| v.trim().parse().ok()).unwrap_or(0);
            if suites.len() > 1 && suites.len() > allowed {
                out.push(Finding {
                    rule: "tier-budget-areas",
                    path: format!("area {area}"),
                    line: None,
                    message: format!(
                        "{} T3 suites (max 1; allowlisted {allowed}) — {}",
                        suites.len(),
                        suites.iter().cloned().collect::<Vec<_>>().join(",")
                    ),
                });
            }
        }
        if out.is_empty() {
            self.checked.set(Some(scope.len()));
        }
        Ok(out)
    }

    fn checked(&self) -> Option<(usize, String)> {
        self.checked.get().map(|n| (n, "suites".into()))
    }

    fn hint(&self) -> &'static str {
        "at most one T3 suite per use-case area: consolidate, or lower a tier"
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(Ledger::suites()),
        Box::new(Ledger::areas()),
        Box::new(Areas::default()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn area_of_reads_the_uc_shape() {
        assert_eq!(area_of("UC-operator-channel-14"), Some("operator-channel"));
        assert_eq!(area_of("UC-alpha-1"), None);
        assert_eq!(area_of("UC-alpha-123"), None);
        assert_eq!(area_of("spira/x.sh"), None);
    }
}
