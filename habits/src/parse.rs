use std::path::Path;

pub struct Finding {
    pub start: u32,
    pub end: u32,
    /// Stable across edits, so a finding can be recognised as gone at the next run.
    pub key: String,
    pub text: String,
}

fn clip(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() > 110 {
        format!("{}…", s.chars().take(110).collect::<String>())
    } else {
        s.to_string()
    }
}

pub fn same_file(reported: &str, file: &Path) -> bool {
    let r = Path::new(reported.strip_prefix("./").unwrap_or(reported));
    r == file || file.ends_with(r)
}

/// `path:line:col: severity: message` lines (gcc, `shellcheck -f gcc`, `cargo --message-format short`).
pub fn gcc(out: &str) -> Vec<(String, u32, String)> {
    out.lines()
        .filter_map(|l| {
            let mut it = l.splitn(4, ':');
            let (path, line, col, rest) = (it.next()?, it.next()?, it.next()?, it.next()?);
            col.trim().parse::<u32>().ok()?;
            Some((path.to_string(), line.trim().parse().ok()?, rest.trim().to_string()))
        })
        .collect()
}

pub fn gcc_findings(out: &str, file: &Path) -> Vec<Finding> {
    gcc(out)
        .into_iter()
        .filter(|(p, _, _)| same_file(p, file))
        .map(|(_, line, msg)| Finding { start: line, end: line, key: msg.clone(), text: format!("line {line}: {}", clip(&msg)) })
        .collect()
}

/// `rustfmt --check` diffs: one finding per run of changed lines, anchored on the old-side lines.
pub fn rustfmt(out: &str, file: &Path) -> Vec<Finding> {
    let mut found = Vec::new();
    let mut live = false;
    let mut old = 0u32;
    let (mut minus, mut plus): (Vec<(u32, String)>, Vec<String>) = (Vec::new(), Vec::new());
    let mut flush = |old: u32, minus: &mut Vec<(u32, String)>, plus: &mut Vec<String>| {
        if minus.is_empty() && plus.is_empty() {
            return;
        }
        let (start, end) = (minus.first().map_or(old, |m| m.0), minus.last().map_or(old, |m| m.0));
        let text = match (minus.first(), plus.first()) {
            (Some((_, m)), Some(p)) => format!("rustfmt would change `{}` to `{}`", clip(m), clip(p)),
            (Some((_, m)), None) => format!("rustfmt would remove `{}`", clip(m)),
            (None, p) => format!("rustfmt would add `{}`", clip(p.map_or("", String::as_str))),
        };
        found.push(Finding { start, end, key: text.clone(), text: format!("line {start}: {text}") });
        minus.clear();
        plus.clear();
    };
    for l in out.lines() {
        if let Some(h) = l.strip_prefix("Diff in ") {
            flush(old, &mut minus, &mut plus);
            let h = h.trim_end_matches(':');
            let (path, line) = match h.split_once(" at line ") {
                Some((p, n)) => (p, n),
                None => h.rsplit_once(':').unwrap_or((h, "0")),
            };
            live = same_file(path, file);
            old = line.trim().parse().unwrap_or(0);
        } else if live {
            match l.as_bytes().first() {
                Some(b'-') => {
                    minus.push((old, l[1..].to_string()));
                    old += 1;
                }
                Some(b'+') => plus.push(l[1..].to_string()),
                _ => {
                    flush(old, &mut minus, &mut plus);
                    old += 1;
                }
            }
        }
    }
    flush(old, &mut minus, &mut plus);
    found
}

pub struct Func {
    pub name: String,
    pub start: u32,
    pub end: u32,
    pub cognitive: f64,
}

/// Named functions from `rust-code-analysis-cli -m -O json`; closures roll up into their function.
pub fn functions(json: &str) -> Option<Vec<Func>> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let mut out = Vec::new();
    collect(&v, &mut out);
    Some(out)
}

fn collect(space: &serde_json::Value, out: &mut Vec<Func>) {
    if space["kind"] == "function" && space["name"] != "<anonymous>" {
        if let (Some(name), Some(s), Some(e), Some(c)) =
            (space["name"].as_str(), space["start_line"].as_u64(), space["end_line"].as_u64(), space["metrics"]["cognitive"]["sum"].as_f64())
        {
            out.push(Func { name: name.to_string(), start: s as u32, end: e as u32, cognitive: c });
        }
    }
    for child in space["spaces"].as_array().into_iter().flatten() {
        collect(child, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rustfmt_diff_anchors_on_old_lines() {
        let out = "Diff in /r/src/a.rs:10:\n fn ok() {}\n-let x=1;\n+let x = 1;\n fn next() {}\n";
        let f = rustfmt(out, Path::new("/r/src/a.rs"));
        assert_eq!((f.len(), f[0].start, f[0].end), (1, 11, 11));
        assert!(f[0].text.contains("let x=1;") && f[0].text.contains("let x = 1;"));
        assert!(rustfmt(out, Path::new("/r/src/b.rs")).is_empty(), "another file's diff is not this file's");
    }

    #[test]
    fn gcc_lines() {
        let f = gcc_findings("/r/a.sh:4:9: note: Double quote to prevent globbing. [SC2086]\nnoise\n", Path::new("/r/a.sh"));
        assert_eq!((f.len(), f[0].start), (1, 4));
    }
}
