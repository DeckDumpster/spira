//! The text the operator reads: lib.sh `bead_context`, CHECK 4's cause summaries.

use serde_json::Value;

use crate::model::{py_str, Bead};

/// python: "%dh" % h if h < 48 else "%dd" % (h / 24); "?" when unparseable or naive.
pub fn age(created: Option<&str>, now: i64) -> String {
    match created.and_then(crate::host::parse_iso) {
        Some(t) => {
            let h = (now - t) as f64 / 3600.0;
            if h < 48.0 {
                format!("{}h", h.trunc() as i64)
            } else {
                format!("{}d", (h / 24.0).trunc() as i64)
            }
        }
        None => "?".into(),
    }
}

fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace())
}

/// lib.sh `bead_context`, rendered from a row already in hand.
pub fn bead_context(b: &Bead, now: i64) -> String {
    let mut o = String::new();
    o.push_str(&format!(
        "BEAD    {}  [{}, P{}, open {}]\n",
        b.id,
        if b.status.is_empty() {
            "None".to_string()
        } else {
            b.status.clone()
        },
        py_str(b.priority.as_ref()),
        age(b.created_at.as_deref(), now)
    ));
    o.push_str(&format!(
        "TITLE   {}\n",
        b.title
            .as_deref()
            .filter(|t| !t.is_empty())
            .unwrap_or("(none)")
    ));
    let labs = if b.labels.is_empty() {
        "(none)".to_string()
    } else {
        b.labels.join(", ")
    };
    o.push_str(&format!("LABELS  {labs}\n\nWHAT THIS BEAD IS FOR\n"));
    let desc = b
        .description
        .as_deref()
        .filter(|d| !d.is_empty())
        .unwrap_or("(no description — that is itself the problem)");
    o.push_str(py_strip(desc));
    o.push('\n');
    let notes: Vec<String> = match &b.notes {
        Some(Value::String(s)) => s
            .split('\n')
            .filter(|n| !n.trim().is_empty())
            .map(str::to_string)
            .collect(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|n| match n {
                Value::Object(m) => py_str(m.get("text")),
                Value::String(s) => s.clone(),
                x => x.to_string(),
            })
            .collect(),
        _ => Vec::new(),
    };
    if !notes.is_empty() {
        o.push_str("\nMOST RECENT NOTES\n");
        for n in notes.iter().skip(notes.len().saturating_sub(3)) {
            let t: String = py_strip(n).chars().take(400).collect();
            o.push_str(&format!("  - {t}\n"));
        }
    }
    // $(…) strips trailing newlines.
    o.trim_end_matches('\n').to_string()
}

/// The `sp-<kind>-N[-cause]` legacy labels as "<cause> xN, …" (sort -n by N), else
/// "unrecorded". Counter labels are no longer written (sp-lzt); kept for old beads.
pub fn causes(labels_csv: &str, kind: &str) -> String {
    let prefix = format!("sp-{kind}-");
    let mut rows: Vec<(u64, String, String)> = labels_csv
        .split(',')
        .filter_map(|l| {
            let rest = l.strip_prefix(&prefix)?;
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                return None;
            }
            let after = &rest[digits.len()..];
            let cause = if after.is_empty() {
                "unrecorded".to_string()
            } else {
                after.strip_prefix('-')?.to_string()
            };
            let line = format!("{digits} {cause}");
            Some((digits.parse().unwrap_or(0), cause, line))
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
    let s = rows
        .iter()
        .map(|(n, c, _)| format!("{c} x{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    if s.is_empty() {
        "unrecorded".into()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::parse_beads;

    #[test]
    fn context_matches_the_python() {
        let b = &parse_beads(
            r#"{"id":"sp-x","status":"open","priority":1,"created_at":"2026-09-28T00:00:00Z","title":"T","labels":["a","b"],
                "description":"  do it  \n","notes":"one\n\ntwo\nthree\nfour"}"#,
        )
        .unwrap()[0];
        let now = crate::host::parse_iso("2026-09-28T05:30:00Z").unwrap();
        assert_eq!(
            bead_context(b, now),
            "BEAD    sp-x  [open, P1, open 5h]\nTITLE   T\nLABELS  a, b\n\nWHAT THIS BEAD IS FOR\ndo it\n\nMOST RECENT NOTES\n  - two\n  - three\n  - four"
        );
        let bare = &parse_beads(r#"{"id":"sp-y","created_at":"2026-09-20T00:00:00Z"}"#).unwrap()[0];
        let now = crate::host::parse_iso("2026-09-28T00:00:00Z").unwrap();
        assert_eq!(
            bead_context(bare, now),
            "BEAD    sp-y  [None, PNone, open 8d]\nTITLE   (none)\nLABELS  (none)\n\nWHAT THIS BEAD IS FOR\n(no description — that is itself the problem)"
        );
    }

    #[test]
    fn causes_sort_numerically_and_default() {
        assert_eq!(
            causes(
                "a,sp-requeue-2-rebase-conflict,sp-requeue-10,sp-requeue-1-red",
                "requeue"
            ),
            "red x1, rebase-conflict x2, unrecorded x10"
        );
        assert_eq!(causes("sp-reclaim-3-oom", "reclaim"), "oom x3");
        assert_eq!(causes("plan,spira", "requeue"), "unrecorded");
        assert_eq!(causes("sp-requeue-x", "requeue"), "unrecorded");
    }
}
