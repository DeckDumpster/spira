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
    fn the_shared_formatter_round_trips_through_parse() {
        for s in ["1970-01-01T00:00:00Z", "2026-09-30T00:00:00Z", "2024-02-29T12:34:56Z"] {
            let e = parse_utc(s).unwrap();
            assert_eq!(spira_world::clock::utc_iso(e), s);
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
