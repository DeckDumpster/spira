//! `sync-claim-inputs <id>...`: writes each record's claim inputs (blocking edges, parent,
//! labels) from the content store, the whole set each time, so a repeat converges.

use spira_config::edges;

use crate::callers::{Answer, Bd, Machine, CANNOT_TELL, NO_ROW};
use crate::content_sql::Edge;

pub const VERB: &str = "sync-claim-inputs";

/// What the content store holds of one bead that the claim rule reads.
pub struct ClaimRead {
    pub parent: Option<String>,
    pub labels: Vec<String>,
    pub edges: Vec<Edge>,
}

/// The ids `rank::declared_blockers` yields for a bead: its own blocking edges, parent excluded.
pub fn blocks(id: &str, read: &ClaimRead) -> Vec<String> {
    read.edges
        .iter()
        .filter(|e| edges::blocks(edges::Edge::Dep(&e.kind)))
        .filter(|e| e.issue_id == id)
        .map(|e| e.depends_on_id.clone())
        .filter(|b| read.parent.as_deref() != Some(b.as_str()))
        .collect()
}

pub const ONLY_IF_UNSET: &str = "--only-if-unset";

pub fn run(args: &[String], m: &mut dyn Machine, bd: &mut dyn Bd) -> Answer {
    let only_if_unset = args.iter().any(|a| a == ONLY_IF_UNSET);
    let ids: Vec<&String> = args.iter().filter(|a| *a != ONLY_IF_UNSET).collect();
    if ids.is_empty() || ids.iter().any(|a| a.starts_with('-')) {
        return Answer { code: CANNOT_TELL, stderr: format!("usage: spira-lc {VERB} [{ONLY_IF_UNSET}] <bead-id>...\n"), ..Default::default() };
    }
    let (mut worst, mut said, mut kept) = (0, String::new(), String::new());
    for id in ids {
        let id: &String = id;
        let (code, why) = match bd.claim_read(id) {
            Err(e) => (CANNOT_TELL, format!("{id}: the content store cannot be read: {}", e.trim())),
            Ok(None) => (NO_ROW, format!("{id}: no such bead in the content store")),
            Ok(Some(read)) => {
                let mut argv = vec!["set-claim-inputs".to_string(), id.clone()];
                argv.extend(blocks(id, &read).into_iter().flat_map(|b| ["--block".to_string(), b]));
                argv.extend(read.parent.iter().flat_map(|p| ["--parent".to_string(), p.clone()]));
                argv.extend(read.labels.iter().flat_map(|l| ["--label".to_string(), l.clone()]));
                if only_if_unset {
                    argv.push(ONLY_IF_UNSET.to_string());
                }
                let (code, out) = m.call(&argv);
                if code == 0 && out.trim().starts_with("kept ") {
                    kept.push_str(&format!("kept {id}: already written\n"));
                }
                (code, if code == 0 { String::new() } else { format!("{id}: {}", out.trim()) })
            }
        };
        if code != 0 {
            worst = worst.max(code);
            said.push_str(&format!("{why}\n"));
        }
    }
    Answer { code: worst, stdout: kept, stderr: said, ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(issue: &str, to: &str, kind: &str) -> Edge {
        Edge { issue_id: issue.into(), depends_on_id: to.into(), kind: kind.into() }
    }

    #[test_id(tfzd4jw)]
    #[test]
    fn only_the_beads_own_blocking_edges_other_than_its_parent_are_blocks() {
        let read = ClaimRead {
            parent: Some("sp-e".into()),
            labels: vec![],
            edges: vec![
                edge("sp-a", "sp-e", "parent-child"),
                edge("sp-a", "sp-e", "blocks"),
                edge("sp-a", "sp-b", "blocks"),
                edge("sp-a", "sp-c", "blocked-by"),
                edge("sp-a", "sp-r", "related"),
                edge("sp-x", "sp-y", "blocks"),
            ],
        };
        assert_eq!(blocks("sp-a", &read), ["sp-b", "sp-c"]);
    }
}
