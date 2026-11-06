//! The moot-sweep, in-process (was `cockpit/moot-sweep.sh --apply`, which sourced conf.sh
//! through db.sh and filtered with an inline python3): resolve the asks whose `MOOT-WHEN:`
//! predicate now exits 0.
//!
//! Three invariants, unchanged. An ask with no MOOT-WHEN is never touched. A predicate that
//! errors or times out leaves the ask open (a failed probe never reads as cleared). Resolution
//! goes through `resolve`, never a raw close.

use std::path::Path;
use std::process::{Command, Stdio};

pub struct MootCfg {
    /// `COCKPIT_DB`: the beads database, verified to hold `.beads` before anything is read.
    pub db: Option<String>,
    pub lc: spira_config::lc_call::LcClient,
    pub ask_label: String,
    /// `SPIRA_MOOT_TIMEOUT`: each predicate's bound, seconds.
    pub timeout_s: u64,
    /// `SPIRA_RESOLVE_SH`: the `resolve` program (a test hands in a recorder).
    pub resolve: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub found: u32,
    pub resolved: u32,
    pub errors: u32,
}

/// The asks a sweep checks: ask-labelled, `open`, and carrying a predicate. `rows` is
/// `content list --json` output, any warning lines before the payload skipped.
pub fn asks_of(rows: &str, ask_label: &str) -> Vec<(String, String)> {
    let start = rows.lines().position(|l| l.starts_with('[') || l.starts_with('{'));
    let Some(start) = start else { return Vec::new() };
    let payload = rows.lines().skip(start).collect::<Vec<_>>().join("\n");
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&payload) else { return Vec::new() };
    let rows = match &doc {
        serde_json::Value::Array(a) => a.clone(),
        v => v.get("issues").and_then(|i| i.as_array()).cloned().unwrap_or_default(),
    };
    let mut out = Vec::new();
    for r in rows {
        let labelled = r.get("labels").and_then(|l| l.as_array()).is_some_and(|l| l.iter().any(|x| x.as_str() == Some(ask_label)));
        if !labelled || r.get("status").and_then(|s| s.as_str()).unwrap_or("") != "open" {
            continue;
        }
        let desc = r.get("description").and_then(|d| d.as_str()).unwrap_or("");
        if let Some(pred) = desc.lines().find_map(moot_when) {
            out.push((r.get("id").map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).unwrap_or_default(), pred));
        }
    }
    out
}

/// `^\s*MOOT-WHEN:\s*(.+)$` on one line, the group stripped.
fn moot_when(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix("MOOT-WHEN:")?;
    (!rest.is_empty()).then(|| rest.trim().to_string())
}

/// The first 300 bytes of a predicate's output, as `head -c 300` cuts them.
fn short(out: &[u8]) -> String {
    let trimmed = {
        let mut end = out.len();
        while end > 0 && out[end - 1] == b'\n' {
            end -= 1;
        }
        &out[..end]
    };
    String::from_utf8_lossy(&trimmed[..trimmed.len().min(300)]).into_owned()
}

