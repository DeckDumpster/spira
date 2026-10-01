//! `render_memories`/`system_prompt_split`, ported from `spira/lib.sh` (wave4-decomposition.md
//! row Q, wave 4.35, sp-kelr2). Pure string transforms; the cache read/write and the
//! `SPIRA_MEMORIES_CMD`/`bdq` seam that fills `mem_json` live in `main.rs` (I/O at the edge,
//! same split every crate in this workspace draws). `groom_claims_verified`/
//! `bead_named_paths`, row Q's other two functions, are a separate bead's scope (aeon-only,
//! wave4-decomposition.md bead 34) and are not touched here.

use std::collections::BTreeMap;

/// `render_memories <prefixes> [budget] [core]` over `bd memories --json`'s object (already
/// read, as a string, by the caller). Core statutes render in full (budget allowing); every
/// other matching key becomes a slug in its namespace's index. Trailing newline stripped —
/// `main.rs` feeds this straight to `println!`, which adds its own.
pub fn render(mem_json: &str, prefixes: &str, budget: usize, core_csv: &str, harness: &str) -> String {
    let Ok(serde_json::Value::Object(d)) = serde_json::from_str::<serde_json::Value>(mem_json.trim()) else {
        return String::new();
    };
    let prefixes: Vec<&str> = prefixes.split(',').filter(|p| !p.is_empty()).collect();
    let core: std::collections::BTreeSet<&str> = core_csv.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    let mem: BTreeMap<&str, String> = d
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.as_str(), s.trim().to_string())))
        .filter(|(k, _)| prefixes.iter().any(|p| k.starts_with(p)))
        .collect();

    let (mut core_out, mut used, mut fallback, mut index) = (Vec::new(), 0usize, Vec::new(), Vec::new());
    for (k, v) in &mem {
        if core.contains(k) {
            let block = format!("## {k}\n\n{v}\n");
            let len = block.chars().count();
            if used + len > budget {
                fallback.push(*k);
            } else {
                core_out.push(block);
                used += len;
            }
        } else {
            index.push(*k);
        }
    }
    index.extend(fallback);
    index.sort();

    let mut parts = Vec::new();
    if !core_out.is_empty() {
        parts.push(core_out.join("\n"));
    }
    if !index.is_empty() {
        // Two namespaces share this tier, fetched by two different tools — one header
        // naming one command left the other namespace unfetchable by it (sp-cvpp0).
        let namespaces = [
            (
                "law-",
                format!(
                    "## Statutes in force — full text on request\n\nThese are law and bind you exactly as the text above does. The slug states\nthe rule; read the reasoning and the scar behind any of them with:\n\n    {harness}/rule.sh show <slug-without-law-prefix>\n"
                ),
            ),
            (
                "sop-",
                "## Runbooks on the shelf — full text on request\n\nThese bind exactly as the statutes above do. Read the full runbook —\nCHECK, FIX, ESCALATE — with:\n\n    sop show <slug-without-sop-prefix>\n"
                    .to_string(),
            ),
        ];
        for (ns, header) in namespaces {
            let group: Vec<&str> = index.iter().copied().filter(|k| k.starts_with(ns)).collect();
            if !group.is_empty() {
                parts.push(header + &group.join("\n"));
            }
        }
    }
    parts.join("\n\n")
}

/// Split a rendered persona prompt on `<!-- task -->` into `(system_file_contents,
/// task_file_contents)`. `main.rs` writes these verbatim — no trailing newline added beyond
/// what is already in the format string, matching the bash's two `printf '%s'`s.
pub fn split(statutes: &str, prompt: &str) -> (String, String) {
    let (sys, task) = match prompt.split_once("<!-- task -->") {
        Some((s, t)) => (s.to_string(), t.strip_prefix('\n').unwrap_or(t).to_string()),
        None => (String::new(), prompt.to_string()),
    };
    (format!("# Memories in force\n\n{statutes}\n\n---\n\n{sys}"), task)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIX3: &str = r#"{"law-rm-alpha":"Alpha statute body. This is the full text of alpha.","law-rm-beta":"Beta statute body. This is the full text of beta.","law-rm-gamma":"Gamma statute body. This is the full text of gamma."}"#;

    #[test]
    fn core_slug_renders_in_full_others_as_index_slugs() {
        let out = render(FIX3, "law-rm-", 120_000, "law-rm-alpha", "/h");
        assert!(out.contains("## law-rm-alpha"));
        assert!(out.contains("Alpha statute body"));
        assert!(out.contains("law-rm-beta"));
        assert!(!out.contains("Beta statute body"));
    }

    #[test]
    fn budget_fallback_demotes_to_index_rather_than_vanishing() {
        let out = render(FIX3, "law-rm-", 40, "law-rm-alpha,law-rm-beta", "/h");
        assert!(out.contains("law-rm-alpha"));
        assert!(!out.contains("Alpha statute body"));
    }

    #[test]
    fn mixed_namespaces_each_get_their_own_retrieval_command() {
        let fix = r#"{"law-rm-alpha":"Alpha statute body.","sop-rm-widget":"Widget runbook body."}"#;
        let out = render(fix, "law-rm-,sop-rm-", 0, "", "/h");
        assert!(out.contains("sop-rm-widget") && out.contains("sop show"));
        assert!(out.contains("law-rm-alpha") && out.contains("rule.sh show"));
        let law_section = out.split("## Runbooks").next().unwrap_or("");
        assert!(!law_section.contains("sop-rm-widget"));
    }

    #[test]
    fn garbage_json_renders_empty() {
        assert_eq!(render("garbage", "law-", 1, "", "/h"), "");
    }

    #[test]
    fn split_separates_on_the_task_marker() {
        let (sys, task) = split("STATUTES", "system part\n<!-- task -->\ntask part");
        assert_eq!(sys, "# Memories in force\n\nSTATUTES\n\n---\n\nsystem part\n");
        assert_eq!(task, "task part");
    }

    #[test]
    fn split_with_no_marker_is_entirely_the_task() {
        let (sys, task) = split("", "just a task, no split");
        assert_eq!(sys, "# Memories in force\n\n\n\n---\n\n");
        assert_eq!(task, "just a task, no split");
    }
}
