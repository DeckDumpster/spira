//! The Rust half of design §3.4 ("bd status is inert for work beads", sp-mve9i): a Rust
//! decision that reads a bead's bd `status` or `assignee`. bd holds content; spira-lc holds
//! state — a work bead's READY/WORKING/LANDED is `spira-lc show`/`list`, its holder is the
//! lifecycle row's `holder`, and nothing reads either off a bd row to decide something.
//!
//! The shell half is `lifecycle-read` (shell.rs, a bd show/list in a conditional). Rust has
//! no grammar in this crate, so this is a line-oriented pass over the masked source
//! (`landstate::mask_rust`: comments blanked, string contents blanked), with a short,
//! identifier-like literal's text restored so `"closed"` and `"status"` stay visible while
//! prose inside a longer string never matches. Four shapes are a finding:
//!
//! 1. **compare** — a bd status value (`"open"`, `"in_progress"`, `"closed"`, …) compared
//!    against a status-named expression on the same line: `b.status == "closed"`,
//!    `r.get("status")… != Some("closed")`, `matches!(b.status.as_str(), "open" | …)`.
//! 2. **match** — an arm on a bd status value inside a `match` over a status-named
//!    expression (`match status { "closed" => … }`, `match (st, assignee) { ("in_progress", …`).
//! 3. **query** — a bd query or write filtered by status: a `"--status"` argument (gh spells
//!    its own `--state`, so the flag is bd's alone), or a bd status value handed to a
//!    `…list…(` call (`bd.list(db, &["open", "in_progress"], …)`).
//! 4. **assignee** — a bd row's `assignee` read in a condition (`if`/`match`/`==`/`!=`/
//!    `is_some`/`is_none`): the claim's holder is the lifecycle row's, not bd's.
//!
//! Scope, not an allow-list: test code (a `tests/` directory, a `tests.rs`/`*_tests.rs`
//! file, or what follows a `#[cfg(test)]` module header) decides nothing in production; the
//! `lifecycle` crate is the machine; and spira-lc's `bd_facts.rs` is the one-time migration
//! classifier's reader of bd (design §4), which seeds the machine from bd once and goes with
//! the classifier. A bead that is not a work bead — an ask, an alert, an insight, an epic —
//! has no lifecycle row, and its bd status is its only state; a decision over those names
//! the kind it reads through `spira_config::nonwork` (spira-config/src/nonwork.rs), whose one module is the
//! scope that carries bd status for non-work beads.

use crate::finding::{Class, Finding};
use crate::landstate::mask_rust;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// bd's status vocabulary (bd 0.5x `--status` values).
const BD_STATUSES: &str = "open|in_progress|closed|blocked|deferred|tombstone|hooked|pinned";

struct Res {
    status_tok: Regex,
    cmp_right: Regex,
    cmp_left: Regex,
    cmp_matches: Regex,
    cmp_contains: Regex,
    boundary: Regex,
    lit: Regex,
    arm: Regex,
    match_head: Regex,
    status_flag: Regex,
    status_flag_bare: Regex,
    status_value_line: Regex,
    status_var: Regex,
    other_states: Regex,
    list_call: Regex,
    assignee: Regex,
    assignee_cond: Regex,
}

