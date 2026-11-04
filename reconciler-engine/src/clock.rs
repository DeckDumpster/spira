//! The wall clock, read in-process — no `date` child.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `YYYY-MM-DDTHH:MM:SSZ`, UTC, the way `date -u +%Y-%m-%dT%H:%M:%SZ` printed it.
pub fn now_iso() -> String {
    iso_of(now_secs())
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
    use super::{iso_of, now_iso, now_secs};

    #[test]
    fn epoch_seconds_render_as_utc() {
        assert_eq!(iso_of(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_of(1_793_166_343), "2026-10-28T05:45:43Z");
    }

    #[test]
    fn the_clock_is_read_without_a_date_program() {
        let (before, iso, after) = (now_secs(), now_iso(), now_secs());
        assert!((iso_of(before)..=iso_of(after)).contains(&iso), "{iso}");
        assert!(before > 1_700_000_000);
    }
}
