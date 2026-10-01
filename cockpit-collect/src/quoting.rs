//! Small formatting helpers shared by every probe, ported 1:1 from the `python3 -c` and
//! `tr`/`sed` fragments scattered through `spira/cockpit.sh`. Kept pure and dependency-free
//! so they can be unit tested directly against literal fixtures (DESIGN.md "Design").

/// `tr -c 'A-Za-z0-9 ._/:,()#+-' ' ' | tr -s ' '` — the snapshot is a `KEY=value` file a
/// shell `source`s, so freeform prose (a close reason, a git subject, a bead title) must
/// never carry a newline, an `=`, or a quote into it. Matches the allowlist used throughout
/// cockpit.sh for any value pulled from free text.
pub fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = false;
    for c in s.chars() {
        let allowed = c.is_ascii_alphanumeric()
            || " ._/:,()#+-".contains(c);
        let c = if allowed { c } else { ' ' };
        if c == ' ' {
            if last_was_space {
                continue;
            }
            last_was_space = true;
        } else {
            last_was_space = false;
        }
        out.push(c);
    }
    out.trim().to_string()
}

/// The same allowlist, but also flattening `=` to `-` first (a handful of call sites do
/// `.replace("=", "-")` after truncation so a title cannot inject a bogus KEY into the
/// snapshot). Truncates to `max_len` *before* sanitizing, matching Python's `[:n]` on the
/// raw title followed by the replace.
pub fn sanitize_title(s: &str, max_len: usize) -> String {
    let truncated: String = s.chars().take(max_len).collect();
    let mut out = String::with_capacity(truncated.len());
    for c in truncated.chars() {
        let allowed = c.is_ascii_alphanumeric() || " ._/:,()#+-".contains(c);
        out.push(if allowed { c } else { ' ' });
    }
    out.replace('=', "-")
}