fn res() -> &'static Res {
    static R: OnceLock<Res> = OnceLock::new();
    R.get_or_init(|| {
        let lit = format!(r#""(?:{BD_STATUSES})""#);
        Res {
            // A status-named expression: an identifier with `status` in it (not a type: those
            // are capitalised, and `.status()` is a process's ExitStatus), the key `"status"`,
            // or the short local `st` the aeon and landing-pass use for one.
            status_tok: Regex::new(r#"(?:\b[a-z_]*status[a-z_]*\b(?:\s*[^\s(]|\s*$)|"status"|\bst\b)"#).unwrap(),
            cmp_right: Regex::new(&format!(r#"(?:==|!=)\s*(?:Some\(\s*)?{lit}"#)).unwrap(),
            cmp_left: Regex::new(&format!(r#"{lit}\s*\)?\s*(?:==|!=)"#)).unwrap(),
            cmp_matches: Regex::new(&format!(r#"matches!\(([^"]*?){lit}"#)).unwrap(),
            cmp_contains: Regex::new(&format!(r#"\[[^\]]*{lit}[^\]]*\]\s*\.contains\((.*)"#)).unwrap(),
            boundary: Regex::new(r"&&|\|\||[{;|]|=>|\s=\s").unwrap(),
            lit: Regex::new(&lit).unwrap(),
            arm: Regex::new(&format!(r#"{lit}[\s)|,_a-zA-Z(]*(?:\|[^=]*)?=>"#)).unwrap(),
            match_head: Regex::new(r"\bmatch\b").unwrap(),
            status_flag: Regex::new(&format!(r#""--status=(?:{BD_STATUSES})|"--status"\s*,\s*"(?:{BD_STATUSES})(?:,(?:{BD_STATUSES}))*""#)).unwrap(),
            status_flag_bare: Regex::new(r#""--status"(?:\.into\(\)|\.to_string\(\))?\s*[,)]\s*(?:\S.*)?$"#).unwrap(),
            status_value_line: Regex::new(&format!(r#"^\s*(?:\.arg\(|\.push\()?"(?:{BD_STATUSES})(?:,(?:{BD_STATUSES}))*""#)).unwrap(),
            status_var: Regex::new(r#""list".*"--status"\s*,\s*&?[a-z_]*status|"--status"\s*,\s*&?[a-z_]*statuses|"list".*"--status"\s*,\s*[A-Z][A-Z_]*\b|"--status"\.into\(\)\s*,\s*[a-z_]*status"#).unwrap(),
            other_states: Regex::new(r#""(?:queued|completed|merged|success|failure|waiting|pending|requested|cancelled)""#).unwrap(),
            list_call: Regex::new(&format!(r#"\b[a-z_]*list[a-z_]*\s*\([^;]*{lit}"#)).unwrap(),
            assignee: Regex::new(r#"(?:\.assignee\b|"assignee")"#).unwrap(),
            assignee_cond: Regex::new(r"(?:\bif\b|\bwhile\b|\bmatch\b|==|!=|is_some|is_none|is_some_and)").unwrap(),
        }
    })
}

/// A bd status value compared against a status-named operand: the operand is the piece of
/// the line between the comparison and the nearest boundary before it (`&&`, `||`, `{`, `;`,
/// a closure's `|`, `=>`, an assignment), so `o.status.success() && pr == "closed"` is a
/// PR's state, not a bead's. A line that reads the `"status"` key of a JSON row counts any
/// comparison on it (`get("status")…map(|s| s == "closed")`).
fn compares_status(r: &Res, n: &str) -> bool {
    let keyed = n.contains("\"status\"");
    let operand_is_status = |piece: &str| keyed || r.status_tok.is_match(piece);
    for m in r.cmp_right.find_iter(n) {
        let left = &n[..m.start()];
        let piece = r.boundary.split(left).last().unwrap_or(left);
        if operand_is_status(piece) {
            return true;
        }
    }
    for m in r.cmp_left.find_iter(n) {
        let right = &n[m.end()..];
        let piece = r.boundary.split(right).next().unwrap_or(right);
        if operand_is_status(piece) {
            return true;
        }
    }
    if let Some(c) = r.cmp_matches.captures(n) {
        if operand_is_status(c.get(1).map_or("", |g| g.as_str())) {
            return true;
        }
    }
    if let Some(c) = r.cmp_contains.captures(n) {
        let arg = c.get(1).map_or("", |g| g.as_str());
        if operand_is_status(arg.split(')').next().unwrap_or(arg)) {
            return true;
        }
    }
    false
}

/// The rowless controls — the rule's two NAMED exceptions (DESIGN.md "The rowless
/// controls", the Concierge's ruling on sp-mve9i): `(file, function)`. Each is the positive
/// control for "no bead is rowless": a bead with no lifecycle row has no state but bd's, so
/// finding one means asking bd which beads it considers live and looking for each in the
/// machine. That read audits the machine's coverage; it decides nothing about a bead the
/// machine holds. Named here, by file and function, and argued at each definition — not an
/// allow-list file, and a third entry is a design change, not a configuration.
pub const ROWLESS_CONTROLS: &[(&str, &str)] =
    &[("sentinel/src/lifecycle.rs", "rowless"), ("watchtower/src/probes.rs", "rowless_beads")];

/// The off claim record — the rule's other NAMED exceptions (DESIGN.md "The off claim
/// record", argued for spira-claim in spira-claim/DESIGN.md §8.7): `(file, function)`. With
/// `lifecycle_enforce` off an aeon claims through bd (sp-860zj kept that path), so bd's
/// `in_progress` and assignee are the claim and no lifecycle row records it; spira-claim's
/// live-work guard must read that claim, and its reopen must release it, or the off path
/// loses its safety and its ready set. Each function is called only with the switch off and
/// goes with the off path; a further entry is a design change argued there, not a
/// configuration.
pub const OFF_CLAIM_RECORD: &[(&str, &str)] =
    &[("spira-claim/src/bd_claim.rs", "off_holder"), ("spira-claim/src/bd_claim.rs", "off_release_args")];

/// Test code decides nothing in production: a file under a `tests/` directory, or named
/// `tests.rs` / `*_tests.rs`.
fn is_test_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    rel.starts_with("tests/") || rel.contains("/tests/") || name == "tests.rs" || name.ends_with("_tests.rs")
}

/// The machine itself and its one migration reader of bd (module doc).
fn in_scope(rel: &str) -> bool {
    !(rel.starts_with("lifecycle/")
        || rel.starts_with("lifecycle-guard/")
        || rel == "spira-lc/src/bd_facts.rs" || rel == "spira-config/src/nonwork.rs")
}

/// `code` (masked) with every short, identifier-like string literal's text put back from
/// `orig`, byte for byte (masking keeps lengths), so `"closed"` reads as itself while a
/// long or escaped literal stays blank.
fn restore_short_literals(orig: &str, code: &str) -> String {
    let ob = orig.as_bytes();
    let mut cb = code.as_bytes().to_vec();
    let mut i = 0;
    while i < cb.len() {
        if cb[i] == b'"' {
            if let Some(off) = cb[i + 1..].iter().position(|&c| c == b'"') {
                let end = i + 1 + off;
                let body = &ob[i + 1..end.min(ob.len())];
                if !body.is_empty()
                    && body.len() <= 64
                    && body.iter().all(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'=' || c == b',')
                {
                    cb[i + 1..end].copy_from_slice(body);
                }
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }
    String::from_utf8(cb).unwrap_or_default()
}

pub fn scan_rust(files: &[PathBuf], root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    for path in files {
        let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/");
        if is_test_file(&rel) || !in_scope(&rel) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        findings.extend(scan_text(&rel, &text));
    }
    findings
}

pub fn scan_text(rel: &str, text: &str) -> Vec<Finding> {
    let r = res();
    let code = mask_rust(text);
    let lines: Vec<String> = text.lines().zip(code.lines()).map(|(o, c)| restore_short_literals(o, c)).collect();
    let mut out = Vec::new();
    // The status-named `match` heads still open: (brace depth at the head, line).
    let mut depth: i64 = 0;
    let mut heads: Vec<i64> = Vec::new();
    let controls: Vec<&str> =
        ROWLESS_CONTROLS.iter().chain(OFF_CLAIM_RECORD).filter(|(f, _)| *f == rel).map(|(_, n)| *n).collect();
    // Inside a rowless control: the brace depth its `fn` line opened from.
    let mut control: Option<i64> = None;
    for (idx, n) in lines.iter().enumerate() {
        if n.trim_start().starts_with("#[cfg(test)]")
            && lines.get(idx + 1).is_some_and(|l| l.trim_start().starts_with("mod ") || l.contains(" mod "))
        {
            break;
        }
        if control.is_none()
            && controls.iter().any(|name| {
                n.split("fn ").skip(1).any(|rest| rest.starts_with(name) && rest[name.len()..].trim_start().starts_with(['(', '<']))
            })
        {
            control = Some(depth);
        }
        let in_control = control.is_some();
        let has_status = r.status_tok.is_match(n);
        let mut push = |shape: &str, detail: String| {
            if in_control {
                return;
            }
            out.push(Finding {
                class: Class::BdStatusRead,
                file: rel.to_string(),
                line: idx + 1,
                function: None,
                callee: Some(shape.to_string()),
                detail,
            });
        };
        let flag_next = r.status_flag_bare.is_match(n)
            && lines[idx + 1..].iter().find(|l| !l.trim().is_empty()).is_some_and(|l| r.status_value_line.is_match(l));
        if r.other_states.is_match(n) {
            // A line that also names a GitHub run/PR state ("queued", "merged", …) is about
            // that state, not a bead's.
        } else if has_status && compares_status(r, n) {
            push("compare", "a bd status compared to decide; read the bead's state from spira-lc (show/list/state) instead".into());
        } else if !heads.is_empty() && r.arm.is_match(n) {
            push("match", "a match arm on a bd status; read the bead's state from spira-lc (show/list/state) instead".into());
        } else if r.status_flag.is_match(n) || flag_next || r.status_var.is_match(n) || r.list_call.is_match(n) {
            push("query", "a bd query or write filtered by status; ask spira-lc list --state for a work bead's state, or spira_config::nonwork for a non-work bead".into());
        } else if r.assignee.is_match(n) && r.assignee_cond.is_match(n) {
            push("assignee", "a bd assignee read in a condition; a work bead's holder is spira-lc's (show: holder)".into());
        }
        let opens = n.matches('{').count() as i64;
        let closes = n.matches('}').count() as i64;
        if r.match_head.is_match(n) && has_status && opens > closes && !r.lit.is_match(n.split("match").next().unwrap_or("")) {
            heads.push(depth);
        }
        depth += opens - closes;
        if control.is_some_and(|d| depth <= d && opens + closes > 0) {
            control = None;
        }
        while heads.last().is_some_and(|&d| depth <= d) {
            heads.pop();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shapes(src: &str) -> Vec<(usize, String)> {
        scan_text("x/src/a.rs", src).into_iter().map(|f| (f.line, f.callee.unwrap())).collect()
    }

    #[test]
    fn each_shape_is_found() {
        let src = r#"fn a(b: &Bead) -> bool { b.status == "closed" }
fn b(r: &Value) -> bool { r.get("status").and_then(|s| s.as_str()) != Some("closed") }
fn c(b: &Bead) -> bool { matches!(b.status.as_str(), "open" | "in_progress") }
fn d(st: &str) { if st != "closed" { x() } }
fn e(status: &str) -> u8 {
    match status {
        "closed" => 1,
        _ => 0,
    }
}
fn f() { bd(&["list", "--status", "open", "--json"]); }
fn g() { let _ = bd.list(db, &["open", "in_progress"], None, None); }
fn h(b: &Bead) -> bool { if b.assignee.is_some() { true } else { false } }
"#;
        assert_eq!(
            shapes(src),
            vec![
                (1, "compare".into()),
                (2, "compare".into()),
                (3, "compare".into()),
                (4, "compare".into()),
                (7, "match".into()),
                (11, "query".into()),
                (12, "query".into()),
                (13, "assignee".into()),
            ]
        );
    }

    #[test]
    fn prose_other_states_and_tests_are_not_bd_status_reads() {
        let src = r#"// b.status == "closed" was the old check
fn a() { eprintln!("status == \"closed\" is never read"); }
fn b(o: &Output) -> bool { o.status.success() }
fn c(s: &str) -> Kind { match s { "closed" => Kind::C, _ => Kind::O } }
fn d(pr: &str) -> bool { pr == "closed" }
fn e(gh: &Gh) { gh.call(&["pr", "list", "--state", "open"]); }
fn f(q: &Path) -> PathBuf { q.join("open") }
fn g(b: &Bead) -> String { b.assignee.clone().unwrap_or_default() }
#[cfg(test)]
mod tests {
    fn t(b: &Bead) { assert!(b.status == "closed"); }
}
"#;
        assert_eq!(shapes(src), Vec::<(usize, String)>::new());
    }

    #[test]
    fn a_match_closes_with_its_braces() {
        let src = r#"fn e(status: &str) -> u8 {
    let n = match status {
        "open" => 1,
        _ => 0,
    };
    match kind {
        "open" => 2,
        _ => n,
    }
}
"#;
        assert_eq!(shapes(src), vec![(3, "match".into())]);
    }

    /// The rowless controls are exempt by file and function, and only there: the same body
    /// in another function, or in another file, is a finding.
    #[test]
    fn the_rowless_controls_are_named_exceptions_and_nothing_else_is() {
        let body = |name: &str| {
            format!(
                "pub fn {name}(snap: &Snapshot) -> Vec<String> {{\n    snap.list.iter().filter(|b| matches!(b.status.as_str(), \"open\" | \"in_progress\")).map(|b| b.id.clone()).collect()\n}}\nfn after(b: &Bead) -> bool {{ b.status == \"closed\" }}\n"
            )
        };
        let lines = |rel: &str, name: &str| scan_text(rel, &body(name)).into_iter().map(|f| f.line).collect::<Vec<_>>();
        assert_eq!(lines("sentinel/src/lifecycle.rs", "rowless"), vec![4], "the control is exempt, the fn after it is not");
        assert_eq!(lines("watchtower/src/probes.rs", "rowless_beads"), vec![4]);
        assert_eq!(lines("sentinel/src/lifecycle.rs", "rowless_too"), vec![2, 4], "a name that only starts the same");
        assert_eq!(lines("sentinel/src/store.rs", "rowless"), vec![2, 4], "the same name in another file");
    }

    /// The off claim record: spira-claim's two named functions, and only those, in only
    /// that file.
    #[test]
    fn the_off_claim_record_is_named_by_file_and_function() {
        let src = "pub fn off_holder(b: &BeadRecord) -> Option<String> {\n    (b.status == \"in_progress\").then(|| String::new())\n}\npub fn off_release_args(id: &str) -> [&str; 4] {\n    [\"update\", id, \"--status\", \"open\"]\n}\nfn other(b: &BeadRecord) -> bool { b.status == \"closed\" }\n";
        let lines = |rel: &str| scan_text(rel, src).into_iter().map(|f| f.line).collect::<Vec<_>>();
        assert_eq!(lines("spira-claim/src/bd_claim.rs"), vec![7], "the two named functions are exempt, the third is not");
        assert_eq!(lines("spira-claim/src/unpoison.rs"), vec![2, 5, 7], "the same names in another file");
    }

    #[test]
    fn test_files_and_the_machine_are_out_of_scope() {
        assert!(is_test_file("sentinel/src/tests.rs"));
        assert!(is_test_file("spira-claim/src/unpoison_tests.rs"));
        assert!(is_test_file("loom/tests/endpoint.rs"));
        assert!(!is_test_file("sentinel/src/store.rs"));
        assert!(!in_scope("lifecycle/src/classify.rs"));
        assert!(!in_scope("spira-lc/src/bd_facts.rs"));
        assert!(in_scope("spira-lc/src/close_on_land.rs"));
    }
}
