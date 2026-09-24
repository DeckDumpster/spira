//! The /api/ops route — reads the collector's snapshot files and serves them as JSON.
//!
//! NEVER CALLS bd, git OR A PROBE. That is health.sh's own rule and the reason the
//! collector exists: those sources cost seconds a pass and a pane that shelled out would
//! freeze on every repaint. This route reads two small files, checks two stamps, and calls
//! systemctl once — all inside the 5-second cache.
//!
//! PARSE cockpit.env; DO NOT SOURCE IT. The format is KEY='VALUE' with '\'' for an embedded
//! apostrophe. A line that does not match is skipped, never guessed at.
//!
//! A KEY ABSENT FROM THE FILE IS ABSENT FROM THE JSON. The renderer prints ? for an absent
//! key rather than substituting 0 — a panel that reports a broken check as all-clear
//! displaces the suspicion that would have prompted a look.

use serde_json::{Map, Value};
use std::collections::HashMap;
use std::time::{Instant, SystemTime};
use tokio::process::Command;

/// Parse a shell single-quoted value of the form `'VALUE'`, where embedded apostrophes
/// are encoded as `'\''` (close-quote, backslash-apostrophe, open-quote).
///
/// Returns `None` if the string does not begin with `'`.
/// A line that does not parse cleanly is dropped rather than guessed at.
pub fn parse_shell_value(s: &str) -> Option<String> {
    if !s.starts_with('\'') {
        return None;
    }
    let mut result = String::new();
    let mut chars = s.chars();
    chars.next(); // consume the opening '

    loop {
        match chars.next() {
            None => return None, // unterminated — malformed
            Some('\'') => {
                // Closing quote. A '\'' sequence immediately following means an embedded
                // apostrophe: the shell convention is close-quote, backslash-quote,
                // open-quote. Consume the \ and the literal ', then open the next section.
                let mut peek = chars.clone();
                if peek.next() == Some('\\') && peek.next() == Some('\'') {
                    chars.next(); // consume \
                    chars.next(); // consume ' (the literal apostrophe)
                    result.push('\'');
                    // The next char should open the next single-quoted section.
                    let mut peek2 = chars.clone();
                    if peek2.next() == Some('\'') {
                        chars.next(); // consume the opening ' of the next section
                    } else {
                        // Embedded apostrophe at the very end — valid.
                        break;
                    }
                } else {
                    // Normal end of value.
                    break;
                }
            }
            Some(c) => result.push(c),
        }
    }
    Some(result)
}

/// Parse a KEY='VALUE' shell env file. Returns (data, age_s, error).
///
/// Only SP_* keys are captured. cockpit.sh and budget.sh both use this format.
pub fn parse_env_file(path: &str) -> (HashMap<String, String>, Option<u64>, Option<String>) {
    let age_s = std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|mtime| SystemTime::now().duration_since(mtime).ok())
        .map(|d| d.as_secs());

    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => return (HashMap::new(), age_s, Some(e.to_string())),
    };

    let mut data = HashMap::new();
    for line in content.lines() {
        let Some(eq) = line.find('=') else { continue };
        let key = line[..eq].trim();
        if !key.starts_with("SP_") {
            continue;
        }
        if let Some(value) = parse_shell_value(&line[eq + 1..]) {
            data.insert(key.to_string(), value);
        }
    }
    (data, age_s, None)
}

/// The HALT and DRAIN world state, read directly from stamps — not from the snapshot.
///
/// A dead collector would render a halted world as healthy if the banner read the snapshot
/// instead of the stamps. Two file checks and one systemctl call, inside the 5-second cache.
pub struct WorldState {
    pub halted: bool,
    pub halted_since: Option<String>,
    pub halted_why: Option<String>,
    pub draining: bool,
    pub draining_age_s: Option<u64>,
    pub sentinel_active: Option<bool>,
}

