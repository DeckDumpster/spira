//! `epoch(t)` from forge.sh's inline Python: parses `YYYY-MM-DDTHH:MM:SS[.ffffff]Z` (GitHub's
//! timestamp shape, `Z` stripped, fractional seconds ignored) into Unix seconds. Any other
//! shape, or empty, is 0 — never an error, matching the Python's `except: return 0`.

/// Days since the Unix epoch for a given civil date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn epoch(t: &str) -> u64 {
    let t = t.trim();
    let t = t.strip_suffix('Z').unwrap_or(t);
    // Cut any fractional-second suffix the way Python's strptime with '%Y-%m-%dT%H:%M:%S'
    // would refuse (forge.sh's format has no %f) — GitHub timestamps here carry none, but
    // fail closed (0) on anything unexpected rather than misparse.
    let bytes = t.as_bytes();
    if bytes.len() != 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' || bytes[13] != b':' || bytes[16] != b':' {
        return 0;
    }
    let num = |s: &str| s.parse::<i64>().ok();
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(s)) =
        (num(&t[0..4]), num(&t[5..7]), num(&t[8..10]), num(&t[11..13]), num(&t[14..16]), num(&t[17..19]))
    else {
        return 0;
    };
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || !(0..=23).contains(&h) || !(0..=59).contains(&mi) || !(0..=60).contains(&s) {
        return 0;
    }
    let days = days_from_civil(y, mo, d);
    let secs = days * 86_400 + h * 3600 + mi * 60 + s;
    if secs < 0 {
        0
    } else {
        secs as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_timestamps() {
        assert_eq!(epoch("2026-09-29T00:00:00Z"), 1_790_640_000);
        assert_eq!(epoch(""), 0);
        assert_eq!(epoch("garbage"), 0);
        assert_eq!(epoch("1970-01-01T00:00:00Z"), 0);
    }
}
