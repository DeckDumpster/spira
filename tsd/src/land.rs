//! One record per member landed, with the commit it reverts when it is a revert: the `land`
//! family. `reverts` is a field of the landing, never read from a commit subject.

use serde_json::Value;
use std::path::Path;

pub const FAMILY: &str = "land";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandRecord {
    pub repo: String,
    pub bead: String,
    /// The member's tip: the commit a later revert names.
    pub commit: String,
    pub reverts: Option<String>,
}

impl LandRecord {
    pub fn validate(&self) -> Result<(), String> {
        for (name, v) in [("repo", &self.repo), ("bead", &self.bead), ("commit", &self.commit)] {
            if v.trim().is_empty() {
                return Err(format!("a land record needs a {name}"));
            }
        }
        if self.reverts.as_deref().is_some_and(|r| !is_commit(r)) {
            return Err(format!("reverts {:?} is not a commit id", self.reverts));
        }
        Ok(())
    }

    pub fn fields(&self) -> Vec<(String, Value)> {
        vec![
            ("repo".into(), self.repo.clone().into()),
            ("bead".into(), self.bead.clone().into()),
            ("commit".into(), self.commit.clone().into()),
            ("reverts".into(), self.reverts.clone().map_or(Value::Null, Value::from)),
        ]
    }

    pub fn from_row(row: &Value) -> Result<LandRecord, String> {
        let s = |k: &str| row.get(k).and_then(Value::as_str).map(str::to_string);
        let need = |k: &str| s(k).ok_or_else(|| format!("row has no {k}"));
        let rec = LandRecord { repo: need("repo")?, bead: need("bead")?, commit: need("commit")?, reverts: s("reverts") };
        rec.validate()?;
        Ok(rec)
    }

    /// Appends this landing's one row to `<root>/tsd/land.jsonl`.
    pub fn write(&self, root: &Path, host: &str, ts: &str) -> Result<(), String> {
        self.validate()?;
        let line = crate::build_row(ts, host, FAMILY, &self.fields())?;
        crate::append_line(&crate::family_path(root, FAMILY), &line)
    }
}

/// A commit id: 7 to 64 hex digits. A prefix names a commit as far as it is long enough.
pub fn is_commit(s: &str) -> bool {
    (7..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whether two commit ids name the same commit, either being an abbreviation of the other.
pub fn same_commit(a: &str, b: &str) -> bool {
    is_commit(a) && is_commit(b) && (a.starts_with(b) || b.starts_with(a))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reverts_that_is_not_a_commit_id_is_refused_and_an_abbreviation_matches() {
        let rec = LandRecord { repo: "r".into(), bead: "b".into(), commit: "abcdef0123".into(), reverts: Some("Revert x".into()) };
        assert!(rec.validate().is_err());
        assert!(same_commit("abcdef0123456", "abcdef0"));
        assert!(!same_commit("abcdef0", "abcdef1"));
        assert!(!same_commit("abc", "abc"));
    }
}
