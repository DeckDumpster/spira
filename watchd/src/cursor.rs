//! Cursor arithmetic (`_wd_pos`, `_wd_total`, `_wd_age`) and the exact-range read
//! (`_wd_*` callers' shared `sed -n 'pos+1,totalp'`, never `tail | head`, because the log is
//! being appended to while this runs). File IO lives in `fs_ops.rs`; this module is the pure
//! part, heavily unit tested because it is the part every command's correctness rests on.

pub use inbox_rules::{clamp_pos, parse_cursor};
pub use spira_world::clock::format_age;

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(trh3zxc)]
    #[test]
    fn a_missing_cursor_reads_as_nothing_unread_from_the_start() {
        assert_eq!(clamp_pos(None, 100), 0);
    }

    #[test_id(t4mn9e0)]
    #[test]
    fn a_cursor_past_the_end_clamps_to_the_end() {
        assert_eq!(clamp_pos(Some(150), 100), 100);
    }

    #[test_id(tkzlt89)]
    #[test]
    fn a_negative_cursor_clamps_to_zero() {
        assert_eq!(clamp_pos(Some(-5), 100), 0);
    }

    #[test_id(t3a4477)]
    #[test]
    fn an_ordinary_cursor_passes_through() {
        assert_eq!(clamp_pos(Some(42), 100), 42);
    }

    #[test_id(tewx31i)]
    #[test]
    fn a_cursor_of_zero_total_is_zero() {
        assert_eq!(clamp_pos(Some(0), 0), 0);
        assert_eq!(clamp_pos(None, 0), 0);
    }

    #[test_id(txcajjc)]
    #[test]
    fn parsing_a_cursor_file() {
        assert_eq!(parse_cursor("42\n"), Some(42));
        assert_eq!(parse_cursor(""), None);
        assert_eq!(parse_cursor("junk"), None);
        assert_eq!(parse_cursor("-3"), Some(-3));
    }

    #[test_id(t9eqvn3)]
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

    #[test_id(t6eiwgm)]
    #[test]
    fn a_negative_age_clamps_to_zero_rather_than_render_negative() {
        assert_eq!(format_age(-100), "0s");
    }
}
