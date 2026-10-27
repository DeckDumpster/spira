//! `ffx-vocabulary` — no Final Fantasy X term survives in a path or a file. Contract: DESIGN.md.

use std::collections::BTreeMap;
use std::sync::Mutex;

use regex::Regex;

use crate::{Entry, Finding, LintError, Rule, SyncCell, Tree};

pub struct FfxVocabulary {
    mode: Result<Mode, String>,
    checked: SyncCell<Option<usize>>,
    report: Mutex<Option<String>>,
}

const NAME: &str = "ffx-vocabulary";
const MODE_KEY: &str = "SPIRA_FFX_VOCABULARY_MODE";
const SELF_PATH: &str = "spira-lint/src/rules/ffx_vocabulary.rs";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Report,
    Enforce,
}

impl Mode {
    pub fn parse(v: &str) -> Result<Mode, String> {
        match v.trim() {
            "report" => Ok(Mode::Report),
            "enforce" => Ok(Mode::Enforce),
            other => Err(format!("{MODE_KEY} = {other:?} — expected \"report\" or \"enforce\"")),
        }
    }
}

/// Matched case-insensitively against one word of a name (words split at every non-alphanumeric
/// and at camelCase joins). "summonses" is deliberately absent: it is the legal plural, not the verb.
const TERMS: &[&str] = &[
    "aeon", "aeons", "fayth", "fayths", "chamber", "chambers", "maechen", "auron", "summon", "summons", "summoned",
    "summoning", "summoner", "summoners", "sphere", "spheres", "guardian", "guardians", "valefor", "ifrit", "ixion",
    "shiva", "bahamut", "anima", "yojimbo", "cindy", "sandy", "mindy", "magus",
];

/// Case-sensitive: "Sin" is the villain, "sin" is English.
const CASED: &[&str] = &["Sin"];

fn exempt(path: &str) -> bool {
    path == SELF_PATH || path.starts_with("docs/spikes/") || path.starts_with("raw/spira-beads/") || path.ends_with(".tsv")
}

/// Split a name or line into its words: alphanumeric runs, cut again at `aB` and `ABc` joins.
fn words(s: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_alphanumeric() {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_alphanumeric() {
            i += 1;
        }
        let mut from = start;
        for k in start + 1..i {
            let (p, c) = (b[k - 1], b[k]);
            let next_lower = k + 1 < i && b[k + 1].is_ascii_lowercase();
            let cut = c.is_ascii_uppercase() && (p.is_ascii_lowercase() || p.is_ascii_digit() || (p.is_ascii_uppercase() && next_lower));
            if cut {
                out.push((from, &s[from..k]));
                from = k;
            }
        }
        out.push((from, &s[from..i]));
    }
    out
}

/// Every FFX term in `text`, as the term lowercased (`Sin` as written).
pub fn terms_in(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (_, w) in words(text) {
        let lower = w.to_ascii_lowercase();
        if TERMS.contains(&lower.as_str()) {
            out.push(lower);
        } else if CASED.contains(&w) {
            out.push(w.to_string());
        }
    }
    static PHRASE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let phrase = PHRASE.get_or_init(|| Regex::new(r"(?i:\bthe)\s+Sending\b|(?i:\bsending[_-]keys\b)").expect("static regex"));
    for m in phrase.find_iter(text) {
        out.push(if m.as_str().to_ascii_lowercase().contains("keys") { "sending_keys".into() } else { "the Sending".into() });
    }
    out
}

impl FfxVocabulary {
    pub fn with_mode(mode: Result<Mode, String>) -> FfxVocabulary {
        FfxVocabulary { mode, checked: SyncCell::new(None), report: Mutex::new(None) }
    }

    fn hits(&self, tree: &Tree) -> Result<(Vec<(String, usize, String)>, usize), LintError> {
        let files = crate::scope(tree, self)?;
        let mut hits = Vec::new();
        for e in &files {
            for t in terms_in(&e.path) {
                hits.push((e.path.clone(), 0, t));
            }
            let Some(src) = tree.content(e) else { continue };
            if src.contains(&0) {
                continue;
            }
            let text = String::from_utf8_lossy(src);
            for (i, line) in text.lines().enumerate() {
                for t in terms_in(line) {
                    hits.push((e.path.clone(), i + 1, t));
                }
            }
        }
        Ok((hits, files.len()))
    }
}

