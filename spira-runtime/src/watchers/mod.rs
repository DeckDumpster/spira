//! The watchers that used to poll as scripts, now handlers that subscribe to events. A handler
//! here is pure: what the world looks like arrives as the outcome of an observe effect, and
//! what it has to say leaves as an `inbox.append` effect.

pub mod inbox_keeper;
pub mod round_duty;

pub const INBOX_APPEND: &str = "inbox.append";
pub const CONCIERGE_WAKE: &str = "concierge.wake";

/// `YYYY-MM-DDTHH:MM:SSZ` for a UTC epoch-seconds value.
pub fn iso8601(secs: u64) -> String {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::iso8601;

    #[test]
    fn epoch_seconds_render_as_utc() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_793_166_343), "2026-10-28T05:45:43Z");
        assert_eq!(iso8601(951_782_400), "2000-02-29T00:00:00Z");
    }
}