pub async fn read_world_state(run: &str, instance: &str, systemctl: &str) -> WorldState {
    let halted_path = format!("{run}/world.halted");
    let halted = std::path::Path::new(&halted_path).exists();
    let (halted_since, halted_why) = if halted {
        match std::fs::read_to_string(&halted_path) {
            Ok(s) => {
                let mut lines = s.lines();
                let since = lines.next().map(str::to_string);
                let why = lines
                    .find(|l| l.starts_with("why: "))
                    .map(|l| l[5..].to_string());
                (since, why)
            }
            Err(_) => (None, None),
        }
    } else {
        (None, None)
    };

    let draining_path = format!("{run}/world.draining");
    let draining_meta = std::fs::metadata(&draining_path);
    let draining = draining_meta.is_ok();
    let draining_age_s = draining_meta
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|mtime| SystemTime::now().duration_since(mtime).ok())
        .map(|d| d.as_secs());

    // Try the instance-suffixed timer name first, then the plain one — same order health.sh uses.
    let sentinel_active = check_sentinel(instance, systemctl).await;

    WorldState {
        halted,
        halted_since,
        halted_why,
        draining,
        draining_age_s,
        sentinel_active,
    }
}

async fn check_sentinel(instance: &str, systemctl: &str) -> Option<bool> {
    let names = [
        format!("spira-sentinel-{instance}.timer"),
        "spira-sentinel.timer".to_string(),
    ];
    let mut found_inactive = false;
    for name in &names {
        match Command::new(systemctl)
            .args(["--user", "is-active", name])
            .output()
            .await
        {
            Ok(out) => {
                let state = String::from_utf8_lossy(&out.stdout);
                let state = state.trim();
                if state == "active" {
                    return Some(true);
                }
                if !state.is_empty() {
                    found_inactive = true;
                }
            }
            Err(_) => {}
        }
    }
    if found_inactive {
        Some(false)
    } else {
        None // systemctl not available or returned nothing
    }
}

/// One cached read of the ops endpoint.
pub struct OpsSnapshot {
    pub taken: Instant,
    pub body: String,
}

