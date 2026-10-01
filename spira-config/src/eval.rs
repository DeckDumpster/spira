//! A small, deliberately narrow evaluator for the `: "${KEY:=EXPR}"` default statements the
//! [`crate::registry`] reads out of `spira/conf.d/`. `EXPR` is bash, but a survey of every one
//! of the 222 keys that carry a real default (wave4-decomposition.md, sp-eekjm) found only
//! four shapes: a literal, a variable reference (`$VAR` / `${VAR}`), a `${VAR:-default}`
//! fallback (never nested two deep), and three callers of exactly two command substitutions
//! (`dirname`, the harness's own `_spira_join`) plus one `$(( ... ))` arithmetic expression.
//! This evaluator covers exactly that shape and refuses, by name, anything wider — it is not
//! a bash parser, and growing it to one defeats the point of retiring bash in the first place.
//! A registry default that needs more than this should move to the hand-written side of
//! `resolve` instead (mark its `conf.d` file `PROCEDURAL`), not stretch this evaluator.

use std::collections::BTreeMap;

/// Evaluates `expr` (the text captured between a key's `${KEY:=` and its matching `}`)
/// against `known` — every variable already resolved before this key in topological order,
/// keyed by name without a leading `$`. An unset variable reads as empty, the same as an
/// unset shell variable.
pub fn eval_expr(expr: &str, known: &BTreeMap<String, String>) -> Result<String, String> {
    let bytes = expr.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() {
            let (piece, next) = eval_dollar(expr, i, known)?;
            out.push_str(&piece);
            i = next;
        } else {
            // Advance by one CHAR, not one byte, so a multi-byte doc character (an em dash,
            // say) that somehow ended up in a default literal is not split mid-codepoint.
            let ch = expr[i..].chars().next().expect("i < bytes.len()");
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    Ok(out)
}

/// Handles one `$...` construct starting at `expr[start]` (`expr.as_bytes()[start] == b'$'`).
/// Returns the substituted text and the byte offset just past the construct.
fn eval_dollar(
    expr: &str,
    start: usize,
    known: &BTreeMap<String, String>,
) -> Result<(String, usize), String> {
    let bytes = expr.as_bytes();
    let after_dollar = start + 1;
    match bytes.get(after_dollar) {
        Some(b'{') => eval_brace(expr, after_dollar + 1, known),
        Some(b'(') => {
            if bytes.get(after_dollar + 1) == Some(&b'(') {
                eval_arith(expr, after_dollar + 2, known)
            } else {
                eval_command_sub(expr, after_dollar + 1, known)
            }
        }
        Some(c) if c.is_ascii_alphabetic() || *c == b'_' => {
            let mut j = after_dollar;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_')
            {
                j += 1;
            }
            let name = &expr[after_dollar..j];
            Ok((known.get(name).cloned().unwrap_or_default(), j))
        }
        _ => Err(format!("eval_expr: stray '$' in {expr:?} at byte {start}")),
    }
}

/// `${NAME}` or `${NAME:-default}` / `${NAME-default}`, brace-depth aware so a default that
/// itself names another `${...}` (none do today; see the module doc) would still be captured
/// whole rather than truncated at the first inner `}`.
fn eval_brace(
    expr: &str,
    start: usize,
    known: &BTreeMap<String, String>,
) -> Result<(String, usize), String> {
    let end = find_matching_brace(expr, start)
        .ok_or_else(|| format!("eval_expr: unbalanced '{{' in {expr:?}"))?;
    let inner = &expr[start..end];
    let (name, default) = match inner.find(":-").or_else(|| inner.find('-')) {
        Some(p) if inner[..p].chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
            let op_len = if inner[p..].starts_with(":-") { 2 } else { 1 };
            (&inner[..p], Some(&inner[p + op_len..]))
        }
        _ => (inner, None),
    };
    let value = known.get(name).cloned().unwrap_or_default();
    let result = if value.is_empty() {
        match default {
            Some(d) => eval_expr(d, known)?,
            None => String::new(),
        }
    } else {
        value
    };
    Ok((result, end + 1))
}

