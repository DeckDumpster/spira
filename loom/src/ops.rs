//! The /api/ops route — reads the collector's snapshot files and serves them as JSON.
//!
//! NEVER CALLS bd, git OR A PROBE. That is health.sh's own rule and the reason the
//! collector exists: those sources cost seconds a pass and a pane that shelled out would
//! freeze on every repaint. This route reads one small file, checks its stamp, and calls
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
/// Only SP_* keys are captured. `cockpit-collect` (sp-kt4l3; formerly `spira/cockpit.sh`)
/// uses this format.
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

pub async fn read_world_state(run: &str, instance: &str, units: Option<&dyn runtime::units::Units>) -> WorldState {
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
    let sentinel_active = units.and_then(|u| check_sentinel(u, instance));

    WorldState {
        halted,
        halted_since,
        halted_why,
        draining,
        draining_age_s,
        sentinel_active,
    }
}

/// Whether the sentinel's timer (the instance's, else the plain one) is active, through the
/// `Units` port. A unit the manager knows nothing of reads inactive, as `is-active` said;
/// `None` when neither could be read at all.
pub fn check_sentinel(units: &dyn runtime::units::Units, instance: &str) -> Option<bool> {
    let names = [format!("spira-sentinel-{instance}.timer"), "spira-sentinel.timer".to_string()];
    let mut found_inactive = false;
    for name in &names {
        if let Ok(f) = units.facts(&runtime::units::UnitHandle::named(name)) {
            if f.state == runtime::units::UnitState::Active {
                return Some(true);
            }
            found_inactive = true;
        }
    }
    found_inactive.then_some(false)
}

/// One cached read of the ops endpoint.
pub struct OpsSnapshot {
    pub taken: Instant,
    pub obj: Map<String, Value>,
}

fn is_claim_key(k: &str) -> bool {
    let row = k.strip_prefix("SP_NEXT").map(|r| r.strip_suffix("_REPO").unwrap_or(r));
    matches!(k, "SP_READY" | "SP_SLOTS_READY") || row.is_some_and(|r| r == "_N" || (!r.is_empty() && r.bytes().all(|b| b.is_ascii_digit())))
}

/// The claimable count and the NEXT rows as the claim view answers them: the one `View` the
/// pane draws and the Now page counts, so N, READY and the rows cannot disagree.
pub fn claim_keys(snap: &cockpit_ops::lcview::Snapshot) -> Vec<(String, String)> {
    const MAX_NEXT_ROWS: usize = 5;
    let v = cockpit_ops::lcview::view(snap);
    let n = if v.next_unknown { "?".to_string() } else { v.next_count.to_string() };
    let mut out = vec![("SP_NEXT_N".to_string(), n.clone()), ("SP_READY".to_string(), n.clone()), ("SP_SLOTS_READY".to_string(), n)];
    for (i, it) in v.next.iter().take(MAX_NEXT_ROWS).enumerate() {
        out.push((format!("SP_NEXT{i}"), format!("{} {} {} {}", it.prio, it.persona, it.id, it.title)));
        out.push((format!("SP_NEXT{i}_REPO"), it.repo.clone()));
    }
    out
}

