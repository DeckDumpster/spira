//! Field formatters shared by several sections: fitting text to a column budget, human token
//! counts, percentages, model-id shortening, and the two duration spellings the pane uses
//! (minutes-based for lease/rate-limit windows, seconds-based for mail age).

/// `fit <text> <cols>` — cut to `cols` **characters** (the pane runs in `LC_ALL=C.UTF-8`
/// specifically so this counts codepoints, not bytes), marked with `…` when cut. `cols < 2`
/// never cuts, matching `[ "$n" -ge 2 ] 2>/dev/null` guarding the whole branch.
pub fn fit(text: &str, cols: i64) -> String {
    if cols < 2 {
        return text.to_string();
    }
    let n = cols as usize;
    let chars: Vec<char> = text.chars().collect();
    if chars.len() > n {
        let mut s: String = chars[..n - 1].iter().collect();
        s.push('\u{2026}');
        s
    } else {
        text.to_string()
    }
}

/// `tok <n>` -> "494M" / "126k". Anything not a plain non-negative integer (including `?`
/// and `-`) passes through unchanged.
pub fn tok(v: &str) -> String {
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return if v.is_empty() { "?".to_string() } else { v.to_string() };
    }
    let n: i64 = v.parse().unwrap_or(0);
    if n >= 1_000_000 {
        format!("{}M", n / 1_000_000)
    } else if n >= 1000 {
        format!("{}k", n / 1000)
    } else {
        n.to_string()
    }
}

/// `pct <part> <whole>` -> "74%", or "?" when either side is not a plain integer or the
/// whole is not `> 0`.
pub fn pct(part: &str, whole: &str) -> String {
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(part) || !all_digits(whole) {
        return "?".to_string();
    }
    let w: i64 = whole.parse().unwrap_or(0);
    if w <= 0 {
        return "?".to_string();
    }
    let p: i64 = part.parse().unwrap_or(0);
    format!("{}%", p * 100 / w)
}

/// `model_short <id>` -> a name that fits a third-width pane: `claude-opus-4-6` -> `opus 4.6`.
/// `?`, `-` and empty pass straight through; anything not starting `claude-` also passes
/// through unchanged, deliberately (an unrecognised id must print as itself, never a guess).
pub fn model_short(id: &str) -> String {
    match id {
        "" | "?" | "-" => id.to_string(),
        s if s.starts_with("claude-") => {
            let rest = &s["claude-".len()..];
            // 1. strip a trailing 8-digit date stamp: `-YYYYMMDD` at the end.
            let no_date = strip_trailing_date(rest);
            // 2. `-N-M` at the end -> " N.M"
            if let Some(s2) = split_trailing_two_numbers(&no_date) {
                return s2;
            }
            // 3. `-N` at the end -> " N"
            if let Some(s3) = split_trailing_one_number(&no_date) {
                return s3;
            }
            no_date
        }
        s => s.to_string(),
    }
}

fn strip_trailing_date(s: &str) -> String {
    // `s/-[0-9]{8}$//`
    if s.len() > 9 {
        let tail = &s[s.len() - 8..];
        if tail.bytes().all(|b| b.is_ascii_digit()) && s.as_bytes()[s.len() - 9] == b'-' {
            return s[..s.len() - 9].to_string();
        }
    }
    s.to_string()
}

fn split_trailing_two_numbers(s: &str) -> Option<String> {
    // `s/-([0-9]+)-([0-9]+)$/ \1.\2/`
    let dash2 = s.rfind('-')?;
    let (head, m2) = (&s[..dash2], &s[dash2 + 1..]);
    if m2.is_empty() || !m2.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let dash1 = head.rfind('-')?;
    let (head2, m1) = (&head[..dash1], &head[dash1 + 1..]);
    if m1.is_empty() || !m1.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!("{head2} {m1}.{m2}"))
}

fn split_trailing_one_number(s: &str) -> Option<String> {
    // `s/-([0-9]+)$/ \1/`
    let dash = s.rfind('-')?;
    let (head, m) = (&s[..dash], &s[dash + 1..]);
    if m.is_empty() || !m.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!("{head} {m}"))
}