fn find_matching_brace(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 1i32;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// `$(( EXPR ))` — integer arithmetic. Supports a variable reference (`$VAR`, `${VAR}`,
/// `${VAR:-default}`), an integer literal, `*`, `+`, `-` and parentheses — exactly what the
/// one registry key that uses this (`SPIRA_QUEUE_THROTTLE_DEPTH_AT`) needs, no more.
fn eval_arith(
    expr: &str,
    start: usize,
    known: &BTreeMap<String, String>,
) -> Result<(String, usize), String> {
    // No internal parens appear in any registry arithmetic expression today, so the close is
    // found as a literal "))" rather than by paren-depth bookkeeping (see the module doc).
    let close = expr[start..]
        .find("))")
        .ok_or_else(|| format!("eval_expr: unterminated '$((' in {expr:?}"))?;
    let inner_raw = &expr[start..start + close];
    let substituted = eval_expr(inner_raw, known)?;
    let value = arith_eval(&substituted)
        .ok_or_else(|| format!("eval_expr: cannot evaluate arithmetic {substituted:?} (from {inner_raw:?})"))?;
    Ok((value.to_string(), start + close + 2))
}

/// `+`/`-`/`*` left-to-right over whitespace-separated integer tokens — not real operator
/// precedence, which none of today's registry arithmetic needs (a single `*` with two
/// operands). Extend this, don't reach for a crate, if a second shape ever appears.
fn arith_eval(s: &str) -> Option<i64> {
    let mut tokens = s.split_whitespace();
    let mut acc: i64 = tokens.next()?.parse().ok()?;
    loop {
        let Some(op) = tokens.next() else { return Some(acc) };
        let rhs: i64 = tokens.next()?.parse().ok()?;
        acc = match op {
            "*" => acc.checked_mul(rhs)?,
            "+" => acc.checked_add(rhs)?,
            "-" => acc.checked_sub(rhs)?,
            _ => return None,
        };
    }
}

/// `$(cmd ...)` — command substitution. Only the two functions the registry's defaults
/// actually call are supported: `dirname <path>` and the harness's own `_spira_join <base>
/// <rel>`. Anything else is refused by name rather than silently producing an empty string,
/// so a new command substitution in a future `conf.d` file fails its own test instead of
/// drifting from bash.
fn eval_command_sub(
    expr: &str,
    start: usize,
    known: &BTreeMap<String, String>,
) -> Result<(String, usize), String> {
    let close = find_matching_paren(expr, start)
        .ok_or_else(|| format!("eval_expr: unterminated '$(' in {expr:?}"))?;
    let inner = &expr[start..close];
    let words = shell_words(inner, known)?;
    let (cmd, args) = words.split_first().ok_or_else(|| format!("eval_expr: empty $() in {expr:?}"))?;
    let result = match cmd.as_str() {
        "dirname" => {
            let path = args.first().map(String::as_str).unwrap_or("");
            sh_dirname(path)
        }
        "_spira_join" => {
            let base = args.first().map(String::as_str).unwrap_or("");
            let rel = args.get(1).map(String::as_str).unwrap_or("");
            spira_join(base, rel)
        }
        other => return Err(format!("eval_expr: unsupported command substitution $({other} ...) in {expr:?}")),
    };
    Ok((result, close + 1))
}

fn find_matching_paren(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 1i32;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Splits `s` on whitespace, honouring a double-quoted segment (the only quoting the
/// registry's command substitutions use), and evaluates any `$...` inside each word.
fn shell_words(s: &str, known: &BTreeMap<String, String>) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut chars = s.chars().peekable();
    let mut cur = String::new();
    let mut in_word = false;
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            if in_word {
                words.push(eval_expr(&cur, known)?);
                cur.clear();
                in_word = false;
            }
            continue;
        }
        in_word = true;
        if c == '"' {
            while let Some(&n) = chars.peek() {
                if n == '"' {
                    chars.next();
                    break;
                }
                cur.push(n);
                chars.next();
            }
        } else {
            cur.push(c);
        }
    }
    if in_word {
        words.push(eval_expr(&cur, known)?);
    }
    Ok(words)
}

/// coreutils `dirname`, as far as this evaluator needs it: strips trailing slashes, then
/// everything after the last remaining slash. `""` and a path with no slash both become `.`;
/// a path that is only slashes becomes `/`.
pub fn sh_dirname(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return if path.is_empty() { ".".to_string() } else { "/".to_string() };
    }
    match trimmed.rfind('/') {
        None => ".".to_string(),
        Some(0) => "/".to_string(),
        Some(p) => trimmed[..p].to_string(),
    }
}

