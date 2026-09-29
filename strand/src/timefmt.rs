//! The few time conversions strand needs, without a date crate: RFC 3339 → epoch, and epoch
//! → `YYYY-MM-DDTHH:MM:SSZ` / `HH:MMZ` (UTC) / `HH:MM` (local, via libc).

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `2026-09-29T00:00:00Z`, with optional fractional seconds and a `Z` or `±HH:MM` offset.
/// None for anything else — an unparseable lease is not an expired one.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, se) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if s.as_bytes()[4] != b'-' || s.as_bytes()[7] != b'-' || !matches!(s.as_bytes()[10], b'T' | b't' | b' ') {
        return None;
    }
    let mut rest = &s[19..];
    if let Some(r) = rest.strip_prefix('.') {
        let n = r.bytes().take_while(u8::is_ascii_digit).count();
        rest = &r[n..];
    }
    let offset = match rest {
        "Z" | "z" | "" => 0,
        r if r.len() == 6 && (r.starts_with('+') || r.starts_with('-')) => {
            let sign = if r.starts_with('-') { -1 } else { 1 };
            let oh = r.get(1..3)?.parse::<i64>().ok()?;
            let om = r.get(4..6)?.parse::<i64>().ok()?;
            sign * (oh * 3600 + om * 60)
        }
        _ => return None,
    };
    Some(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se - offset)
}

/// `YYYY-MM-DDTHH:MM:SSZ`.
pub fn utc_stamp(t: i64) -> String {
    let (y, m, d) = civil_from_days(t.div_euclid(86400));
    let s = t.rem_euclid(86400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s % 3600 / 60, s % 60)
}

/// `HH:MMZ`.
pub fn utc_hhmm(t: i64) -> String {
    let s = t.rem_euclid(86400);
    format!("{:02}:{:02}Z", s / 3600, s % 3600 / 60)
}

/// `HH:MM` in the host's local zone (what `date -d @t +%H:%M` printed).
pub fn local_hhmm(t: i64) -> String {
    let tt = t as libc::time_t;
    // SAFETY: localtime_r writes into the tm we own; a zeroed tm is a valid initial value.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&tt, &mut tm).is_null() };
    if !ok {
        return utc_hhmm(t);
    }
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

pub fn now() -> i64 {
    if let Some(n) = std::env::var("SPIRA_NOW").ok().and_then(|v| v.parse().ok()) {
        return n;
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let t = parse_rfc3339("2026-09-29T01:02:03Z").unwrap();
        assert_eq!(utc_stamp(t), "2026-09-29T01:02:03Z");
        assert_eq!(utc_hhmm(t), "01:02Z");
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-09-29T01:02:03.123456Z"), Some(t));
        assert_eq!(parse_rfc3339("2026-09-28T18:02:03-07:00"), Some(t));
        assert_eq!(parse_rfc3339("garbage"), None);
        assert_eq!(parse_rfc3339(""), None);
    }
}
