//! Parses the fixed `[YYYY-MM-DDTHH:MM:SSZ]` stamp a watcher may prefix its own lines with
//! (`notify`'s "producer's own clock"). No date/time crate is vendored in this workspace's
//! registry cache, so this is Howard Hinnant's `days_from_civil` — a well-known,
//! allocation-free proleptic-Gregorian day count — rather than a dependency.

pub fn parse_utc(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' || b[19] != b'Z' {
        return None;
    }
    let num = |a: usize, c: usize| -> Option<i64> { std::str::from_utf8(&b[a..a + c]).ok()?.parse().ok() };
    let y = num(0, 4)?;
    let mo = num(5, 2)?;
    let d = num(8, 2)?;
    let h = num(11, 2)?;
    let mi = num(14, 2)?;
    let se = num(17, 2)?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    Some(days * 86400 + h * 3600 + mi * 60 + se)
}

/// Days since 1970-01-01, proleptic Gregorian. https://howardhinnant.github.io/date_algorithms.html
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The inverse of `days_from_civil` — https://howardhinnant.github.io/date_algorithms.html
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

/// Renders `[YYYY-MM-DDTHH:MM:SSZ]`'s bare form (no brackets) for a `watchd tail` resume
/// message — the only place this binary prints a timestamp of its own.
pub fn format_utc(epoch: i64) -> String {
    let days = epoch.div_euclid(86400);
    let secs_of_day = epoch.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let h = secs_of_day / 3600;
    let mi = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_itself() {
        assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn a_known_date() {
        // 2026-09-30T00:00:00Z — cross-checked against `date -u -d ... +%s`.
        assert_eq!(parse_utc("2026-09-30T00:00:00Z"), Some(1790726400));
    }

    #[test]
    fn one_second_later() {
        assert_eq!(parse_utc("2026-09-30T00:00:01Z"), Some(1790726401));
    }

    #[test]
    fn a_handful_more_cross_checked_dates() {
        for (s, want) in [
            ("2000-01-01T00:00:00Z", 946684800i64),
            ("2024-02-29T12:34:56Z", 1709210096),
            ("1999-12-31T23:59:59Z", 946684799),
        ] {
            assert_eq!(parse_utc(s), Some(want), "{s}");
        }
    }

    #[test]
    fn format_round_trips_through_parse() {
        for s in ["1970-01-01T00:00:00Z", "2026-09-30T00:00:00Z", "2024-02-29T12:34:56Z"] {
            let e = parse_utc(s).unwrap();
            assert_eq!(format_utc(e), s);
        }
    }

    #[test]
    fn a_malformed_stamp_is_none() {
        assert_eq!(parse_utc(""), None);
        assert_eq!(parse_utc("not a date"), None);
        assert_eq!(parse_utc("2026-09-30 00:00:00"), None);
        assert_eq!(parse_utc("2026-13-40T99:99:99Z"), None);
    }
}