/// `_spira_join <base> <rel>` (spira/conf.sh): `base` with any trailing slash stripped,
/// joined to `rel` with exactly one slash — so a workspaces root of `/` does not double up.
pub fn spira_join(base: &str, rel: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn literal_passes_through() {
        assert_eq!(eval_expr("28", &known(&[])).unwrap(), "28");
        assert_eq!(eval_expr("ops groomer qa", &known(&[])).unwrap(), "ops groomer qa");
    }

    #[test]
    fn bare_variable_reference() {
        assert_eq!(eval_expr("$SPIRA_DB", &known(&[("SPIRA_DB", "/x/db")])).unwrap(), "/x/db");
    }

    #[test]
    fn unset_bare_variable_is_empty() {
        assert_eq!(eval_expr("$SPIRA_NOPE", &known(&[])).unwrap(), "");
    }

    #[test]
    fn braced_variable_with_literal_suffix() {
        assert_eq!(
            eval_expr("${SPIRA_HOME}/chamber", &known(&[("SPIRA_HOME", "/h")])).unwrap(),
            "/h/chamber"
        );
    }

    #[test]
    fn fallback_default_used_when_var_is_unset_or_empty() {
        assert_eq!(eval_expr("${SPIRA_WIKI:-$SPIRA_REPO}", &known(&[("SPIRA_REPO", "/r")])).unwrap(), "/r");
        assert_eq!(
            eval_expr(
                "${SPIRA_WIKI:-$SPIRA_REPO}",
                &known(&[("SPIRA_WIKI", "/w"), ("SPIRA_REPO", "/r")])
            )
            .unwrap(),
            "/w"
        );
        // Empty (set-but-blank) is treated the same as unset, matching bash's `:-`.
        assert_eq!(
            eval_expr("${SPIRA_WIKI:-$SPIRA_REPO}", &known(&[("SPIRA_WIKI", ""), ("SPIRA_REPO", "/r")])).unwrap(),
            "/r"
        );
    }

    #[test]
    fn xdg_style_fallback_with_literal_tail() {
        assert_eq!(
            eval_expr(
                "${XDG_CONFIG_HOME:-$HOME/.config}/spira/overrides",
                &known(&[("HOME", "/h")])
            )
            .unwrap(),
            "/h/.config/spira/overrides"
        );
        assert_eq!(
            eval_expr(
                "${XDG_CONFIG_HOME:-$HOME/.config}/spira/overrides",
                &known(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/h")])
            )
            .unwrap(),
            "/xdg/spira/overrides"
        );
    }

    #[test]
    fn dirname_command_substitution() {
        assert_eq!(
            eval_expr("$(dirname \"$SPIRA_HOME\")/cockpit", &known(&[("SPIRA_HOME", "/a/b/spira")])).unwrap(),
            "/a/b/cockpit"
        );
    }

    #[test]
    fn spira_join_command_substitution() {
        assert_eq!(
            eval_expr(
                "$(_spira_join \"$SPIRA_WORKSPACES\" beads-test)",
                &known(&[("SPIRA_WORKSPACES", "/ws/")])
            )
            .unwrap(),
            "/ws/beads-test"
        );
    }

    #[test]
    fn arithmetic_with_fallback_operand() {
        assert_eq!(
            eval_expr("$(( ${SPIRA_QUEUE_BATCH_MAX:-8} * 2 ))", &known(&[])).unwrap(),
            "16"
        );
        assert_eq!(
            eval_expr(
                "$(( ${SPIRA_QUEUE_BATCH_MAX:-8} * 2 ))",
                &known(&[("SPIRA_QUEUE_BATCH_MAX", "5")])
            )
            .unwrap(),
            "10"
        );
    }

    #[test]
    fn unsupported_command_substitution_is_refused_by_name() {
        let e = eval_expr("$(whoami)", &known(&[])).unwrap_err();
        assert!(e.contains("whoami"), "{e}");
    }

    #[test]
    fn dirname_matches_coreutils_on_edge_cases() {
        assert_eq!(sh_dirname("/a/b"), "/a");
        assert_eq!(sh_dirname("/a"), "/");
        assert_eq!(sh_dirname("a"), ".");
        assert_eq!(sh_dirname(""), ".");
        assert_eq!(sh_dirname("/"), "/");
    }
}
