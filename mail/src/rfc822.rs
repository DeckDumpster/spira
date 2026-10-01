//! A single RFC 5322 `Date:` header value in UTC, with no calendar dependency — mail.sh's
//! own `date -u '+%a, %d %b %Y %H:%M:%S +0000'`. Civil-date math is Howard Hinnant's
//! `civil_from_days` (public domain), which this workspace has no other copy of.

use std::time::{SystemTime, UNIX_EPOCH};

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn weekday_index(days_since_epoch: i64) -> usize {
    (((days_since_epoch % 7) + 7) % 7 + 4) as usize % 7
}

/// Renders a Unix timestamp (seconds since epoch, UTC) as an RFC 5322 `Date:` value.
pub fn format_unix(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400);
    let (year, month, day) = civil_from_days(days);
    let hh = tod / 3600;
    let mm = (tod % 3600) / 60;
    let ss = tod % 60;
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} +0000",
        WEEKDAYS[weekday_index(days)],
        day,
        MONTHS[(month - 1) as usize],
        year,
        hh,
        mm,
        ss
    )
}

/// `done`'s own note-footer timestamp — mail.sh's `date -u '+%Y-%m-%d %H:%M UTC'`, a
/// different shape from the `Date:` header this module otherwise renders.
pub fn format_done(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02} UTC", tod / 3600, (tod % 3600) / 60)
}

pub fn now() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    format_unix(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_thursday_jan_1_1970() {
        assert_eq!(format_unix(0), "Thu, 01 Jan 1970 00:00:00 +0000");
    }

    #[test]
    fn a_known_recent_timestamp_renders_correctly() {
        // 2026-09-30T10:40:26Z — matches the git log timestamp seen this session.
        assert_eq!(format_unix(1790764826), "Wed, 30 Sep 2026 10:40:26 +0000");
    }
}