impl OpsSnapshot {
    pub async fn take(run: &str, instance: &str, units: Option<&dyn runtime::units::Units>) -> OpsSnapshot {
        let (data, cockpit_age_s, cockpit_error) =
            parse_env_file(&format!("{run}/cockpit.env"));
        let world = read_world_state(run, instance, units).await;

        let mut obj = Map::new();
        for (k, v) in data.iter().filter(|(k, _)| !is_claim_key(k)) {
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

        OpsSnapshot { taken: Instant::now(), obj }
    }

    /// The body, with the claimable keys read from `claim` now; with none, they are absent
    /// and the renderer shows `?`.
    pub fn body(&self, claim: Option<&cockpit_ops::lcview::Snapshot>) -> String {
        let mut obj = self.obj.clone();
        for (k, v) in claim.map(claim_keys).unwrap_or_default() {
            obj.insert(k, Value::String(v));
        }
        serde_json::to_string(&Value::Object(obj)).unwrap_or_else(|_| "{}".to_string())
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
        // This is the live SP_HEIMDALL_KEYS value from the cockpit.env on this box.
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
        let d = testkit::TempDir::new("loom-ops");
        let tmp = d.join("ops.env");
        std::fs::write(&tmp, "SP_OPEN='5'\nOTHER='x'\nSP_READY='3'\n").unwrap();
        let (data, _, err) = parse_env_file(tmp.to_str().unwrap());
        std::fs::remove_file(&tmp).ok();
        assert!(err.is_none());
        assert_eq!(data.get("SP_OPEN"), Some(&"5".to_string()));
        assert_eq!(data.get("SP_READY"), Some(&"3".to_string()));
        // POSITIVE CONTROL: SP_OPEN was found, so the parser ran.
        assert!(
            !data.contains_key("OTHER"),
            "non-SP_ key must not appear in data map"
        );
    }

    #[test]
    fn absent_key_not_in_map() {
        // A key absent from the file must be absent from the map. The renderer prints ?
        // for a missing key — it must never receive a default 0.
        let d = testkit::TempDir::new("loom-ops2");
        let tmp = d.join("ops.env");
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

    use cockpit_ops::lcview::{apply_claim_views, ClaimView, Meta, Row, Snapshot};

    fn claim_snapshot(ids: &[&str]) -> Snapshot {
        let mut s = Snapshot::default();
        for (n, id) in ids.iter().enumerate() {
            let claim = ClaimView { verdict: spira_claim::verdicts::Kind::Claimable, blocker: None, runes: vec!["builder".into()] };
            s.rows.push(Row { id: id.to_string(), state: "READY".into(), claim: Some(claim), persona: Some("builder".into()), ..Default::default() });
            s.meta.insert(id.to_string(), Meta { title: format!("title {id}"), priority: Some(n as i64), ..Default::default() });
        }
        apply_claim_views(&mut s);
        s
    }

    fn json_of(body: &str) -> Map<String, Value> {
        serde_json::from_str::<Value>(body).unwrap().as_object().unwrap().clone()
    }

    fn rows_in(o: &Map<String, Value>) -> usize {
        (0..10).filter(|i| o.contains_key(&format!("SP_NEXT{i}"))).count()
    }

    #[tokio::test]
    async fn ops_answers_claimable_keys_from_the_claim_view_not_the_collectors_copy() {
        let d = testkit::TempDir::new("loom-ops-claim");
        let stale = "SP_NEXT_N='1'\nSP_READY='1'\nSP_SLOTS_READY='1'\nSP_NEXT0='P0 old sp-o x'\nSP_NEXT1='P0 old sp-p x'\nSP_NEXT2='P0 old sp-q x'\nSP_NEXT2_REPO='r'\nSP_OPEN='5'\n";
        std::fs::write(d.join("cockpit.env"), stale).unwrap();
        let snap = OpsSnapshot::take(d.path().to_str().unwrap(), "t", None).await;

        let four = json_of(&snap.body(Some(&claim_snapshot(&["sp-a", "sp-b", "sp-c", "sp-d"]))));
        assert_eq!((four["SP_NEXT_N"].as_str(), four["SP_READY"].as_str(), four["SP_SLOTS_READY"].as_str()), (Some("4"), Some("4"), Some("4")));
        assert_eq!(rows_in(&four), 4);
        assert!(four["SP_NEXT0"].as_str().unwrap().contains("sp-a"), "{four:?}");
        assert_eq!(four["SP_OPEN"], "5", "keys that are not claimability pass through");

        let one = json_of(&snap.body(Some(&claim_snapshot(&["sp-a"]))));
        assert_eq!(one["SP_NEXT_N"], "1");
        assert_eq!(rows_in(&one), 1, "no row outlives a shorter set: {one:?}");
        assert!(!one.contains_key("SP_NEXT2_REPO"));

        let blind = json_of(&snap.body(None));
        assert!(!blind.contains_key("SP_NEXT_N") && !blind.contains_key("SP_READY") && rows_in(&blind) == 0, "{blind:?}");
    }
}
