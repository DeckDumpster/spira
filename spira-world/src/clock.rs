//! The three timestamps world printed through a `date` child, from the clock in this process.

/// `strftime(format)` over `epoch` broken down in UTC (`utc`) or local time, as `date` does.
fn strftime(epoch: i64, format: &str, utc: bool) -> String {
    let t: libc::time_t = epoch as libc::time_t;
    // SAFETY: `tm` is plain data that gmtime_r/localtime_r fill; `buf` outlives the call and
    // its length is passed, and `fmt` is a NUL-terminated copy of `format`.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        // glibc's localtime_r initialises the zone from TZ on its first call.
        let filled = if utc { libc::gmtime_r(&t, &mut tm) } else { libc::localtime_r(&t, &mut tm) };
        if filled.is_null() {
            return String::new();
        }
        let Ok(fmt) = std::ffi::CString::new(format) else { return String::new() };
        let mut buf = [0u8; 128];
        let n = libc::strftime(buf.as_mut_ptr() as *mut libc::c_char, buf.len(), fmt.as_ptr(), &tm);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

/// `date -u +%Y-%m-%dT%H:%M:%SZ` at `epoch`.
pub fn utc_iso(epoch: i64) -> String {
    strftime(epoch, "%Y-%m-%dT%H:%M:%SZ", true)
}

/// `date -u -d "+N seconds" +%H:%M:%SZ`: the UTC time of day at `epoch`.
pub fn utc_hms(epoch: i64) -> String {
    strftime(epoch, "%H:%M:%SZ", true)
}

/// `date "+%Y-%m-%d %H:%M:%S %Z"`: local time at `epoch`, with the zone's abbreviation.
pub fn local_stamp(epoch: i64) -> String {
    strftime(epoch, "%Y-%m-%d %H:%M:%S %Z", false)
}

/// Seconds since the epoch, now.
pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_forms_match_date() {
        // 2026-11-03T21:04:05Z
        let t = 1_793_739_845;
        assert_eq!(utc_iso(t), "2026-11-03T21:04:05Z");
        assert_eq!(utc_hms(t), "21:04:05Z");
        assert_eq!(utc_iso(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn the_local_stamp_has_dates_shape_and_a_zone() {
        let s = local_stamp(1_793_739_845);
        let (date, rest) = s.split_once(' ').unwrap_or_default();
        let (time, zone) = rest.split_once(' ').unwrap_or_default();
        assert_eq!((date.len(), time.len()), (10, 8), "{s}");
        assert!(date.starts_with("2026-11-0") && !zone.is_empty(), "{s}");
    }
}
