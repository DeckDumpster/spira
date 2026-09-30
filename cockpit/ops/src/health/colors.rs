//! The pane's fixed palette and the handful of "what colour does this value get" rules that
//! are shared across sections. Exact escapes match the bash original so a parity diff against
//! a captured frame is byte-for-byte.

pub const RST: &str = "\x1b[0m";
pub const DIM: &str = "\x1b[2m";
pub const B: &str = "\x1b[1m";
pub const OK: &str = "\x1b[32m";
pub const WARN: &str = "\x1b[33m";
pub const BAD: &str = "\x1b[31m";
pub const ACC: &str = "\x1b[36m";

/// `dot 1|0` -> a green `●` or a red `○`.
pub fn dot(live: bool) -> String {
    if live { format!("{OK}\u{25cf}{RST}") } else { format!("{BAD}\u{25cb}{RST}") }
}

/// `num <value> <warn_at> [label]` — green below the threshold, coloured warn at/above it,
/// `?` always loud. `value` is the raw snapshot string; a non-integer falls to the green
/// branch exactly as `[ "$v" -ge "$w" ] 2>/dev/null` fails silently and the `if` takes its
/// `else`.
pub fn num(value: &str, warn_at: i64, label: &str) -> String {
    if value == "?" {
        format!("{BAD}{B}?{RST}{label}")
    } else if value.parse::<i64>().is_ok_and(|v| v >= warn_at) {
        format!("{WARN}{value}{RST}{label}")
    } else {
        format!("{OK}{value}{RST}{label}")
    }
}

/// Like `num`, but any non-zero reading is an outright fault rather than a threshold.
pub fn bad_unless_zero(value: &str, label: &str) -> String {
    if value == "?" {
        format!("{BAD}{B}?{RST}{label}")
    } else if value == "0" {
        format!("{OK}0{RST}{label}")
    } else {
        format!("{BAD}{B}{value}{RST}{label}")
    }
}

pub fn pri_colour(pri: &str) -> &'static str {
    match pri {
        "P0" => "\x1b[31m\x1b[1m", // C_BAD C_B
        "P1" => WARN,
        _ => DIM,
    }
}

pub fn verb_colour(verb: &str) -> &'static str {
    match verb {
        "landed" | "finished" | "announced" => OK,
        "reopened" | "poisoned" | "slain" | "reaped" => BAD,
        "in_progress" | "ended" | "reclaimed" => WARN,
        "claimed" => ACC,
        _ => DIM,
    }
}

pub fn part_colour(actor: &str) -> &'static str {
    if actor == "ops" { WARN } else { DIM }
}

pub fn kind_colour(kind: &str) -> &'static str {
    match kind {
        "bug" | "incident" => BAD,
        "decision" => WARN,
        _ => DIM,
    }
}

/// `age <seconds-string> <warn>` -> "12s" / "4m", coloured; `?` if unreadable.
pub fn age_str(seconds: &str, warn: i64) -> String {
    if seconds == "?" {
        return format!("{BAD}{B}?{RST}");
    }
    let Ok(a) = seconds.parse::<i64>() else {
        // Bash's `[ "$a" -lt 120 ]` failing silently falls to the `else` branch (minutes),
        // and the subsequent `-ge` compare also fails silently (false) — same path here.
        return format!("{DIM}{seconds}{RST}");
    };
    let s = if a < 120 { format!("{a}s") } else { format!("{}m", a / 60) };
    if a >= warn {
        format!("{BAD}{s}{RST}")
    } else {
        format!("{DIM}{s}{RST}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_colours() {
        assert_eq!(dot(true), format!("{OK}\u{25cf}{RST}"));
        assert_eq!(dot(false), format!("{BAD}\u{25cb}{RST}"));
    }

    #[test]
    fn num_unreadable_is_loud_never_green() {
        assert_eq!(num("?", 5, "x"), format!("{BAD}{B}?{RST}x"));
    }

    #[test]
    fn num_below_threshold_is_green_above_is_warn() {
        assert_eq!(num("4", 5, ""), format!("{OK}4{RST}"));
        assert_eq!(num("5", 5, ""), format!("{WARN}5{RST}"));
    }

    #[test]
    fn bad_unless_zero_zero_is_ok_anything_else_is_bad() {
        assert_eq!(bad_unless_zero("0", ""), format!("{OK}0{RST}"));
        assert_eq!(bad_unless_zero("3", ""), format!("{BAD}{B}3{RST}"));
        assert_eq!(bad_unless_zero("?", ""), format!("{BAD}{B}?{RST}"));
    }

    #[test]
    fn age_str_seconds_vs_minutes_boundary_and_colour() {
        assert_eq!(age_str("119", 1000), format!("{DIM}119s{RST}"));
        assert_eq!(age_str("120", 1000), format!("{DIM}2m{RST}"));
        assert_eq!(age_str("120", 1), format!("{BAD}2m{RST}"));
        assert_eq!(age_str("?", 1), format!("{BAD}{B}?{RST}"));
    }
}
