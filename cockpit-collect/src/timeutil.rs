//! Timestamp parsing shared by the metrics and sparkline ports. No calendar crate: both
//! inputs are fixed-shape ISO-8601 strings.

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn num(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn civil(date: &str, h: i64, mi: i64, s: i64) -> Option<i64> {
    let (y, rest) = date.split_at_checked(4)?;
    let rest = rest.strip_prefix('-')?;
    let (m, rest) = rest.split_at_checked(2)?;
    let d = rest.strip_prefix('-')?;
    let (y, m, d) = (num(y)?, num(m)?, num(d)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86400 + h * 3600 + mi * 60 + s)
}

/// `%Y-%m-%dT%H:%M:%SZ`, exactly.
pub fn parse_strict(s: &str) -> Option<i64> {
    if s.len() != 20 || !s.is_ascii() {
        return None;
    }
    let b = s.as_bytes();
    if b[10] != b'T' || b[13] != b':' || b[16] != b':' || b[19] != b'Z' {
        return None;
    }
    let (h, mi, sec) = (num(&s[11..13])?, num(&s[14..16])?, num(&s[17..19])?);
    if h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    civil(&s[..10], h, mi, sec)
}

/// Tolerant ISO-8601 (date, optional time with fraction, `Z` or `±HH:MM`; no zone is UTC),
/// as epoch seconds.
pub fn parse_iso(s: &str) -> Option<f64> {
    let s = s.trim();
    if !s.is_ascii() || s.len() < 10 {
        return None;
    }
    let mut secs = civil(&s[..10], 0, 0, 0)? as f64;
    let rest = &s[10..];
    if rest.is_empty() {
        return Some(secs);
    }
    let rest = rest.strip_prefix('T').or_else(|| rest.strip_prefix(' '))?;
    let (clock, off) = match rest.find(['Z', 'z', '+', '-']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let mut parts = clock.split(':');
    let h = num(parts.next()?)?;
    let mi = parts.next().map_or(Some(0), num)?;
    let sec_s = parts.next().unwrap_or("0");
    let (whole, frac) = sec_s.split_once('.').unwrap_or((sec_s, ""));
    let sec = num(whole)?;
    if parts.next().is_some() || h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    secs += (h * 3600 + mi * 60 + sec) as f64;
    if !frac.is_empty() {
        secs += format!("0.{frac}").parse::<f64>().ok()?;
    }
    let off_s = match off {
        "" | "Z" | "z" => 0,
        o => {
            let sign = if o.starts_with('-') { -1 } else { 1 };
            let body = &o[1..];
            let (oh, om) = body.split_once(':').unwrap_or((body, "0"));
            sign * (num(oh)? * 3600 + num(om)? * 60)
        }
    };
    Some(secs - off_s as f64)
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_known_epoch() {
        assert_eq!(parse_strict("2026-09-10T00:00:00Z"), Some(1788998400));
        assert_eq!(parse_strict("2026-09-10T00:00:00"), None);
        assert_eq!(parse_strict("garbage"), None);
    }

    #[test]
    fn iso_variants_agree() {
        let a = parse_iso("2026-09-10T00:00:00Z").unwrap();
        assert_eq!(parse_iso("2026-09-10T02:00:00+02:00"), Some(a));
        assert_eq!(parse_iso("2026-09-10T00:00:00.500000Z"), Some(a + 0.5));
        assert_eq!(parse_iso("2026-09-10 00:00:00"), Some(a));
        assert_eq!(parse_iso("2026-09-10"), Some(a));
        assert_eq!(parse_iso("nope"), None);
    }
}
