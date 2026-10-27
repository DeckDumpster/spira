//! The evidence pack: input to the mender, never a verdict.

use crate::signature::Hit;
use suite_select::glob::case_match;

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct MemberSpec {
    pub bead: String,
    pub tip: String,
}

/// The failure as the summons states it. Every field is a fact about the pass; nothing in it
/// is the mender's to set.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Spec {
    pub batch: String,
    pub pass: u32,
    pub suite: String,
    pub repo: String,
    pub base: String,
    /// The suite output text itself, never a path.
    pub output: String,
    /// The tree the pass already built, where the mend's checks run.
    #[serde(default)]
    pub tree: Option<String>,
    pub members: Vec<MemberSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub bead: String,
    pub tip: String,
    pub files: Vec<String>,
    pub diff: String,
}

/// The members whose diff touches a file the suite covers. A suite that declares no covers
/// covers everything (`None`), so every member touches it.
pub fn touching<'a>(members: &'a [Member], covers: Option<&[String]>) -> Vec<&'a Member> {
    members
        .iter()
        .filter(|m| match covers {
            None => true,
            Some(globs) => m.files.iter().any(|f| globs.iter().any(|g| case_match(g, f))),
        })
        .collect()
}

pub struct Pack<'a> {
    pub spec: &'a Spec,
    pub output: &'a str,
    pub hits: &'a [Hit],
    pub covers: Option<&'a [String]>,
    pub members: &'a [Member],
    pub diff_cap: usize,
}

pub fn tail(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let from = lines.len().saturating_sub(max_lines);
    lines[from..].join("\n")
}

fn cut(diff: &str, cap: usize, bead: &str) -> String {
    if diff.len() <= cap {
        return diff.to_string();
    }
    let mut end = cap;
    while !diff.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n... cut at {cap} bytes; the rest: mender diff <spec> {bead}", &diff[..end])
}

impl Pack<'_> {
    pub fn render(&self) -> String {
        let s = self.spec;
        let touch = touching(self.members, self.covers);
        let mut out = format!("# Evidence pack — {} pass {} suite {}\n\n", s.batch, s.pass, s.suite);
        if let Some(t) = &s.tree {
            out.push_str(&format!("Pass tree (run the mend's checks here): {t}\n\n"));
        }
        out.push_str("Input, not a verdict: disagree with any of it.\n\n## Suite output (tail)\n\n```\n");
        out.push_str(&tail(self.output, 120));
        out.push_str("\n```\n\n## Environment signatures\n\n");
        if self.hits.is_empty() {
            out.push_str("none found in the output\n");
        }
        for h in self.hits {
            out.push_str(&format!("- {} at output line {}: {}\n", h.name, h.line, h.text));
        }
        out.push_str("\n## Members whose diff touches what the suite exercises\n\n");
        match self.covers {
            None => out.push_str("the suite declares no `# covers:`, so it exercises everything: every member is listed\n"),
            Some(g) => out.push_str(&format!("suite covers: {}\n", g.join(" "))),
        }
        for m in &touch {
            let hit: Vec<&str> = m.files.iter().filter(|f| self.covers.is_none_or(|g| g.iter().any(|p| case_match(p, f)))).map(String::as_str).collect();
            out.push_str(&format!("- {} @ {}: {}\n", m.bead, m.tip, hit.join(", ")));
        }
        if touch.is_empty() {
            out.push_str("- none: no member's diff touches a covered file (an interaction, the environment, or a gap in `# covers:`)\n");
        }
        out.push_str("\n## Member diffs\n");
        for m in self.members {
            out.push_str(&format!("\n### {} @ {} ({} files)\n\n```diff\n{}\n```\n", m.bead, m.tip, m.files.len(), cut(&m.diff, self.diff_cap, &m.bead)));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(bead: &str, files: &[&str]) -> Member {
        Member { bead: bead.into(), tip: "abc".into(), files: files.iter().map(|f| f.to_string()).collect(), diff: format!("diff of {bead}") }
    }

    fn spec() -> Spec {
        Spec { batch: "r-1".into(), pass: 2, suite: "test-x.sh".into(), repo: "/r".into(), base: "main".into(), output: "/o".into(), tree: None, members: vec![] }
    }

    #[test]
    fn only_the_member_touching_a_covered_file_is_pointed_at() {
        let ms = [member("sp-a", &["spira/lib.sh"]), member("sp-b", &["docs/x.md"])];
        let covers = vec!["spira/*.sh".to_string()];
        assert_eq!(touching(&ms, Some(&covers)).iter().map(|m| m.bead.as_str()).collect::<Vec<_>>(), vec!["sp-a"]);
    }

    #[test]
    fn a_suite_with_no_covers_points_at_every_member() {
        let ms = [member("sp-a", &["x"]), member("sp-b", &["y"])];
        assert_eq!(touching(&ms, None).len(), 2);
    }

    #[test]
    fn the_pack_names_signatures_touching_members_and_every_diff() {
        let ms = [member("sp-a", &["spira/lib.sh"]), member("sp-b", &["docs/x.md"])];
        let covers = vec!["spira/*.sh".to_string()];
        let hits = crate::signature::scan("a\nfork: retry: Resource temporarily unavailable\n");
        let s = spec();
        let text = Pack { spec: &s, output: "a\nb", hits: &hits, covers: Some(&covers), members: &ms, diff_cap: 1000 }.render();
        assert!(text.contains("EAGAIN at output line 2"), "{text}");
        assert!(text.contains("- sp-a @ abc: spira/lib.sh"), "{text}");
        assert!(!text.contains("- sp-b @ abc:"), "{text}");
        assert!(text.contains("diff of sp-a") && text.contains("diff of sp-b"), "{text}");
    }

    #[test]
    fn nothing_touching_says_so_instead_of_staying_silent() {
        let ms = [member("sp-b", &["docs/x.md"])];
        let covers = vec!["spira/*.sh".to_string()];
        let s = spec();
        let text = Pack { spec: &s, output: "", hits: &[], covers: Some(&covers), members: &ms, diff_cap: 1000 }.render();
        assert!(text.contains("none: no member's diff touches"), "{text}");
        assert!(text.contains("none found in the output"), "{text}");
    }

    #[test]
    fn a_large_diff_is_cut_at_a_char_boundary_and_points_at_the_verb() {
        let mut m = member("sp-a", &["f"]);
        m.diff = "é".repeat(100);
        let cut = cut(&m.diff, 11, "sp-a");
        assert!(cut.starts_with(&"é".repeat(5)) && cut.contains("mender diff <spec> sp-a"), "{cut}");
    }

    #[test]
    fn the_pass_tree_is_named_when_the_spec_has_one() {
        let mut s = spec();
        s.tree = Some("/pass/tree".into());
        let text = Pack { spec: &s, output: "", hits: &[], covers: None, members: &[], diff_cap: 10 }.render();
        assert!(text.contains("Pass tree (run the mend's checks here): /pass/tree"), "{text}");
    }

    #[test]
    fn the_output_is_tailed_to_its_last_lines() {
        assert_eq!(tail("1\n2\n3\n4", 2), "3\n4");
    }
}
