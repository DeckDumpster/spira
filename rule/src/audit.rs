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

const COCKPIT: &str = "{{COCKPIT}}/";
const SPIRA_HOME: &str = "{{SPIRA_HOME}}/";
const BIN_PREFIX: &str = "../bin/";

fn placeholder_tokens(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for prefix in [COCKPIT, SPIRA_HOME] {
        let mut rest = line;
        while let Some(at) = rest.find(prefix) {
            let tail = &rest[at..];
            let end = tail.find(|c: char| c.is_whitespace() || matches!(c, '`' | '"' | '|' | ')')).unwrap_or(tail.len());
            out.push(tail[..end].trim_end_matches(['.', ',', ';', ':']));
            rest = &tail[end.max(prefix.len())..];
        }
    }
    out
}

/// Resolves a placeholder-rooted path against a release tree: `{{COCKPIT}}/x` is `cockpit/x`,
/// `{{SPIRA_HOME}}/../bin/x` a shipped bin, `{{SPIRA_HOME}}/x` is `spira/x`.
pub fn placeholder_resolver<'a>(root: &'a std::path::Path, bins: &'a std::collections::BTreeSet<String>) -> impl Fn(&str) -> bool + 'a {
    move |token: &str| {
        if let Some(rel) = token.strip_prefix(COCKPIT) {
            return root.join("cockpit").join(rel).exists();
        }
        let Some(rel) = token.strip_prefix(SPIRA_HOME) else { return false };
        match rel.strip_prefix(BIN_PREFIX) {
            Some(name) => bins.contains(name),
            None => root.join("spira").join(rel).exists(),
        }
    }
}

/// What a persona prompt names that the release does not have, as `runes/<name>:<line>: <token>`.
pub fn prompt_findings(
    name: &str,
    text: &str,
    resolve_path: &dyn Fn(&str) -> bool,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let mut miss: Vec<String> = placeholder_tokens(line).into_iter().filter(|t| !resolve_path(t)).map(str::to_string).collect();
        miss.extend(missing_refs(&extract_refs(line).into_iter().filter(|r| !r.tool.starts_with("{{")).collect::<Vec<_>>(), lookup));
        for t in miss {
            out.push(format!("runes/{name}:{}: {t}", i + 1));
        }
    }
    out
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
    if f.slug.starts_with("runes/") {
        return format!("Persona prompt {} names a tool or path that does not exist: {}", f.slug, f.missing.join(", "));
    }
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
    let read = move |name: &str| dirs.iter().map(|d| d.join(name)).find(|p| p.is_file()).map(|p| String::from_utf8_lossy(&std::fs::read(p).unwrap_or_default()).into_owned());
    move |name: &str| read(name).map(|text| follow_shim(&text, &read))
}

/// A shim script's verbs live in the binary it execs, so its text is read together with that
/// binary's: `exec -a "$0" <bin>` appends what `lookup(<bin>)` returns.
pub fn follow_shim(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let target = text.lines().find_map(|l| l.trim().strip_prefix("exec -a \"$0\" ")).and_then(|r| r.split_whitespace().next());
    match target.and_then(lookup) {
        Some(bin) => format!("{text}\n{bin}"),
        None => text.to_string(),
    }
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
    if title.starts_with("Persona prompt ") {
        return format!("{title}\n\nThe prompt names the above and the activated release has no such tool or path. Amend the prompt to name what exists.\n");
    }
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

    #[test_id(tgdcjox)]
    #[test]
    fn a_verb_the_script_names_passes() {
        let r = extract_refs("run bead.sh file \"title\" for it");
        assert!(missing_refs(&r, &release).is_empty());
    }

    #[test_id(tki9opp)]
    #[test]
    fn an_unresolved_bare_tool_is_not_ours_to_judge() {
        let r = extract_refs("run `cargo build` first");
        assert!(missing_refs(&r, &release).is_empty());
    }

    #[test_id(twosl4r)]
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

    #[test_id(ty0gnv0)]
    #[test]
    fn a_statute_naming_statute_check_is_clean_once_the_release_ships_the_binary() {
        let m: BTreeMap<String, serde_json::Value> =
            [("law-every-decision-passes-the-statute-check".to_string(), json!("Before a decision run `statute-check hook` on it"))].into();
        let shipped = |n: &str| (n == "statute-check").then(|| "usage: statute-check hook [opts]".to_string());
        assert!(audit(&m, &shipped).is_empty());
        let other_verb = |n: &str| (n == "statute-check").then(|| "usage: statute-check [opts]".to_string());
        assert_eq!(audit(&m, &other_verb).len(), 1, "positive control: a verb the binary lacks is a finding");
    }

    #[test]
    fn a_missing_cockpit_path_is_found_on_its_line() {
        let none = |_: &str| false;
        assert_eq!(prompt_findings("x.md", "ok\n{{COCKPIT}}/no-such.sh", &none, &release), vec!["runes/x.md:2: {{COCKPIT}}/no-such.sh".to_string()]);
    }

    #[test]
    fn a_shipped_bin_behind_spira_home_is_clean() {
        let bins: std::collections::BTreeSet<String> = ["reply".to_string()].into();
        let root = std::path::Path::new("/nonexistent-root");
        let r = placeholder_resolver(root, &bins);
        assert!(prompt_findings("x.md", "{{SPIRA_HOME}}/../bin/reply x", &r, &release).is_empty());
        assert_eq!(prompt_findings("x.md", "{{SPIRA_HOME}}/../bin/gone x", &r, &release).len(), 1);
    }

    #[test]
    fn a_shim_is_judged_by_the_binary_it_execs() {
        let shim = |n: &str| match n {
            "t.sh" => Some("exec -a \"$0\" tbin --home x \"$@\"".to_string()),
            "tbin" => Some("\"ideas\" => run".to_string()),
            _ => None,
        };
        let look = |n: &str| shim(n).map(|t| follow_shim(&t, &shim));
        assert!(prompt_findings("x.md", "see t.sh ideas", &|_| false, &look).is_empty());
        assert_eq!(prompt_findings("x.md", "see t.sh gone", &|_| false, &look).len(), 1);
    }

    #[test]
    fn a_script_the_release_lacks_is_flagged() {
        let none = |_: &str| false;
        assert_eq!(prompt_findings("x.md", "run gone.sh now", &none, &release), vec!["runes/x.md:1: gone.sh".to_string()]);
    }

    #[test]
    fn a_placeholder_rooted_script_is_not_judged_by_base_name() {
        let present = |t: &str| t == "{{COCKPIT}}/panel-run.sh";
        assert!(prompt_findings("x.md", "{{COCKPIT}}/panel-run.sh", &present, &release).is_empty());
    }
}
