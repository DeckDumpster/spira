//! Cursor arithmetic (`_wd_pos`, `_wd_total`, `_wd_age`) and the exact-range read
//! (`_wd_*` callers' shared `sed -n 'pos+1,totalp'`, never `tail | head`, because the log is
//! being appended to while this runs). File IO lives in `fs_ops.rs`; this module is the pure
//! part, heavily unit tested because it is the part every command's correctness rests on.

/// Clamp a raw cursor value into `[0, total]`. A cursor that is missing, negative, or past
/// the end of the log (rotation, truncation, hand-edit, or a final unterminated line `wc -l`
/// undercounts) reads as "nothing unread" rather than negative or out of range — the
/// conservative direction, because the alternative is replaying a whole log into a fresh
/// context window.
pub fn clamp_pos(raw: Option<i64>, total: u64) -> u64 {
    let total = total as i64;
    match raw {
        Some(p) if p < 0 => 0,
        Some(p) if p > total => total as u64,
        Some(p) => p as u64,
        None => 0,
    }
}

/// Parses a cursor file's content the way the bash did: the first whitespace-delimited
/// token, empty/negative/non-numeric reads as "no cursor at all" (`None`), which `clamp_pos`
/// then turns into 0.
pub fn parse_cursor(content: &str) -> Option<i64> {
    content.split_whitespace().next()?.parse::<i64>().ok()
}

/// The coarsest non-zero unit — `_wd_age`. Whitespace-free, because `status` is a
/// whitespace-delimited table addressed by column index. A negative age (a clock that moved
/// backwards, or a log with a future mtime) clamps to zero rather than rendering.
pub fn format_age(seconds: i64) -> String {
    let s = seconds.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_cursor_reads_as_nothing_unread_from_the_start() {
        assert_eq!(clamp_pos(None, 100), 0);
    }

    #[test]
    fn a_cursor_past_the_end_clamps_to_the_end() {
        assert_eq!(clamp_pos(Some(150), 100), 100);
    }

    #[test]
    fn a_negative_cursor_clamps_to_zero() {
        assert_eq!(clamp_pos(Some(-5), 100), 0);
    }

    #[test]
    fn an_ordinary_cursor_passes_through() {
        assert_eq!(clamp_pos(Some(42), 100), 42);
    }

    #[test]
    fn a_cursor_of_zero_total_is_zero() {
        assert_eq!(clamp_pos(Some(0), 0), 0);
        assert_eq!(clamp_pos(None, 0), 0);
    }

    #[test]
    fn parsing_a_cursor_file() {
        assert_eq!(parse_cursor("42\n"), Some(42));
        assert_eq!(parse_cursor(""), None);
        assert_eq!(parse_cursor("junk"), None);
        assert_eq!(parse_cursor("-3"), Some(-3));
    }

    #[test]
    fn age_picks_the_coarsest_unit_that_is_not_zero() {
        assert_eq!(format_age(0), "0s");
        assert_eq!(format_age(59), "59s");
        assert_eq!(format_age(60), "1m");
        assert_eq!(format_age(3599), "59m");
        assert_eq!(format_age(3600), "1h");
        assert_eq!(format_age(86399), "23h");
        assert_eq!(format_age(86400), "1d");
    }

    #[test]
    fn a_negative_age_clamps_to_zero_rather_than_render_negative() {
        assert_eq!(format_age(-100), "0s");
    }
}
