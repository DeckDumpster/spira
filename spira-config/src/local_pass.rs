//! Recorded local passes, keyed by commit sha: what CI-bound steps (a publish PR, a release
//! cut) must find before they run. A record is a file `<run>/local-pass/<kind>/<sha>`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DIR: &str = "local-pass";
pub const OVERRIDE_ENV: &str = "SPIRA_LOCAL_PASS_OVERRIDE";
pub const MIN_OVERRIDE_REASON: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    FullSuite,
    AcceptanceAd,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "full-suite" => Some(Kind::FullSuite),
            "acceptance-ad" => Some(Kind::AcceptanceAd),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::FullSuite => "full-suite",
            Kind::AcceptanceAd => "acceptance-ad",
        }
    }

    fn producer(self) -> &'static str {
        match self {
            Kind::FullSuite => "testenv --suites <every spira/test-*.sh> <branch-at-that-commit> (a green, undeferred run records it)",
            Kind::AcceptanceAd => "acceptance-local.sh <tree-at-that-commit> --predecessor <tag> (phases A-D green records it)",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Passed,
    Overridden(String),
}

fn valid_sha(sha: &str) -> bool {
    !sha.is_empty() && sha.len() <= 64 && sha.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn record_path(run: &Path, kind: Kind, sha: &str) -> PathBuf {
    run.join(DIR).join(kind.as_str()).join(sha)
}

pub fn record(run: &Path, kind: Kind, sha: &str, producer: &str, now: &str) -> Result<(), String> {
    if !valid_sha(sha) {
        return Err(format!("local-pass: not a full commit sha: {sha:?}"));
    }
    let p = record_path(run, kind, sha);
    fs::create_dir_all(p.parent().unwrap()).map_err(|e| format!("local-pass: {}: {e}", p.display()))?;
    let tmp = p.with_extension("tmp");
    fs::write(&tmp, format!("{now} {producer}\n")).map_err(|e| format!("local-pass: {}: {e}", tmp.display()))?;
    fs::rename(&tmp, &p).map_err(|e| format!("local-pass: {}: {e}", p.display()))
}

pub fn check(run: &Path, kind: Kind, sha: &str, override_reason: Option<&str>, who: &str, now: &str) -> Result<Verdict, String> {
    if !valid_sha(sha) {
        return Err(format!("local-pass: not a full commit sha: {sha:?}"));
    }
    if record_path(run, kind, sha).is_file() {
        return Ok(Verdict::Passed);
    }
    let missing = format!(
        "no {} local-pass record for {sha} under {} — produce it with: {}. Override only with {OVERRIDE_ENV}=\"<reason, at least {MIN_OVERRIDE_REASON} chars>\" (logged)",
        kind.as_str(),
        run.join(DIR).display(),
        kind.producer()
    );
    let reason = override_reason.map(str::trim).unwrap_or("");
    if reason.chars().count() < MIN_OVERRIDE_REASON {
        return Err(missing);
    }
    let log = run.join(DIR).join("overrides.log");
    fs::create_dir_all(run.join(DIR)).map_err(|e| format!("local-pass: cannot log the override: {e}"))?;
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .map_err(|e| format!("local-pass: cannot log the override ({}): {e}", log.display()))?;
    writeln!(f, "{now} OVERRIDE kind={} sha={sha} by={who} reason={}", kind.as_str(), reason.replace('\n', " "))
        .map_err(|e| format!("local-pass: cannot log the override: {e}"))?;
    Ok(Verdict::Overridden(reason.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    const OTHER: &str = "fedcba9876543210fedcba9876543210fedcba98";

    fn run_dir(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(tag)
    }

    #[test]
    fn a_record_passes_only_its_own_commit_and_kind() {
        let d = run_dir("own");
        record(d.path(), Kind::FullSuite, SHA, "t", "now").unwrap();
        assert_eq!(check(d.path(), Kind::FullSuite, SHA, None, "me", "now"), Ok(Verdict::Passed));
        assert!(check(d.path(), Kind::FullSuite, OTHER, None, "me", "now").is_err());
        assert!(check(d.path(), Kind::AcceptanceAd, SHA, None, "me", "now").is_err());
    }

    #[test]
    fn the_refusal_names_the_missing_record_and_the_producing_command() {
        let d = run_dir("refuse");
        let e = check(d.path(), Kind::AcceptanceAd, SHA, None, "me", "now").unwrap_err();
        assert!(e.contains("acceptance-ad") && e.contains(SHA) && e.contains("acceptance-local.sh") && e.contains(OVERRIDE_ENV), "{e}");
    }

    #[test]
    fn an_override_needs_a_real_reason_and_is_logged() {
        let d = run_dir("override");
        assert!(check(d.path(), Kind::FullSuite, SHA, Some("short"), "me", "now").is_err());
        assert!(!d.path().join(DIR).join("overrides.log").exists());
        let v = check(d.path(), Kind::FullSuite, SHA, Some("ci is the only runner for this"), "me", "t0").unwrap();
        assert_eq!(v, Verdict::Overridden("ci is the only runner for this".into()));
        let log = fs::read_to_string(d.path().join(DIR).join("overrides.log")).unwrap();
        assert!(log.contains("full-suite") && log.contains(SHA) && log.contains("by=me"), "{log}");
    }

    #[test]
    fn a_non_sha_is_refused_never_used_as_a_path() {
        let d = run_dir("sha");
        assert!(record(d.path(), Kind::FullSuite, "../../x", "t", "now").is_err());
        assert!(check(d.path(), Kind::FullSuite, "a/b", None, "me", "now").is_err());
    }
}
