//! The statute audit: which tools and verbs does each statute name, and does the activated
//! release have them. Pure — the caller supplies the lookup that reads the release.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRef {
    pub tool: String,
    pub verb: Option<String>,
    /// A bare name (`queue open-batch`) is checked only when it resolves; a script name
    /// (`bead.sh drop`) is checked even when it does not, because naming it is the claim.
    pub script: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub slug: String,
    pub missing: Vec<String>,
}

const EDGE: &[char] = &['`', '\'', '"', '(', ')', ',', ';', ':', '.', '[', ']'];

fn clean(tok: &str) -> &str {
    tok.trim_start_matches(|c| c != '.' && EDGE.contains(&c)).trim_end_matches(EDGE)
}

fn is_verb(tok: &str) -> bool {
    let mut c = tok.chars();
    c.next().is_some_and(|f| f.is_ascii_lowercase())
        && tok.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !tok.ends_with("ed")
}

fn is_script(tok: &str) -> bool {
    tok.ends_with(".sh")
        && tok.len() > 3
        && !tok.contains(['<', '>', '*', '{', '}', '$', '|'])
}

fn base(tool: &str) -> &str {
    tool.rsplit('/').next().unwrap_or(tool)
}

pub fn extract_refs(text: &str) -> Vec<ToolRef> {
    let mut out: Vec<ToolRef> = Vec::new();
    let mut push = |r: ToolRef| {
        if !out.contains(&r) {
            out.push(r);
        }
    };
    let toks: Vec<&str> = text.split_whitespace().collect();
    for (i, raw) in toks.iter().enumerate() {
        let t = clean(raw);
        if is_script(t) {
            let verb = toks.get(i + 1).map(|n| clean(n)).filter(|n| is_verb(n) && !n.ends_with(".sh"));
            let verb = verb.filter(|_| !raw.ends_with(['.', ',', ';', ':']));
            push(ToolRef { tool: t.to_string(), verb: verb.map(str::to_string), script: true });
        }
    }
    for (n, span) in text.split('`').enumerate() {
        if n % 2 == 0 {
            continue;
        }
        let w: Vec<&str> = span.split_whitespace().collect();
        if let [tool, verb, ..] = w[..] {
            if is_verb(tool) && is_verb(verb) {
                push(ToolRef { tool: tool.to_string(), verb: Some(verb.to_string()), script: false });
            }
        }
    }
    out
}

/// `lookup(name)` is the content of the named tool in the activated release, `None` if the
/// release has no such file.
pub fn missing_refs(refs: &[ToolRef], lookup: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
    let mut out = Vec::new();
    for r in refs {
        let name = base(&r.tool);
        match lookup(name) {
            None if r.script => out.push(r.tool.clone()),
            None => {}
            Some(content) => {
                if let Some(v) = &r.verb {
                    if !names_verb(&content, name, v) {
                        out.push(format!("{} {}", r.tool, v));
                    }
                }
            }
        }
    }
    out
}

/// A verb is implemented where the tool's own text pairs it with the tool's name or quotes
/// it as a dispatch word; a bare mention in prose does not count.
fn names_verb(content: &str, tool: &str, verb: &str) -> bool {
    if !tool.ends_with(".sh") {
        return content
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .any(|w| w == verb);
    }
    content.contains(&format!("{tool} {verb}"))
        || content.contains(&format!("\"{verb}\""))
        || content.contains(&format!("\n    {verb})"))
        || content.contains(&format!("\n  {verb})"))
        || content.contains(&format!("|{verb})"))
        || content.contains(&format!("{verb}|"))
}

pub fn audit(
    memories: &BTreeMap<String, serde_json::Value>,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Vec<Finding> {
    memories
        .iter()
        .filter(|(k, _)| k.starts_with("law-"))
        .filter_map(|(k, v)| v.as_str().map(|t| (k, t)))
        .filter_map(|(k, t)| {
            let missing = missing_refs(&extract_refs(t), lookup);
            (!missing.is_empty()).then(|| Finding { slug: k.clone(), missing })
        })
        .collect()
}

pub fn finding_title(f: &Finding) -> String {
    format!("Statute {} names a tool or verb that does not exist: {}", f.slug, f.missing.join(", "))
}

/// Every tool name the book's statutes name, by the base name the lookup is asked for: what a
/// caller holding the release must read so [`audit`] can be served from memory.
pub fn named_tools(memories: &BTreeMap<String, serde_json::Value>) -> std::collections::BTreeSet<String> {
    memories
        .iter()
        .filter(|(k, _)| k.starts_with("law-"))
        .filter_map(|(_, v)| v.as_str())
        .flat_map(extract_refs)
        .map(|r| base(&r.tool).to_string())
        .collect()
}

/// The activated release as `audit --file` reads it: `SPIRA_HOME`, its parent, then the parent's
/// `bin/` and `model-bin/`, the first regular file of that name.
pub fn release_lookup(home: &str) -> impl Fn(&str) -> Option<String> {
    let home = std::path::Path::new(home);
    let root = home.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    let dirs = [home.to_path_buf(), root.clone(), root.join("bin"), root.join("model-bin")];
    move |name: &str| dirs.iter().map(|d| d.join(name)).find(|p| p.is_file()).map(|p| String::from_utf8_lossy(&std::fs::read(p).unwrap_or_default()).into_owned())
}

/// The findings `audit --file` files, in order: a finding whose slug appears in any open title
/// is skipped, and at most `max` are kept; the second value is whether the bound cut the rest.
pub fn to_file<'a>(findings: &'a [Finding], open_titles: &[String], max: usize) -> (Vec<&'a Finding>, bool) {
    let fresh: Vec<&Finding> = findings.iter().filter(|f| !open_titles.iter().any(|t| t.contains(&f.slug))).collect();
    let bounded = fresh.len() > max;
    (fresh.into_iter().take(max).collect(), bounded)
}

/// The body of the bead filed for a finding titled `title`.
pub fn bead_body(title: &str) -> String {
    format!("{title}\n\nThe statute names the above and the activated release has no such tool or verb. Build it, or amend the statute to name what exists (rule.sh enact with --tool).\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release(name: &str) -> Option<String> {
        match name {
            "bead.sh" => Some("#   bead.sh file \"<title>\"\n#   bead.sh lint".to_string()),
            "queue" => Some("open-batch status".to_string()),
            _ => None,
        }
    }

    #[test]
    fn a_verb_the_script_names_passes() {
        let r = extract_refs("run bead.sh file \"title\" for it");
        assert!(missing_refs(&r, &release).is_empty());
    }

    #[test]
    fn an_unresolved_bare_tool_is_not_ours_to_judge() {
        let r = extract_refs("run `cargo build` first");
        assert!(missing_refs(&r, &release).is_empty());
    }

    #[test]
    fn audit_reports_one_finding_per_offending_statute() {
        let m: BTreeMap<String, serde_json::Value> = [
            ("law-bad".to_string(), json!("run bead.sh drop <id>")),
            ("law-good".to_string(), json!("run bead.sh lint")),
            ("note-x".to_string(), json!("run bead.sh drop")),
        ]
        .into();
        let f = audit(&m, &release);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].slug, "law-bad");
    }
}
