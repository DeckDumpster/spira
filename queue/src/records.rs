//! The queue's own files (DESIGN.md §3, §4): the `open` and `publish` key=value records,
//! `round-seq`, and the atomic writer every one of them goes through.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::model::Member;

/// An ordered `key=value` record. Order and unknown keys survive a rewrite; `get` returns
/// the LAST occurrence of a key (grep | tail -1, as queue_batch_owner reads owner=) except
/// where a caller asks for the first (`get_first`, grep | head -1, as eject/abandon did).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Kv {
    pub lines: Vec<(String, String)>,
}

impl Kv {
    pub fn parse(text: &str) -> Kv {
        let lines = text
            .lines()
            .filter_map(|l| l.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
            .collect();
        Kv { lines }
    }

    pub fn get_first(&self, k: &str) -> Option<&str> {
        self.lines.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.as_str())
    }

    pub fn get(&self, k: &str) -> Option<&str> {
        self.lines.iter().rev().find(|(kk, _)| kk == k).map(|(_, v)| v.as_str())
    }

    pub fn remove(&mut self, k: &str) {
        self.lines.retain(|(kk, _)| kk != k);
    }

    pub fn push(&mut self, k: &str, v: &str) {
        self.lines.push((k.to_string(), v.to_string()));
    }

    pub fn render(&self) -> String {
        let mut s = String::new();
        for (k, v) in &self.lines {
            s.push_str(k);
            s.push('=');
            s.push_str(v);
            s.push('\n');
        }
        s
    }

    /// `members=` (first occurrence) parsed with queue.sh's own `%%:*` / `##*:` split.
    pub fn members(&self) -> Vec<Member> {
        self.get_first("members")
            .unwrap_or("")
            .split_whitespace()
            .map(Member::parse_record)
            .collect()
    }
}

/// Read a record; `Ok(None)` when the file does not exist.
pub fn read_kv(path: &Path) -> Result<Option<Kv>, String> {
    match fs::read_to_string(path) {
        Ok(t) => Ok(Some(Kv::parse(&t))),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Write `contents` to `path` atomically: a sibling temp file renamed into place, so no
/// reader (queue verdict, batcher-cut, queue-watch, cockpit) ever sees a half-written record.
pub fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("record");
    let tmp = dir.join(format!(".{name}.{}", std::process::id()));
    fs::write(&tmp, contents).map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

/// `<queue_dir>/<repo>/<name>`.
pub fn repo_file(queue_dir: &Path, repo: &str, name: &str) -> PathBuf {
    queue_dir.join(repo).join(name)
}

/// round-seq: a bare integer; anything unreadable or non-numeric is 0 (as queue.sh).
pub fn read_seq(path: &Path) -> u64 {
    fs::read_to_string(path).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

/// The open record rewritten for a claim: drop owner/pre_claim_owner/claim_reason, then
/// append the claim's three keys (queue.sh cmd_claim).
pub fn claim_rewrite(kv: &Kv, prev_owner: &str, reason: &str) -> Kv {
    let mut out = kv.clone();
    for k in ["owner", "pre_claim_owner", "claim_reason"] {
        out.remove(k);
    }
    out.push("owner", "concierge");
    out.push("pre_claim_owner", prev_owner);
    out.push("claim_reason", &one_line(reason));
    out
}

/// The open record rewritten for a release: owner restored to pre_claim_owner.
pub fn release_rewrite(kv: &Kv) -> (Kv, String) {
    let restore = kv.get("pre_claim_owner").unwrap_or("").to_string();
    let mut out = kv.clone();
    for k in ["owner", "pre_claim_owner", "claim_reason"] {
        out.remove(k);
    }
    out.push("owner", &restore);
    (out, restore)
}

/// Newlines replaced with spaces (`${x//$'\n'/ }`).
pub fn one_line(s: &str) -> String {
    s.replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::tmpdir;

    #[test]
    fn kv_roundtrip_keeps_order_and_unknown_keys() {
        let t = "pr=12\nhead=abc\nmembers=a:1 b:2\nretries=0\nowner=batcher\n";
        let kv = Kv::parse(t);
        assert_eq!(kv.render(), t);
        assert_eq!(kv.get("owner"), Some("batcher"));
        assert_eq!(kv.members().len(), 2);
    }

    #[test]
    fn claim_then_release_restores_the_owner() {
        let kv = Kv::parse("pr=1\nowner=batcher\n");
        let c = claim_rewrite(&kv, "batcher", "fix\nit");
        assert_eq!(c.render(), "pr=1\nowner=concierge\npre_claim_owner=batcher\nclaim_reason=fix it\n");
        let (r, restored) = release_rewrite(&c);
        assert_eq!(restored, "batcher");
        assert_eq!(r.render(), "pr=1\nowner=batcher\n");
    }

    #[test]
    fn owner_is_the_last_occurrence() {
        let kv = Kv::parse("owner=a\nx=1\nowner=b\n");
        assert_eq!(kv.get("owner"), Some("b"));
        assert_eq!(kv.get_first("owner"), Some("a"));
    }

    #[test]
    fn atomic_write_and_read() {
        let d = tmpdir("records");
        let p = d.join("q/spira/open");
        write_atomic(&p, "pr=1\n").unwrap();
        assert_eq!(read_kv(&p).unwrap().unwrap().get("pr"), Some("1"));
        assert!(read_kv(&d.join("nope")).unwrap().is_none());
    }

    #[test]
    fn seq_defaults_to_zero() {
        let d = tmpdir("seq");
        assert_eq!(read_seq(&d.join("round-seq")), 0);
        fs::write(d.join("round-seq"), "junk").unwrap();
        assert_eq!(read_seq(&d.join("round-seq")), 0);
        fs::write(d.join("round-seq"), "7\n").unwrap();
        assert_eq!(read_seq(&d.join("round-seq")), 7);
    }
}
