//! The wall clock, asked of `date` by name so a caller's injected program is its clock seam.

use std::time::{SystemTime, UNIX_EPOCH};

fn date(program: &str, format: &str) -> Option<String> {
    let out = spira_config::bounded::bounded(program).args(["-u", format]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn now_secs(date_bin: &str) -> u64 {
    date(date_bin, "+%s")
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
}

pub fn now_iso(date_bin: &str) -> String {
    date(date_bin, "+%Y-%m-%dT%H:%M:%SZ").unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}
