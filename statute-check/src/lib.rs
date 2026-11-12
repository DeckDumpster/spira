//! The statutes in force that govern a decision. The shelf is the statute directory itself, one
//! `<slug>.txt` per statute; nothing here names a statute. A vocabulary file of synonym sets
//! bridges the words a decision uses to the words a statute uses.

pub mod doors;
pub mod hook;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Statute {
    pub slug: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ranked {
    pub statute: Statute,
    pub score: f64,
}

const STOP: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "into", "are", "was", "were", "has", "have", "had", "not", "but", "its", "their", "then", "than", "when",
    "while", "which", "who", "what", "any", "all", "each", "one", "two", "may", "must", "can", "will", "would", "should", "also", "only", "own", "per", "via",
    "run", "use", "used", "using", "after", "before", "does", "did", "out", "off", "too", "now", "bin",
];

/// Every `<slug>.txt` in `dir`; an unreadable or empty shelf is an error, never "nothing governs".
pub fn load_shelf(dir: &Path) -> Result<Vec<Statute>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("cannot read the statute shelf {}: {e}", dir.display()))?;
    let mut out = Vec::new();
    for ent in rd.flatten() {
        let p = ent.path();
        if p.extension().and_then(|e| e.to_str()) != Some("txt") {
            continue;
        }
        let Some(slug) = p.file_stem().and_then(|s| s.to_str()).filter(|s| s.starts_with("law-")) else { continue };
        let text = std::fs::read_to_string(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
        out.push(Statute { slug: slug.to_string(), text: text.trim().to_string() });
    }
    if out.is_empty() {
        return Err(format!("the statute shelf {} holds no statute; an empty shelf is not a pass", dir.display()));
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(out)
}

/// Synonym sets, one per line: `a b c`. Any word of a set stands for every other. `#` comments.
#[derive(Debug, Default, Clone)]
pub struct Vocab(Vec<BTreeSet<String>>);

impl Vocab {
    pub fn parse(text: &str) -> Vocab {
        let sets = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.split_whitespace().map(stem).collect::<BTreeSet<_>>())
            .filter(|s| s.len() > 1)
            .collect();
        Vocab(sets)
    }

    pub fn load(path: &Path) -> Result<Vocab, String> {
        std::fs::read_to_string(path).map(|t| Vocab::parse(&t)).map_err(|e| format!("cannot read the vocabulary {}: {e}", path.display()))
    }

    fn related(&self, term: &str) -> BTreeSet<&str> {
        self.0.iter().filter(|s| s.contains(term)).flatten().map(String::as_str).filter(|w| *w != term).collect()
    }
}

fn stem(w: &str) -> String {
    let w = w.to_ascii_lowercase();
    for suf in ["ing", "ed", "es", "s"] {
        if w.len() > suf.len() + 3 && w.ends_with(suf) {
            return w[..w.len() - suf.len()].to_string();
        }
    }
    w
}

fn terms(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.len() >= 3 && !STOP.contains(&w.to_ascii_lowercase().as_str()))
        .map(stem)
        .collect()
}

fn counts(ts: &[String]) -> BTreeMap<&str, usize> {
    let mut m = BTreeMap::new();
    for t in ts {
        *m.entry(t.as_str()).or_insert(0) += 1;
    }
    m
}

const SLUG_WEIGHT: f64 = 3.0;
const RELATED_WEIGHT: f64 = 0.6;
const RELATED_CAP: f64 = 2.0;
/// A statute must reach this share of the best score to be returned.
const KEEP_SHARE: f64 = 0.4;

/// The statutes governing `query`, best first, at most `max`. Empty when no word of the query
/// reaches any statute.
pub fn rank(shelf: &[Statute], vocab: &Vocab, query: &str, max: usize) -> Vec<Ranked> {
    let qterms = terms(query);
    let mut weights: BTreeMap<String, f64> = BTreeMap::new();
    for t in &qterms {
        let w = weights.entry(t.clone()).or_insert(0.0);
        *w = w.max(1.0);
    }
    for t in &qterms {
        for r in vocab.related(t) {
            let w = weights.entry(r.to_string()).or_insert(0.0);
            *w = (*w + RELATED_WEIGHT).min(RELATED_CAP);
        }
    }
    let docs: Vec<(Vec<String>, Vec<String>)> = shelf
        .iter()
        .map(|s| (terms(&s.slug.trim_start_matches("law-").replace('-', " ")), terms(&s.text)))
        .collect();
    let n = shelf.len() as f64;
    let df = |t: &str| docs.iter().filter(|(sl, tx)| sl.iter().chain(tx).any(|x| x == t)).count() as f64;
    let mut scored: Vec<Ranked> = Vec::new();
    for (s, (slug_terms, text_terms)) in shelf.iter().zip(&docs) {
        let tf = counts(text_terms);
        let mut score = 0.0;
        for (t, w) in &weights {
            let idf = (1.0 + n / (1.0 + df(t))).ln();
            let in_text = tf.get(t.as_str()).copied().unwrap_or(0);
            if in_text > 0 {
                score += w * idf * (1.0 + (in_text as f64).ln());
            }
            if slug_terms.contains(t) {
                score += w * idf * SLUG_WEIGHT;
            }
        }
        if score > 0.0 {
            scored.push(Ranked { statute: s.clone(), score });
        }
    }
    scored.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.statute.slug.cmp(&b.statute.slug)));
    let best = scored.first().map_or(0.0, |r| r.score);
    scored.retain(|r| r.score >= best * KEEP_SHARE);
    scored.truncate(max);
    scored
}

pub fn render(hits: &[Ranked]) -> String {
    hits.iter().map(|h| format!("{}\n{}\n", h.statute.slug, h.statute.text)).collect::<Vec<_>>().join("\n")
}
