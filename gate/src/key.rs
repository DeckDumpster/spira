//! The verdict key and the verdict cache entry (gate-lib.sh `gate_key_hash`, `cache_fresh`).

use sha2::{Digest, Sha256};

pub fn sha256_hex(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    d.iter().fold(String::with_capacity(64), |mut acc, b| {
        use std::fmt::Write;
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

/// Everything a verdict depends on. `tree` is the MERGED tree (DESIGN.md "The verdict key").
#[derive(Clone, Debug, Default)]
pub struct KeyInputs<'a> {
    pub repo: &'a str,
    pub tree: &'a str,
    pub files: &'a str,
    pub cmd: &'a str,
    pub harness_h: &'a str,
    pub suites: &'a str,
    pub bead: &'a str,
    pub ejected: &'a str,
}

/// `gate_key_hash` over the hashed inputs, byte-compatible with the bash.
pub fn gate_key(k: &KeyInputs) -> String {
    let files_h = sha256_hex(k.files.as_bytes());
    let cmd_h = sha256_hex(k.cmd.as_bytes());
    sha256_hex(
        format!(
            "{} {} {} {} {} suites={} bead={} ejected={}\n",
            k.repo, k.tree, files_h, cmd_h, k.harness_h, k.suites, k.bead, k.ejected
        )
        .as_bytes(),
    )
}

/// `cache_fresh`: Some((when, by)) when the entry's own `at=` is younger than `ttl`.
/// A non-numeric ttl or `at=` reads as stale — the cheap direction for a cache.
pub fn cache_fresh(entry: &str, ttl: &str, now: u64) -> Option<(String, String)> {
    let ttl: u64 = digits(ttl)?;
    let (mut when, mut by, mut at) = (None, None, None);
    for line in entry.lines() {
        let (k, v) = line.split_once('=').unwrap_or((line, ""));
        match k {
            "when" => when = Some(v.to_string()),
            "by" => by = Some(v.to_string()),
            "at" => at = Some(v.to_string()),
            _ => {}
        }
    }
    let at: u64 = digits(at.as_deref().unwrap_or(""))?;
    if now < at || now - at >= ttl {
        return None;
    }
    let or =
        |v: Option<String>, d: &str| v.filter(|s| !s.is_empty()).unwrap_or_else(|| d.to_string());
    Some((or(when, "an earlier time"), or(by, "unknown caller")))
}

/// The entry's last `suites=` value, `-` when absent or empty.
pub fn cached_suites(entry: &str) -> String {
    entry
        .lines()
        .filter_map(|l| l.strip_prefix("suites="))
        .last()
        .filter(|s| !s.is_empty())
        .unwrap_or("-")
        .to_string()
}

/// The file written on a PASS.
pub fn render_entry(
    when: &str,
    at: u64,
    by: &str,
    repo: &str,
    branch: &str,
    suites: &str,
) -> String {
    format!("when={when}\nat={at}\nby={by}\nrepo={repo}\nbranch={branch}\nsuites={suites}\n")
}

/// Only `[0-9]+` is a number here, as `case *[!0-9]*` had it.
pub fn digits(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> KeyInputs<'static> {
        KeyInputs {
            repo: "spira",
            tree: "t1",
            files: "a\nb",
            cmd: "bash x",
            harness_h: "h",
            suites: "on",
            bead: "sp-a",
            ejected: "",
        }
    }

    #[test]
    fn the_key_matches_the_bash_formula() {
        // printf '%s\n' "spira t1 $(printf a\\nb|sha256sum) $(printf 'bash x'|sha256sum) h suites=on bead=sp-a ejected=" | sha256sum
        let files_h = sha256_hex(b"a\nb");
        let cmd_h = sha256_hex(b"bash x");
        let want = sha256_hex(
            format!("spira t1 {files_h} {cmd_h} h suites=on bead=sp-a ejected=\n").as_bytes(),
        );
        assert_eq!(gate_key(&base()), want);
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn every_input_moves_the_key() {
        let k0 = gate_key(&base());
        let vary: Vec<KeyInputs> = vec![
            KeyInputs {
                repo: "other",
                ..base()
            },
            KeyInputs {
                tree: "t2",
                ..base()
            },
            KeyInputs {
                files: "a",
                ..base()
            },
            KeyInputs {
                cmd: "bash y",
                ..base()
            },
            KeyInputs {
                harness_h: "h2",
                ..base()
            },
            KeyInputs {
                suites: "off",
                ..base()
            },
            KeyInputs {
                bead: "sp-b",
                ..base()
            },
            KeyInputs {
                ejected: "test-x.sh",
                ..base()
            },
        ];
        for v in vary {
            assert_ne!(gate_key(&v), k0, "{v:?}");
        }
    }

    #[test]
    fn a_fresh_entry_names_when_and_by() {
        let e = render_entry(
            "2026-09-29T00:00:00Z",
            1000,
            "aeon",
            "spira",
            "spira/sp-a",
            "test-a.sh",
        );
        assert_eq!(
            cache_fresh(&e, "60", 1030),
            Some(("2026-09-29T00:00:00Z".into(), "aeon".into()))
        );
    }

    #[test]
    fn an_old_or_unreadable_entry_is_stale() {
        let e = render_entry("w", 1000, "b", "r", "br", "-");
        assert_eq!(cache_fresh(&e, "60", 1060), None, "age == ttl is stale");
        assert_eq!(cache_fresh(&e, "0", 1000), None, "ttl 0 caches nothing");
        assert_eq!(cache_fresh(&e, "6x", 1001), None, "non-numeric ttl");
        assert_eq!(cache_fresh(&e, "60", 999), None, "an entry from the future");
        assert_eq!(cache_fresh("when=w\nat=soon\n", "60", 1), None);
        assert_eq!(cache_fresh("when=w\n", "60", 1), None);
    }

    #[test]
    fn a_hostile_when_is_only_text() {
        let e = "when=$(touch /tmp/pwned)\nat=10\nby=\n";
        assert_eq!(
            cache_fresh(e, "60", 20),
            Some(("$(touch /tmp/pwned)".into(), "unknown caller".into()))
        );
    }

    #[test]
    fn cached_suites_reads_the_last_line() {
        assert_eq!(cached_suites("suites=a\nsuites=b,c\n"), "b,c");
        assert_eq!(cached_suites("suites=\n"), "-");
        assert_eq!(cached_suites("when=x\n"), "-");
    }
}