/// now/Nm/Nh/Nd — the one relative-age format used everywhere on this pane (NEXT, RECENT,
/// INFLOW, AWAITING CI, the queue funnel). `secs` must be >= 0; callers clamp.
pub fn rel_age(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 90 {
        format!("{secs}s")
    } else if secs < 5400 {
        format!("{}m", secs / 60)
    } else if secs < 172800 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

/// `_epoch_to_age`: epoch seconds -> now/Nm/Nh/Nd, or `?` when the epoch is absent/non-numeric.
/// Unlike [`rel_age`] this prints the literal `now` word for anything under 90s, matching the
/// funnel's own `_epoch_to_age` rather than the NEXT/RECENT family's `%ds`.
pub fn epoch_to_age(epoch: Option<i64>, now: i64) -> String {
    match epoch {
        None => "?".to_string(),
        Some(ep) => {
            let diff = now - ep;
            if diff < 90 {
                "now".to_string()
            } else if diff < 5400 {
                format!("{}m", diff / 60)
            } else if diff < 172800 {
                format!("{}h", diff / 3600)
            } else {
                format!("{}d", diff / 86400)
            }
        }
    }
}

/// Parse an ISO-8601 UTC timestamp of the shapes bd emits: `YYYY-MM-DDTHH:MM:SSZ`, with or
/// without fractional seconds, or with a `+00:00` offset in place of `Z` (Python's
/// `fromisoformat` after `.replace("Z", "+00:00")`). Non-UTC offsets are not supported —
/// bd has never emitted one to this collector, so no code path here needs to.
pub fn parse_iso8601(s: &str) -> Option<i64> {
    let s = s.trim();
    let s = s.strip_suffix('Z').unwrap_or(s);
    let s = s.strip_suffix("+00:00").unwrap_or(s);
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    let time = time.split('.').next().unwrap_or(time);
    let mut t = time.split(':');
    let hour: i64 = t.next()?.parse().ok()?;
    let min: i64 = t.next()?.parse().ok()?;
    let sec: i64 = t.next().unwrap_or("0").parse().ok()?;
    Some(days_from_civil(year, month, day) * 86400 + hour * 3600 + min * 60 + sec)
}

/// Howard Hinnant's civil_from_days, inverted — days since the Unix epoch for a proleptic
/// Gregorian Y-M-D. No external date crate is in this workspace (Cargo.lock has none), and
/// this collector only ever needs UTC midnight-based arithmetic, so it is cheaper to carry
/// forty lines of well-known integer math than to add one.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The one shell-quoting layer `cockpit.env` itself carries: every value is wrapped in
/// single quotes, with an embedded `'` escaped as `'\''` (close-quote, backslash-quote,
/// open-quote — the standard POSIX idiom), so the file sources safely with no parser.
/// Applied exactly once, at merge/write time ([`unquote_shell_single`] is this function's
/// inverse, used to make that one application idempotent against an already-quoted input).
pub fn self_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The inverse of [`self_quote`]: if `v` is wrapped in a single-quote pair using that exact
/// escaping, return the unescaped inner value; otherwise return `v` unchanged.
///
/// Fragment files (`cockpit.d/*.env`) are meant to carry each probe's RAW, unquoted value —
/// but a fragment can legitimately carry an already-quoted one anyway: `run_probe_body`'s
/// "stale" path copies a fragment's previous value lines forward VERBATIM on a failing
/// pass (DESIGN.md), so a value written by an older binary (or any future producer that
/// quotes its own output, by accident or on purpose) survives in that raw form until the
/// probe succeeds again. Merging that straight through `self_quote` a second time produced
/// the doubled `''\'''\'''` for an empty value in production (the operator, 2026-09-30,
/// `cockpit-collect collect` release 51228489c) — found AFTER the first fix (removing the
/// probe-level pre-quoting) had already landed, because that fix cannot retroactively
/// unquote a value a stale-carried-forward fragment was still holding from before it took
/// effect. Unquoting defensively at the one place every fragment value is read —
/// `parse_fragment` — makes the merge idempotent regardless of what already produced the
/// fragment's content, which the first fix alone could not guarantee.
pub fn unquote_shell_single(v: &str) -> String {
    let bytes = v.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'' {
        v[1..v.len() - 1].replace("'\\''", "'")
    } else {
        v.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_and_collapses() {
        assert_eq!(sanitize("hello\tworld\n\"x\""), "hello world x");
        assert_eq!(sanitize("a  b   c"), "a b c");
        assert_eq!(sanitize(""), "");
    }

    #[test]
    fn sanitize_title_truncates_and_never_leaves_a_literal_equals() {
        // The allowlist already maps '=' to ' ' before the trailing `.replace("=", "-")`
        // in the bash's own `re.sub(...)[:80].replace("=", "-")` ever sees one — ported
        // faithfully rather than "fixed", per law-rust-rewrites-start-from-intent (name
        // the accretion, don't silently change behaviour a reader may depend on).
        assert_eq!(sanitize_title("a=b=c", 80), "a b c");
        let long = "x".repeat(100);
        assert_eq!(sanitize_title(&long, 5), "xxxxx");
    }

    #[test]
    fn rel_age_buckets() {
        assert_eq!(rel_age(5), "5s");
        assert_eq!(rel_age(89), "89s");
        assert_eq!(rel_age(90), "1m");
        assert_eq!(rel_age(5399), "89m");
        assert_eq!(rel_age(5400), "1h");
        assert_eq!(rel_age(172799), "47h");
        assert_eq!(rel_age(172800), "2d");
    }

    #[test]
    fn epoch_to_age_now_bucket_and_missing() {
        assert_eq!(epoch_to_age(None, 100), "?");
        assert_eq!(epoch_to_age(Some(100), 150), "now");
        assert_eq!(epoch_to_age(Some(0), 200), "3m");
    }

    #[test]
    fn parse_iso8601_known_epoch() {
        // 2026-09-30T00:00:00Z — cross-checked against `date -u -d ... +%s`.
        assert_eq!(parse_iso8601("2026-09-30T00:00:00Z"), Some(1790726400));
        assert_eq!(
            parse_iso8601("2026-09-30T00:00:00.123456+00:00"),
            Some(1790726400)
        );
        assert_eq!(parse_iso8601("not-a-date"), None);
    }

    #[test]
    fn self_quote_escapes_embedded_quotes() {
        assert_eq!(self_quote("plain"), "'plain'");
        assert_eq!(self_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn unquote_shell_single_is_self_quotes_exact_inverse() {
        for raw in ["", "plain", "a b", "a'b", "can't stop"] {
            assert_eq!(unquote_shell_single(&self_quote(raw)), raw);
        }
    }

    #[test]
    fn unquote_shell_single_leaves_an_unquoted_value_alone() {
        // A probe's own raw, never-quoted output must round-trip unchanged — this is not
        // a guess that every value is quoted, only a correction when one already is.
        assert_eq!(unquote_shell_single("plain"), "plain");
        assert_eq!(unquote_shell_single(""), "");
        assert_eq!(unquote_shell_single("a b"), "a b");
    }

    #[test]
    fn unquote_shell_single_matches_the_production_fixture() {
        // The exact values found in run/cockpit.d/*.env (the operator, 2026-09-30): an
        // already-quoted empty string, and a quoted non-empty one.
        assert_eq!(unquote_shell_single("''"), "");
        assert_eq!(unquote_shell_single("'a b'"), "a b");
    }
}