impl Rule for FfxVocabulary {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        !exempt(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let mode = self.mode.clone().map_err(LintError::Refused)?;
        let (hits, nfiles) = self.hits(tree)?;
        self.checked.set(Some(nfiles));
        match mode {
            Mode::Enforce => Ok(hits
                .into_iter()
                .map(|(path, line, term)| Finding { rule: NAME, path, line: (line > 0).then_some(line), message: format!("the term \"{term}\"") })
                .collect()),
            Mode::Report => {
                let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
                let mut text = String::new();
                for (path, line, term) in &hits {
                    text.push_str(&format!("{NAME}: {path}:{line}:{term}\n"));
                    *counts.entry(term.as_str()).or_default() += 1;
                }
                for (term, n) in &counts {
                    text.push_str(&format!("{NAME}: count {term} {n}\n"));
                }
                text.push_str(&format!("{NAME}: remaining {} hit(s) in {} file(s) (report mode)\n", hits.len(), nfiles));
                *self.report.lock().unwrap_or_else(|e| e.into_inner()) = Some(text);
                Ok(Vec::new())
            }
        }
    }

    fn report(&self) -> Option<String> {
        self.report.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn checked(&self) -> Option<(usize, String)> {
        self.checked.get().map(|n| (n, "files".into()))
    }

    fn hint(&self) -> &'static str {
        "A Final Fantasy X term is retired from the working vocabulary; use its Norse replacement. \
Line 0 is a hit in the path itself."
    }
}

pub fn rules() -> Vec<Box<dyn Rule>> {
    let mode = spira_config::process::cfg(MODE_KEY).and_then(|v| Mode::parse(&v));
    vec![Box::new(FfxVocabulary::with_mode(mode))]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const ALL: &str = "aeon aeons fayth chamber Maechen Auron summon summoner the Sending sending_keys Sin sphere guardian \
valefor ifrit ixion shiva bahamut anima yojimbo cindy sandy mindy magus";

    fn tree(t: &TempDir, files: &[&str]) -> Tree {
        Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>())
    }

    fn fixture() -> TempDir {
        let t = TempDir::new("ffx");
        t.write("src/all.txt", &format!("{ALL}\n"));
        t.write("docs/spikes/old.md", "aeon fayth\n");
        t.write("data/runs.tsv", "aeon\n");
        t.write("src/clean.txt", "a sin of omission; summonses; Single; AtmoSphere is split\n");
        t
    }

    #[test]
    fn enforce_fails_on_every_term_and_report_lists_them_and_exits_clean() {
        let t = fixture();
        let tr = tree(&t, &["src/all.txt", "docs/spikes/old.md", "data/runs.tsv", "src/clean.txt"]);
        let found = FfxVocabulary::with_mode(Ok(Mode::Enforce)).check(&tr).unwrap();
        let on_all: Vec<_> = found.iter().filter(|f| f.path == "src/all.txt").collect();
        assert_eq!(on_all.len(), 24, "{on_all:?}");
        assert!(found.iter().all(|f| f.path != "docs/spikes/old.md" && f.path != "data/runs.tsv"));
        let r = FfxVocabulary::with_mode(Ok(Mode::Report));
        assert!(r.check(&tr).unwrap().is_empty());
        let text = r.report().unwrap();
        assert!(text.contains("ffx-vocabulary: src/all.txt:1:aeons\n") && text.contains("count aeon 1\n"), "{text}");
        assert!(text.contains("ffx-vocabulary: src/all.txt:1:Sin\n") && text.contains("the Sending"), "{text}");
        assert!(!text.contains("spikes") && !text.contains("tsv"), "{text}");
    }

    #[test]
    fn lowercase_sin_and_longer_words_are_not_terms() {
        assert_eq!(terms_in("a sin; SIN; Single; summonses; chambermaid; sending a message"), Vec::<String>::new());
        assert_eq!(terms_in("AeonState SPIRA_MAX_AEONS aeon-fence.sh"), vec!["aeon", "aeons", "aeon"]);
    }

    #[test]
    fn a_term_in_a_path_is_reported_at_line_zero() {
        let t = TempDir::new("ffx-path");
        t.write("spira/aeon-fence.sh", "echo hi\n");
        let found = FfxVocabulary::with_mode(Ok(Mode::Enforce)).check(&tree(&t, &["spira/aeon-fence.sh"])).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].line, found[0].path.as_str()), (None, "spira/aeon-fence.sh"));
    }

    #[test]
    fn an_unreadable_mode_refuses() {
        let t = fixture();
        let r = FfxVocabulary::with_mode(Mode::parse("loud")).check(&tree(&t, &["src/all.txt"]));
        assert!(matches!(r, Err(LintError::Refused(_))));
    }
}
