//! `lib.sh`'s `log()`, unchanged format: `<ISO-8601 UTC> spira: <msg>` on stdout. Every
//! caller here used the exact same function, so the format stays identical rather than
//! adopting a Rust logging crate's own shape.

pub fn log(msg: &str) {
    println!("{} spira: {}", now_iso(), msg);
}

pub fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    fmt_iso(secs)
}

/// `date -u +%Y-%m-%dT%H:%M:%SZ` for an arbitrary epoch — used both for `log()` and for the
/// several timestamp fields the sweep renders the same way.
pub fn fmt_iso(epoch_secs: i64) -> String {
    // A tiny civil-from-days calculation (Howard Hinnant's algorithm) so this crate needs no
    // chrono dependency for what is, everywhere it is used, UTC-only formatting.
    let z = epoch_secs.div_euclid(86400);
    let secs_of_day = epoch_secs.rem_euclid(86400);
    let (h, m, s) = (secs_of_day / 3600, (secs_of_day % 3600) / 60, secs_of_day % 60);
    let z2 = z + 719468;
    let era = if z2 >= 0 { z2 } else { z2 - 146096 } / 146097;
    let doe = (z2 - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as i64;
    let m_ = (if mp < 10 { mp + 3 } else { mp - 9 }) as i64;
    let year = if m_ <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, m_, d, h, m, s
    )
}

/// Parses `YYYY-MM-DDTHH:MM:SS` (an optional trailing `Z` is stripped first) as UTC,
/// returning the epoch second — the inverse of `fmt_iso`, and the same format
/// `watchtower-czar-outcome.py`'s `ts()` parsed with `datetime.strptime(...).replace
/// (tzinfo=timezone.utc)`. Anything that does not match this exact shape is `None`, the
/// same as the python's broad `except Exception: return None`.
pub fn parse_iso_utc(s: &str) -> Option<i64> {
    let s = s.strip_suffix('Z').unwrap_or(s);
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let mo: i64 = d.next()?.parse().ok()?;
    let da: i64 = d.next()?.parse().ok()?;
    if d.next().is_some() {
        return None;
    }
    let mut t = time.split(':');
    let h: i64 = t.next()?.parse().ok()?;
    let mi: i64 = t.next()?.parse().ok()?;
    let se: i64 = t.next()?.parse().ok()?;
    if t.next().is_some() {
        return None;
    }
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) {
        return None;
    }
    let days = days_from_civil(y, mo, da);
    Some(days * 86400 + h * 3600 + mi * 60 + se)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = (if y >= 0 { y } else { y - 399 }).div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// `date -u +%Y%m%dT%H%M%SZ` — the compact form lapse-record filenames sort chronologically
/// by (sp-fhzib): same instant as `fmt_iso`, punctuation stripped.
pub fn fmt_compact(epoch_secs: i64) -> String {
    fmt_iso(epoch_secs).replace(['-', ':'], "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_compact_strips_punctuation_but_keeps_order() {
        assert_eq!(fmt_compact(1_700_000_000), "20231114T221320Z");
    }

    #[test]
    fn fmt_iso_matches_known_epochs() {
        assert_eq!(fmt_iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(fmt_iso(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(fmt_iso(1_900_000_000), "2030-03-17T17:46:40Z");
    }

    #[test]
    fn parse_iso_utc_is_the_inverse_of_fmt_iso() {
        for secs in [0i64, 1_700_000_000, 1_900_000_000, 1_759_276_800] {
            let s = fmt_iso(secs);
            assert_eq!(parse_iso_utc(&s), Some(secs), "round-trip of {s}");
        }
    }

    #[test]
    fn parse_iso_utc_rejects_garbage() {
        assert_eq!(parse_iso_utc(""), None);
        assert_eq!(parse_iso_utc("not-a-date"), None);
        assert_eq!(parse_iso_utc("2026-13-01T00:00:00Z"), None);
    }
}