/// One predicate's exit code and combined output; `None` when it could not be run.
fn check(cfg: &MootCfg, pred: &str) -> Option<(i32, Vec<u8>)> {
    let path = std::env::temp_dir().join(format!("watchtower-moot-{}-{:?}.out", std::process::id(), std::thread::current().id()));
    let file = std::fs::File::create(&path).ok()?;
    let err = file.try_clone().ok()?;
    // FORK-EDGE: operator predicate — MOOT-WHEN is a shell command by contract (W1 allow-list: bash)
    let status = Command::new("timeout")
        .arg(cfg.timeout_s.to_string())
        .arg("bash")
        .arg("-c")
        .arg(pred)
        .stdin(Stdio::null())
        .stdout(file)
        .stderr(err)
        .status();
    let out = std::fs::read(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    Some((status.ok()?.code().unwrap_or(-1), out))
}

/// The open asks' rows, or `None` when the store did not answer.
fn rows(cfg: &MootCfg) -> Option<String> {
    // db.sh read stdout whatever the exit code, so any answer carrying a payload is read.
    let labels = format!("insight,{},overseer", cfg.ask_label);
    let out = cfg.lc.call(&["content", "list", "--all", "--limit", "0", "--label-any", &labels, "--json"]).ok()?.out;
    Some(out).filter(|t| t.lines().any(|l| l.starts_with('[') || l.starts_with('{')))
}

/// One pass; with `apply`, each cleared ask is resolved.
pub fn sweep(cfg: &MootCfg, apply: bool) -> Summary {
    let mut s = Summary::default();
    let Some(_db) = cfg.db.as_deref().filter(|d| Path::new(d).join(".beads").is_dir()) else { return s };
    let Some(rows) = rows(cfg) else { return s };
    for (id, pred) in asks_of(&rows, &cfg.ask_label) {
        if id.is_empty() {
            continue;
        }
        s.found += 1;
        match check(cfg, &pred) {
            Some((0, out)) if apply => {
                let reason = format!("auto-resolved: MOOT-WHEN predicate cleared — {pred} → exit 0. Evidence: {}", short(&out));
                let ok = spira_config::bounded::bounded(&cfg.resolve).arg(&id).arg(&reason).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|st| st.success());
                if ok {
                    s.resolved += 1;
                } else {
                    s.errors += 1;
                }
            }
            Some((124, _)) => s.errors += 1,
            _ => {}
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(id: &str, status: &str, labels: &[&str], desc: &str) -> serde_json::Value {
        serde_json::json!({"id": id, "status": status, "labels": labels, "description": desc})
    }

    #[test]
    fn only_open_labelled_asks_with_a_predicate_are_checked() {
        let rows = serde_json::Value::Array(vec![
            // literal-ok: test fixture label
            ask("sp-1", "open", &["needs-ryan"], "why\n  MOOT-WHEN:  test -e /x  \nmore"),
            // literal-ok: test fixture label
            ask("sp-2", "open", &["needs-ryan"], "no predicate"),
            // literal-ok: test fixture label
            ask("sp-3", "closed", &["needs-ryan"], "MOOT-WHEN: true"),
            ask("sp-4", "open", &["insight"], "MOOT-WHEN: true"),
        ]);
        let text = format!("warning: something\n{rows}");
        // literal-ok: test fixture label
        assert_eq!(asks_of(&text, "needs-ryan"), vec![("sp-1".to_string(), "test -e /x".to_string())]);
        // literal-ok: test fixture label
        assert!(asks_of("not json", "needs-ryan").is_empty());
    }

    #[test]
    fn a_cleared_predicate_is_resolved_a_live_one_kept_and_a_timeout_counted() {
        let d = testkit::TempDir::new("wt-moot");
        std::fs::create_dir_all(d.join("db/.beads")).unwrap();
        let rows = serde_json::Value::Array(vec![
            // literal-ok: test fixture label
            ask("sp-c", "open", &["needs-ryan"], "MOOT-WHEN: echo gone"),
            // literal-ok: test fixture label
            ask("sp-l", "open", &["needs-ryan"], "MOOT-WHEN: false"),
            // literal-ok: test fixture label
            ask("sp-t", "open", &["needs-ryan"], "MOOT-WHEN: sleep 5"),
        ]);
        let (t, lc) = crate::lc::scripted::scripted(0, &rows.to_string());
        testkit::write_exe(d.join("resolve"), &format!("#!/bin/sh\nprintf '%s|%s\\n' \"$1\" \"$2\" >> {}\n", d.join("resolved").display()));
        let cfg = MootCfg {
            db: Some(d.join("db").to_string_lossy().into_owned()),
            lc,
            // literal-ok: test fixture label
            ask_label: "needs-ryan".into(),
            timeout_s: 1,
            resolve: d.join("resolve").to_string_lossy().into_owned(),
        };
        assert_eq!(sweep(&cfg, true), Summary { found: 3, resolved: 1, errors: 1 });
        let resolved = std::fs::read_to_string(d.join("resolved")).unwrap();
        assert_eq!(resolved, "sp-c|auto-resolved: MOOT-WHEN predicate cleared — echo gone → exit 0. Evidence: gone\n");
        // literal-ok: test fixture label
        assert_eq!(t.seen.lock().unwrap()[0], vec!["content", "list", "--all", "--limit", "0", "--label-any", "insight,needs-ryan,overseer", "--json"]);
    }

    #[test]
    fn no_database_checks_nothing() {
        let cfg = MootCfg { db: Some("/does/not/exist".into()), lc: crate::lc::scripted::scripted(0, "[]").1, ask_label: "a".into(), timeout_s: 1, resolve: "/bin/false".into() };
        assert_eq!(sweep(&cfg, true), Summary::default());
    }
}
