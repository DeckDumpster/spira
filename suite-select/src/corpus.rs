//! The suite corpus: every `test-*.sh` in a suite directory, with its parsed header.
//!
//! Fails closed: a missing directory, an empty corpus, a suite that cannot be read, or a
//! declared `# tier:` that is not `T0`..`T4` is a [`Refusal`] — never a smaller corpus.

use crate::header::{self, Tier};
use crate::{refuse, Refusal};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suite {
    /// The basename, `test-<x>.sh`.
    pub name: String,
    /// `# covers:` tokens; `None` when undeclared or empty — an always-run suite.
    pub covers: Option<Vec<String>>,
    /// `# tier:`; `None` when undeclared.
    pub tier: Option<Tier>,
    /// `# selects-on:` events (`added`, `mode`).
    pub selects_on: Vec<String>,
}

impl Suite {
    /// Parse one suite's header. A declared tier outside `T0`..`T4` is a refusal: the tier
    /// table is unreadable, and guessing would either drop or add the suite by accident.
    pub fn parse(name: &str, text: &str) -> Result<Suite, Refusal> {
        let tier = match header::tier_of(text) {
            None => None,
            Some(raw) => {
                let t = raw.trim();
                if t.is_empty() {
                    None
                } else {
                    match Tier::parse(t) {
                        Some(t) => Some(t),
                        None => {
                            return refuse(format!(
                                "{name}: declares '# tier: {t}', which is not T0..T4 — cannot place it in a tier"
                            ))
                        }
                    }
                }
            }
        };
        Ok(Suite {
            name: name.to_string(),
            covers: header::covers_of(text).filter(|c| !c.is_empty()),
            tier,
            selects_on: header::selects_on_of(text),
        })
    }

    /// The tier the budget judges it by: undeclared counts as T1.
    pub fn tier_or_t1(&self) -> Tier {
        self.tier.unwrap_or(Tier::T1)
    }
}

/// A shell `test-*.sh` basename.
pub fn is_suite_file(name: &str) -> bool {
    name.starts_with("test-") && name.ends_with(".sh") && name.len() >= "test-.sh".len()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Corpus {
    /// Sorted by name (bytes).
    pub suites: Vec<Suite>,
}

impl Corpus {
    pub fn new(mut suites: Vec<Suite>) -> Corpus {
        suites.sort_by(|a, b| a.name.cmp(&b.name));
        suites.dedup_by(|a, b| a.name == b.name);
        Corpus { suites }
    }

    pub fn names(&self) -> Vec<String> {
        self.suites.iter().map(|s| s.name.clone()).collect()
    }

    pub fn get(&self, name: &str) -> Option<&Suite> {
        self.suites
            .binary_search_by(|s| s.name.as_str().cmp(name))
            .ok()
            .map(|i| &self.suites[i])
    }

    /// Every `test-*.sh` in `dir`. Refuses a missing directory, an empty corpus, and any
    /// suite it cannot read or place.
    pub fn load(dir: &Path) -> Result<Corpus, Refusal> {
        let rd = std::fs::read_dir(dir).map_err(|e| {
            Refusal(format!("cannot read the suite directory {}: {e}", dir.display()))
        })?;
        let mut names = Vec::new();
        for ent in rd {
            let ent = ent.map_err(|e| {
                Refusal(format!("cannot list the suite directory {}: {e}", dir.display()))
            })?;
            let n = ent.file_name().to_string_lossy().to_string();
            if is_suite_file(&n) {
                names.push(n);
            }
        }
        Self::load_named(dir, &names)
    }

    /// The suites `names` in `dir`, each of which must be readable. Refuses an empty list.
    pub fn load_named(dir: &Path, names: &[String]) -> Result<Corpus, Refusal> {
        if names.is_empty() {
            return refuse(format!(
                "no test-*.sh suites in {} — refusing to select from an empty corpus",
                dir.display()
            ));
        }
        let mut suites = Vec::with_capacity(names.len());
        for n in names {
            let p: PathBuf = dir.join(n);
            let bytes = std::fs::read(&p)
                .map_err(|e| Refusal(format!("cannot read suite {}: {e}", p.display())))?;
            suites.push(Suite::parse(n, &String::from_utf8_lossy(&bytes))?);
        }
        Ok(Corpus::new(suites))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bad_tier_is_a_refusal_and_an_empty_covers_is_always_run() {
        assert!(Suite::parse("test-a.sh", "# tier: T9\n").is_err());
        assert!(Suite::parse("test-a.sh", "# tier: t1\n").is_err());
        let s = Suite::parse("test-a.sh", "# tier: T2 \n# covers:\n").unwrap();
        assert_eq!(s.tier, Some(Tier::T2));
        assert_eq!(s.covers, None);
        assert_eq!(Suite::parse("test-a.sh", "# tier:\n").unwrap().tier, None);
    }

    #[test]
    fn loading_refuses_what_it_cannot_read() {
        let d = std::env::temp_dir().join(format!("sp-wx2tw-corpus-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        assert!(Corpus::load(&d).is_err(), "missing dir");
        std::fs::create_dir_all(&d).unwrap();
        assert!(Corpus::load(&d).is_err(), "empty corpus");
        std::fs::write(d.join("test-b.sh"), "# covers: x\n").unwrap();
        std::fs::write(d.join("test-a.sh"), "# tier: T1\n").unwrap();
        std::fs::write(d.join("helper.sh"), "").unwrap();
        let c = Corpus::load(&d).unwrap();
        assert_eq!(c.names(), ["test-a.sh", "test-b.sh"]);
        assert!(Corpus::load_named(&d, &["test-gone.sh".into()]).is_err());
        std::fs::write(d.join("test-c.sh"), "# tier: TX\n").unwrap();
        assert!(Corpus::load(&d).is_err(), "an unplaceable tier");
        let _ = std::fs::remove_dir_all(&d);
    }
}
