//! The per-bead verdicts the dispatcher reached on its last pass, published for the readers
//! that must not exec the claim tool: `<SPIRA_RUN>/claim/verdicts.json`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Claimable,
    Blocked,
    Held,
    Snoozed,
    TooDeep,
    StackUnchanged,
    Carried,
    Stopped,
    OutOfScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub verdict: Kind,
    #[serde(default)]
    pub blocker: Option<String>,
    /// The rune partitions that would take this bead; only a claimable bead has any.
    #[serde(default)]
    pub runes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictFile {
    pub computed_at: String,
    pub entries: Vec<Entry>,
}

pub fn path(run_dir: &Path) -> PathBuf {
    run_dir.join("claim").join("verdicts.json")
}

pub fn write(run_dir: &Path, file: &VerdictFile) -> Result<(), String> {
    let dest = path(run_dir);
    let dir = dest.parent().ok_or_else(|| format!("{} has no directory", dest.display()))?;
    let text = serde_json::to_string(file).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".verdicts.{}.tmp", std::process::id()));
    std::fs::create_dir_all(dir)
        .and_then(|_| std::fs::write(&tmp, text))
        .and_then(|_| std::fs::rename(&tmp, &dest))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("cannot write {}: {e}", dest.display())
        })
}

pub fn read(run_dir: &Path) -> Result<VerdictFile, String> {
    let p = path(run_dir);
    let text = std::fs::read_to_string(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))
}

/// `YYYY-MM-DDTHH:MM:SSZ` for an epoch.
pub fn stamp(epoch: i64) -> String {
    let z = epoch.div_euclid(86_400) + 719_468;
    let secs = epoch.rem_euclid(86_400);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs % 3600 / 60, secs % 60)
}

/// Epoch seconds of a [`stamp`]; `None` for anything else.
pub fn epoch(s: &str) -> Option<i64> {
    let n = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    if s.len() != 20 || !s.ends_with('Z') {
        return None;
    }
    let (y, mo, d, h, mi, se) = (n(0, 4)?, n(5, 7)?, n(8, 10)?, n(11, 13)?, n(14, 16)?, n(17, 19)?);
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400 + h * 3600 + mi * 60 + se)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stamp_reads_back_as_the_epoch_it_was_made_from() {
        for t in [0, 951_782_400, 1_793_398_800, 1_709_251_199] {
            assert_eq!(epoch(&stamp(t)), Some(t), "{}", stamp(t));
        }
        assert_eq!(stamp(1_793_398_800), "2026-10-30T22:20:00Z");
        assert_eq!(epoch("yesterday"), None);
    }

    #[test]
    fn a_written_file_reads_back_and_a_missing_one_says_why() {
        let dir = testkit::TempDir::new("verdicts");
        assert!(read(&dir).unwrap_err().contains("cannot read"));
        let f = VerdictFile {
            computed_at: stamp(1_793_398_800),
            entries: vec![Entry { id: "sp-p.1".into(), verdict: Kind::Blocked, blocker: Some("sp-p".into()), runes: vec![] }],
        };
        write(&dir, &f).unwrap();
        assert_eq!(read(&dir).unwrap(), f);
        assert!(std::fs::read_to_string(path(&dir)).unwrap().contains(r#""verdict":"blocked""#));
    }
}
