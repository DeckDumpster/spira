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

/// `YYYY-MM-DDTHH:MM:SSZ` for a UTC epoch-seconds value, computed in-process.
pub fn iso_of(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let rem = secs % 86_400;
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::iso_of;

    #[test]
    fn epoch_seconds_render_as_utc() {
        assert_eq!(iso_of(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_of(1_793_166_343), "2026-10-28T05:45:43Z");
    }
}