/// `_dur_m <minutes>` -> "0m" / "37m" / "4h3m" / "2d1h". Anything not a plain non-negative
/// integer (including `?`, `-`, empty) passes through unchanged.
pub fn dur_m(m: &str) -> String {
    if m.is_empty() || !m.bytes().all(|b| b.is_ascii_digit()) {
        return if m.is_empty() { "?".to_string() } else { m.to_string() };
    }
    let v: i64 = m.parse().unwrap_or(0);
    if v == 0 {
        "0m".to_string()
    } else if v < 60 {
        format!("{v}m")
    } else if v < 1440 {
        format!("{}h{}m", v / 60, v % 60)
    } else {
        format!("{}d{}h", v / 1440, v % 1440 / 60)
    }
}

/// `_mail_dur <seconds>` -> "90s"/"3600s"(<120)/"1m"(<7200)/"1h"(<86400)/"1d". Anything not a
/// plain non-negative integer (`?`, `-`, empty) passes through unchanged.
pub fn mail_dur(s: &str) -> String {
    if s.is_empty() || s == "?" || s == "-" || !s.bytes().all(|b| b.is_ascii_digit()) {
        return if s.is_empty() { "?".to_string() } else { s.to_string() };
    }
    let v: i64 = s.parse().unwrap_or(0);
    if v < 120 {
        format!("{v}s")
    } else if v < 7200 {
        format!("{}m", v / 60)
    } else if v < 86400 {
        format!("{}h", v / 3600)
    } else {
        format!("{}d", v / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_passes_short_text_and_marks_cut_text() {
        assert_eq!(fit("hi", 10), "hi");
        assert_eq!(fit("hello world", 6), "hello\u{2026}");
        assert_eq!(fit("hello world", 1), "hello world"); // cols<2 never cuts
        assert_eq!(fit("hello world", 0), "hello world");
    }

    #[test]
    fn fit_counts_characters_not_bytes() {
        // multibyte marker chars are used on this pane; a 3-char string must not cut at 3
        // bytes.
        assert_eq!(fit("a·b", 10), "a·b");
        assert_eq!(fit("abcd", 3), "ab\u{2026}");
    }

    #[test]
    fn tok_scales() {
        assert_eq!(tok("999"), "999");
        assert_eq!(tok("1500"), "1k");
        assert_eq!(tok("2500000"), "2M");
        assert_eq!(tok("?"), "?");
        assert_eq!(tok("-"), "-");
    }

    #[test]
    fn pct_and_unreadable() {
        assert_eq!(pct("74", "100"), "74%");
        assert_eq!(pct("1", "0"), "?");
        assert_eq!(pct("?", "100"), "?");
    }

    #[test]
    fn model_short_examples() {
        assert_eq!(model_short("claude-opus-4-6"), "opus 4.6");
        assert_eq!(model_short("claude-haiku-4-5-20251001"), "haiku 4.5");
        assert_eq!(model_short("claude-sonnet-4"), "sonnet 4");
        assert_eq!(model_short("?"), "?");
        assert_eq!(model_short("-"), "-");
        assert_eq!(model_short("gpt-4"), "gpt-4");
    }

    #[test]
    fn dur_m_bands() {
        assert_eq!(dur_m("0"), "0m");
        assert_eq!(dur_m("5"), "5m");
        assert_eq!(dur_m("65"), "1h5m");
        assert_eq!(dur_m("1500"), "1d1h");
        assert_eq!(dur_m("?"), "?");
    }

    #[test]
    fn mail_dur_bands() {
        assert_eq!(mail_dur("90"), "90s");
        assert_eq!(mail_dur("150"), "2m");
        assert_eq!(mail_dur("4000"), "66m"); // <7200s is still the minutes band
        assert_eq!(mail_dur("7300"), "2h");
        assert_eq!(mail_dur("90000"), "1d");
        assert_eq!(mail_dur("-"), "-");
    }
}