impl OpsSnapshot {
    pub async fn take(run: &str, instance: &str, systemctl: &str) -> OpsSnapshot {
        let (mut data, cockpit_age_s, cockpit_error) =
            parse_env_file(&format!("{run}/cockpit.env"));
        let (budget_data, budget_age_s, budget_error) =
            parse_env_file(&format!("{run}/budget.env"));
        // budget.env keys merged in; cockpit.env wins on conflicts — cockpit.sh already
        // sourced budget.env, so its snapshot contains those values with the same logic applied.
        for (k, v) in budget_data {
            data.entry(k).or_insert(v);
        }
        let world = read_world_state(run, instance, systemctl).await;

        let mut obj = Map::new();
        for (k, v) in &data {
            obj.insert(k.clone(), Value::String(v.clone()));
        }
        obj.insert(
            "cockpit_env_age_s".into(),
            cockpit_age_s.map_or(Value::Null, |a| a.into()),
        );
        obj.insert(
            "cockpit_env_error".into(),
            cockpit_error.map_or(Value::Null, Value::String),
        );
        obj.insert(
            "budget_env_age_s".into(),
            budget_age_s.map_or(Value::Null, |a| a.into()),
        );
        obj.insert(
            "budget_env_error".into(),
            budget_error.map_or(Value::Null, Value::String),
        );
        obj.insert("halted".into(), Value::Bool(world.halted));
        if let Some(s) = world.halted_since {
            obj.insert("halted_since".into(), Value::String(s));
        }
        if let Some(w) = world.halted_why {
            obj.insert("halted_why".into(), Value::String(w));
        }
        obj.insert("draining".into(), Value::Bool(world.draining));
        if let Some(age) = world.draining_age_s {
            obj.insert("draining_age_s".into(), age.into());
        }
        obj.insert(
            "sentinel_active".into(),
            world.sentinel_active.map_or(Value::Null, Value::Bool),
        );

        OpsSnapshot {
            taken: Instant::now(),
            body: serde_json::to_string(&Value::Object(obj))
                .unwrap_or_else(|_| "{}".to_string()),
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────
//
// Every assertion that something is ABSENT pairs with one showing the same check can find
// something present: a test that cannot fail when the implementation is wrong is no test.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_value() {
        assert_eq!(parse_shell_value("'hello'"), Some("hello".into()));
        assert_eq!(parse_shell_value("''"), Some("".into()));
    }

    #[test]
    fn parse_embedded_apostrophe() {
        // 'a'\''b' encodes a'b — close-quote, backslash-apostrophe, open-quote.
        assert_eq!(parse_shell_value("'a'\\''b'"), Some("a'b".into()));
        // POSITIVE CONTROL: 'a' alone returns "a", not "a'b".
        assert_ne!(parse_shell_value("'a'"), Some("a'b".into()));
    }

    #[test]
    fn parse_single_apostrophe_value() {
        // Python encode("'") = "''\\'''" (6 chars: ' ' \ ' ' ')
        // value of exactly one apostrophe
        let encoded = "''\\'''";
        assert_eq!(
            parse_shell_value(encoded),
            Some("'".into()),
            "a value of one apostrophe must round-trip"
        );
        // POSITIVE CONTROL: without the embedded-apostrophe branch, this would return
        // Some("") (stopping at char 1). Verify empty stays empty:
        assert_eq!(parse_shell_value("''"), Some("".into()));
    }

    #[test]
    fn parse_two_apostrophes_roundtrips() {
        // Python encode("''") = "''\\'''\\'''" (10 chars: ' ' \ ' ' ' \ ' ' ')
        // This is the live SP_AURON_KEYS value from the cockpit.env on this box.
        let encoded = "''\\'''\\'''";
        assert_eq!(
            parse_shell_value(encoded),
            Some("''".into()),
            "two apostrophes must round-trip"
        );
        // POSITIVE CONTROL: an empty value must not return two apostrophes.
        assert_ne!(
            parse_shell_value("''"),
            Some("''".into()),
            "positive control: plain empty value is not two apostrophes"
        );
    }

    #[test]
    fn non_sp_keys_are_skipped() {
        let tmp = std::env::temp_dir().join(format!("loom-ops-{}.env", std::process::id()));
        std::fs::write(&tmp, "SP_OPEN='5'\nOTHER='x'\nSP_READY='3'\n").unwrap();
        let (data, _, err) = parse_env_file(tmp.to_str().unwrap());
        std::fs::remove_file(&tmp).ok();
        assert!(err.is_none());
        assert_eq!(data.get("SP_OPEN"), Some(&"5".to_string()));
        assert_eq!(data.get("SP_READY"), Some(&"3".to_string()));
        // POSITIVE CONTROL: SP_OPEN was found, so the parser ran.
        assert!(
            data.get("OTHER").is_none(),
            "non-SP_ key must not appear in data map"
        );
    }

    #[test]
    fn absent_key_not_in_map() {
        // A key absent from the file must be absent from the map. The renderer prints ?
        // for a missing key — it must never receive a default 0.
        let tmp =
            std::env::temp_dir().join(format!("loom-ops2-{}.env", std::process::id()));
        std::fs::write(&tmp, "SP_OPEN='5'\n").unwrap();
        let (data, _, _) = parse_env_file(tmp.to_str().unwrap());
        std::fs::remove_file(&tmp).ok();
        // POSITIVE CONTROL: SP_OPEN is present, confirming the parser ran.
        assert!(data.contains_key("SP_OPEN"), "positive control: parser ran");
        assert!(
            !data.contains_key("SP_MISSING_KEY"),
            "absent key must not appear in map"
        );
    }

    #[test]
    fn unreadable_file_returns_error() {
        let (_, _, err) = parse_env_file("/nonexistent/path/cockpit.env");
        assert!(err.is_some(), "unreadable file must produce an error string");
    }

    #[tokio::test]
    async fn read_world_state_reflects_the_stamp_not_the_snapshot() {
        // A dead collector would render a halted world as healthy if the banner read the
        // snapshot instead of the stamp — this is the property that makes read_world_state
        // its own read path rather than a field on OpsSnapshot.
        let dir = std::env::temp_dir().join(format!("ops-world-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let run = dir.to_str().unwrap();

        // POSITIVE CONTROL FIRST: no stamp, not halted — the flip below means something only
        // because this baseline is not already halted.
        let w = read_world_state(run, "test", "/nonexistent/systemctl").await;
        assert!(!w.halted, "no world.halted stamp: expected not halted");
        assert!(
            w.sentinel_active.is_none(),
            "a missing systemctl must not fabricate active/inactive"
        );

        std::fs::write(dir.join("world.halted"), "2026-09-24T00:00:00Z\nwhy: acceptance\n")
            .unwrap();
        let w = read_world_state(run, "test", "/nonexistent/systemctl").await;
        assert!(w.halted, "the stamp is present: expected halted");
        assert_eq!(w.halted_since.as_deref(), Some("2026-09-24T00:00:00Z"));
        assert_eq!(w.halted_why.as_deref(), Some("acceptance"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
