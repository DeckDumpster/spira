//! The batch key and the verdict cache (DESIGN.md §3.3). Checked before anything is built.

use crate::record::{Mode, Producer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub fn sha256_hex(data: &[u8]) -> String {
    let d = Sha256::digest(data);
    d.iter().fold(String::new(), |mut acc, b| {
        use std::fmt::Write as _;
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

/// Everything that makes two runs the same claim. Any change makes a new key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyInputs {
    pub repo_name: String,
    pub tree: String,
    pub image_tag: String,
    pub suites: Vec<String>,
    /// sha256 of the runner executable, then the `suite-select` binary (the in-process
    /// selector is linked in and the executable already covers it, sp-wx2tw; the binary is
    /// separate because testlib.sh/testenv-guard.sh/plan-lint.sh shell out to it for a
    /// suite's own header — wave 4.36, sp-bobsp, retiring suite-covers.sh's bytes here).
    pub harness_hash: String,
    pub mode: Mode,
    pub producer: Producer,
    pub profile: String,
    /// `--artifacts` only: the prebuilt set's content hash (DESIGN.md D8). None leaves the
    /// key line exactly as it was before the flag existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts_id: Option<String>,
}

impl KeyInputs {
    pub fn selection_hash(&self) -> String {
        let mut s = self.suites.clone();
        s.sort();
        let mut text = s.join("\n");
        text.push('\n');
        sha256_hex(text.as_bytes())
    }

    pub fn key(&self) -> String {
        let prebuilt = self
            .artifacts_id
            .as_deref()
            .map(|id| format!(" prebuilt={id}"))
            .unwrap_or_default();
        let line = format!(
            "{} {} {} {} {} {} {} {}{prebuilt}\n",
            self.repo_name,
            self.tree,
            self.image_tag,
            self.selection_hash(),
            self.harness_hash,
            self.mode.as_str(),
            self.producer.as_str(),
            self.profile
        );
        sha256_hex(line.as_bytes())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Green,
    Red,
    /// A green cut short by `--deadline` (DESIGN.md D7): recorded, never a cache hit.
    Partial,
}

/// `$SPIRA_VERDICTS/batch-<key>`: `key=value` lines.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictFile {
    /// Absent means an old green-only file: green.
    pub verdict: Option<Verdict>,
    pub when: Option<String>,
    pub by: Option<String>,
    pub at: Option<u64>,
    pub red_suites: Option<String>,
    pub override_reason: Option<String>,
    pub concierge_notified: Option<u64>,
    /// Suites the deadline deferred (partial only).
    pub deferred_suites: Option<String>,
}

impl VerdictFile {
    pub fn parse(text: &str) -> Self {
        let mut v = VerdictFile::default();
        for line in text.lines() {
            let Some((k, val)) = line.split_once('=') else {
                continue;
            };
            let val = val.to_string();
            match k {
                "verdict" => {
                    v.verdict = match val.as_str() {
                        "green" => Some(Verdict::Green),
                        "red" => Some(Verdict::Red),
                        "partial" => Some(Verdict::Partial),
                        _ => None,
                    }
                }
                "when" => v.when = Some(val),
                "by" => v.by = Some(val),
                "at" => v.at = val.parse().ok(),
                "red_suites" => v.red_suites = Some(val),
                "override_reason" => v.override_reason = Some(val),
                "concierge_notified" => v.concierge_notified = val.parse().ok(),
                "deferred_suites" => v.deferred_suites = Some(val),
                _ => {}
            }
        }
        v
    }

    pub fn render(&self) -> String {
        let mut s = String::new();
        let verdict = match self.verdict.unwrap_or(Verdict::Green) {
            Verdict::Green => "green",
            Verdict::Red => "red",
            Verdict::Partial => "partial",
        };
        s.push_str(&format!("verdict={verdict}\n"));
        s.push_str(&format!("when={}\n", self.when.clone().unwrap_or_default()));
        s.push_str(&format!(
            "by={}\n",
            self.by.clone().unwrap_or_else(|| "testenv-batch".into())
        ));
        s.push_str(&format!("at={}\n", self.at.unwrap_or(0)));
        if let Some(r) = &self.red_suites {
            s.push_str(&format!("red_suites={r}\n"));
        }
        if let Some(r) = &self.override_reason {
            s.push_str(&format!("override_reason={r}\n"));
        }
        if let Some(n) = self.concierge_notified {
            s.push_str(&format!("concierge_notified={n}\n"));
        }
        if let Some(d) = &self.deferred_suites {
            s.push_str(&format!("deferred_suites={d}\n"));
        }
        s
    }

    pub fn new(
        verdict: Verdict,
        when: String,
        at: u64,
        red_suites: Option<String>,
        override_reason: Option<String>,
    ) -> Self {
        VerdictFile {
            verdict: Some(verdict),
            when: Some(when),
            by: Some("testenv-batch".into()),
            at: Some(at),
            red_suites,
            override_reason,
            concierge_notified: None,
            deferred_suites: None,
        }
    }
}

/// What the cache says about this key before anything is built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheDecision {
    /// No usable prior verdict: build and run.
    Miss,
    /// This tree already passed within the TTL: exit 0 now.
    Green { when: String },
    /// A prior red within the TTL and no acceptable reason to repeat it: exit 2.
    RepeatRefused {
        when: String,
        red_suites: String,
        short_reason: bool,
        already_notified: bool,
        prior_override: Option<String>,
    },
    /// A prior red, repeated deliberately; the reason goes into the new verdict.
    RepeatAllowed { reason: String },
}

pub const MIN_REPEAT_REASON: usize = 10;

pub fn decide(
    file: Option<&VerdictFile>,
    now: u64,
    ttl: u64,
    repeat_reason: Option<&str>,
) -> CacheDecision {
    if ttl == 0 {
        return CacheDecision::Miss;
    }
    let Some(f) = file else {
        return CacheDecision::Miss;
    };
    let Some(at) = f.at else {
        return CacheDecision::Miss;
    };
    if at > now || now - at >= ttl {
        return CacheDecision::Miss;
    }
    let when = f.when.clone().unwrap_or_else(|| "unknown".into());
    match f.verdict.unwrap_or(Verdict::Green) {
        Verdict::Green => CacheDecision::Green { when },
        // A deadline-cut green proves only the suites that finished: run the key again.
        Verdict::Partial => CacheDecision::Miss,
        Verdict::Red => {
            let reason = repeat_reason.unwrap_or("");
            if reason.chars().count() >= MIN_REPEAT_REASON {
                CacheDecision::RepeatAllowed {
                    reason: reason.to_string(),
                }
            } else {
                CacheDecision::RepeatRefused {
                    when,
                    red_suites: f.red_suites.clone().unwrap_or_else(|| "(unknown)".into()),
                    short_reason: !reason.is_empty(),
                    already_notified: f.concierge_notified.is_some(),
                    prior_override: f.override_reason.clone(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> KeyInputs {
        KeyInputs {
            repo_name: "spira".into(),
            tree: "abc".into(),
            image_tag: "t1".into(),
            suites: vec!["test-b.sh".into(), "test-a.sh".into()],
            harness_hash: "h".into(),
            mode: Mode::Parallel,
            producer: Producer::Explicit,
            profile: "aeon".into(),
            artifacts_id: None,
        }
    }

    #[test]
    fn key_is_stable_and_order_insensitive_in_selection() {
        let a = inputs();
        let mut b = inputs();
        b.suites.reverse();
        assert_eq!(a.key(), b.key());
        assert_eq!(a.key().len(), 64);
    }

    #[test]
    fn every_component_moves_the_key() {
        let base = inputs().key();
        let mut v = inputs();
        v.tree = "abd".into();
        assert_ne!(base, v.key());
        let mut v = inputs();
        v.profile = "release".into();
        assert_ne!(base, v.key());
        let mut v = inputs();
        v.mode = Mode::Serial;
        assert_ne!(base, v.key());
        let mut v = inputs();
        v.producer = Producer::Diff;
        assert_ne!(base, v.key());
        let mut v = inputs();
        v.image_tag = "t2".into();
        assert_ne!(base, v.key());
    }

    #[test]
    fn a_prebuilt_set_moves_the_key_and_its_absence_does_not() {
        // the default key is the pre-D8 line, byte for byte
        let v = inputs();
        let line = format!(
            "spira abc t1 {} h parallel explicit aeon\n",
            v.selection_hash()
        );
        assert_eq!(v.key(), sha256_hex(line.as_bytes()));
        let mut p = inputs();
        p.artifacts_id = Some("id1".into());
        assert_ne!(v.key(), p.key());
        let mut q = inputs();
        q.artifacts_id = Some("id2".into());
        assert_ne!(p.key(), q.key());
    }

    #[test]
    fn selection_hash_matches_sha256sum_of_sorted_lines() {
        // printf 'test-a.sh\ntest-b.sh\n' | sha256sum
        assert_eq!(
            sha256_hex(b"test-a.sh\ntest-b.sh\n"),
            inputs().selection_hash()
        );
    }

    #[test]
    fn verdict_file_round_trips() {
        let f = VerdictFile::new(
            Verdict::Red,
            "2026-09-28T00:00:00Z".into(),
            100,
            Some("test-a.sh test-b.sh".into()),
            Some("runner destroyed mid-run".into()),
        );
        let text = f.render();
        assert!(text.starts_with("verdict=red\nwhen=2026-09-28T00:00:00Z\nby=testenv-batch\nat=100\nred_suites=test-a.sh test-b.sh\n"));
        assert_eq!(VerdictFile::parse(&text), f);
    }

    #[test]
    fn a_partial_green_round_trips_and_is_never_a_cache_hit() {
        let mut f = VerdictFile::new(Verdict::Partial, "w".into(), 100, None, None);
        f.deferred_suites = Some("test-p.sh test-z.sh".into());
        let text = f.render();
        assert!(text.starts_with("verdict=partial\n"));
        assert!(text.contains("deferred_suites=test-p.sh test-z.sh\n"));
        assert_eq!(VerdictFile::parse(&text), f);
        assert_eq!(decide(Some(&f), 150, 86400, None), CacheDecision::Miss);
        assert_eq!(
            decide(Some(&f), 150, 86400, Some("a long enough reason")),
            CacheDecision::Miss
        );
    }

    #[test]
    fn old_green_only_files_read_green() {
        let f = VerdictFile::parse("when=x\nat=100\n");
        assert_eq!(
            decide(Some(&f), 150, 86400, None),
            CacheDecision::Green { when: "x".into() }
        );
    }

    #[test]
    fn ttl_zero_or_expired_or_missing_is_a_miss() {
        let f = VerdictFile::parse("verdict=green\nat=100\n");
        assert_eq!(decide(Some(&f), 150, 0, None), CacheDecision::Miss);
        assert_eq!(
            decide(Some(&f), 100 + 86400, 86400, None),
            CacheDecision::Miss
        );
        assert_eq!(decide(None, 150, 86400, None), CacheDecision::Miss);
        assert_eq!(
            decide(
                Some(&VerdictFile::parse("verdict=green\n")),
                150,
                86400,
                None
            ),
            CacheDecision::Miss
        );
    }

    #[test]
    fn red_is_refused_without_a_sentence_and_allowed_with_one() {
        let f = VerdictFile::parse(
            "verdict=red\nwhen=w\nat=100\nred_suites=test-a.sh\nconcierge_notified=120\n",
        );
        match decide(Some(&f), 150, 86400, Some("short")) {
            CacheDecision::RepeatRefused {
                short_reason,
                already_notified,
                red_suites,
                ..
            } => {
                assert!(short_reason);
                assert!(already_notified);
                assert_eq!(red_suites, "test-a.sh");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            decide(Some(&f), 150, 86400, Some("runner was destroyed")),
            CacheDecision::RepeatAllowed {
                reason: "runner was destroyed".into()
            }
        );
    }
}
