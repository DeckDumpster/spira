//! The `bead` tool's reads, all through the content door (`spira-lc content`).

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use spira_config::lc_call::LcClient;
use spira_config::lc_state::Row as LifecycleRow;

use crate::{branch_candidate, branch_label, parse_list_ids, parse_show_row, IdeaRow, LintSource, ShowRow};

const CHUNK: usize = 500;

/// `content <args>`: the exit code and stdout, as a forked `bdq` read answered.
pub fn read(client: &LcClient, args: &[String]) -> (i32, String) {
    let argv: Vec<&str> = std::iter::once("content").chain(args.iter().map(String::as_str)).collect();
    match client.call(&argv) {
        Ok(r) => (r.code, r.out),
        Err(_) => (127, String::new()),
    }
}

/// The open ideas under `label`, newest first; `Err` carries the failing exit code.
pub fn ideas(client: &LcClient, label: &str) -> Result<Vec<IdeaRow>, i32> {
    let mut args = vec!["list".to_string()];
    args.extend(spira_config::nonwork::status_args(spira_config::nonwork::Kind::Idea, spira_config::nonwork::Which::Open));
    args.extend(["--label", label, "--limit", "0", "--json"].map(String::from));
    let (code, out) = read(client, &args);
    if code != 0 {
        return Err(code);
    }
    Ok(crate::idea_rows(&out))
}

pub type LifecycleRows = Result<HashMap<String, LifecycleRow>, String>;

/// Which beads `lint` judges.
pub enum Set {
    All,
    Ids(Vec<String>),
}

/// A lint source answering from rows read up front: the set, then the beads it points at.
pub struct DoorSource<'a> {
    ids: Vec<String>,
    rows: HashMap<String, Value>,
    lifecycle: Box<dyn Fn() -> LifecycleRows + 'a>,
}

fn list_by_id(client: &LcClient, ids: &[String]) -> Vec<Value> {
    ids.chunks(CHUNK)
        .flat_map(|chunk| {
            let args = ["list", "--all", "--limit", "0", "--json", "--id", &chunk.join(",")].map(String::from);
            match serde_json::from_str::<Value>(&read(client, &args).1) {
                Ok(Value::Array(rows)) => rows,
                _ => Vec::new(),
            }
        })
        .collect()
}

fn targets<'v>(row: &'v Value, kinds: &'v [&str]) -> impl Iterator<Item = &'v str> {
    row["dependencies"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(move |d| d["type"].as_str().is_some_and(|t| kinds.contains(&t)))
        .filter_map(|d| d["depends_on_id"].as_str())
        .filter(|t| !t.is_empty())
}

impl<'a> DoorSource<'a> {
    pub fn load(client: &LcClient, set: Set, lifecycle: impl Fn() -> LifecycleRows + 'a) -> Self {
        let (mut ids, listed) = match set {
            Set::All => {
                let (_, out) = read(client, &["list", "--limit", "0", "--json"].map(String::from));
                let rows = serde_json::from_str::<Value>(&out).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
                (parse_list_ids(&out), rows)
            }
            Set::Ids(ids) => (ids.clone(), list_by_id(client, &ids)),
        };
        ids.retain(|i| !i.is_empty());
        let mut rows: HashMap<String, Value> = listed.into_iter().filter_map(|r| Some((r["id"].as_str()?.to_string(), r))).collect();

        let mut wanted: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for id in &ids {
            let Some(row) = rows.get(id) else { continue };
            let branch = parse_show_row(&row.to_string()).and_then(|r| branch_label(&r.labels).map(|b| branch_candidate(b).to_string()));
            for t in targets(row, &["blocks", "relates-to"]).chain(branch.as_deref().filter(|c| !c.is_empty())) {
                if !rows.contains_key(t) && seen.insert(t.to_string()) {
                    wanted.push(t.to_string());
                }
            }
        }
        for r in list_by_id(client, &wanted) {
            if let Some(id) = r["id"].as_str().map(String::from) {
                rows.insert(id, r);
            }
        }
        DoorSource { ids, rows, lifecycle: Box::new(lifecycle) }
    }

    /// A source that only answers `lifecycle`.
    pub fn lifecycle_only(lifecycle: impl Fn() -> LifecycleRows + 'a) -> Self {
        DoorSource { ids: Vec::new(), rows: HashMap::new(), lifecycle: Box::new(lifecycle) }
    }
}

impl LintSource for DoorSource<'_> {
    fn ids(&self) -> Vec<String> {
        self.ids.clone()
    }
    fn show(&self, id: &str) -> Option<ShowRow> {
        parse_show_row(&self.rows.get(id)?.to_string())
    }
    fn blocks_targets(&self, id: &str) -> Option<Vec<String>> {
        Some(targets(self.rows.get(id)?, &["blocks"]).map(String::from).collect())
    }
    fn relates_targets(&self, id: &str) -> Result<Vec<String>, String> {
        let row = self.rows.get(id).ok_or(" failed")?;
        Ok(targets(row, &["relates-to"]).map(String::from).collect())
    }
    fn lifecycle(&self) -> LifecycleRows {
        (self.lifecycle)()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lint, LintCfg};
    use serde_json::json;
    use spira_config::lc_call::fake::Content;
    use std::sync::Arc;

    fn bead(id: &str, labels: &[&str], blocks: &[&str]) -> Value {
        json!({
            "id": id, "status": "open", "issue_type": "task", "description": "", "labels": labels.iter().copied().chain(["repo:r"]).collect::<Vec<_>>(),
            "dependencies": blocks.iter().map(|b| json!({"issue_id": id, "depends_on_id": b, "type": "blocks"})).collect::<Vec<_>>(),
        })
    }

    // literal-ok: test fixture label vocabulary, not the configured names
    fn cfg() -> LintCfg {
        LintCfg { partitions: vec![], spec_partitions: vec![], no_loop_label: "no-loop".into(), ask_label: "needs-op-test".into(), incident_label: "incident-test".into() }
    }

    fn fixture() -> Arc<Content> {
        Arc::new(Content::new(vec![
            bead("sp-a", &["needs-op-test"], &["sp-d"]),
            bead("sp-b", &[], &[]),
            bead("sp-c", &[], &[]),
            bead("sp-d", &["needs-op-test"], &[]),
        ]))
    }

    #[test]
    fn lint_reads_three_beads_and_their_blocker_in_two_requests() {
        let fake = fixture();
        let client = LcClient::over(fake.clone(), true);
        let ids = ["sp-a", "sp-b", "sp-c"].map(String::from).to_vec();
        let src = DoorSource::load(&client, Set::Ids(ids), || Ok(HashMap::new()));
        let (code, lines) = lint(&src, &cfg());
        assert_eq!(code, 1);
        assert_eq!(lines, vec!["bead: sp-a: blocks edge to ask-labelled sp-d (asks must not block each other; use bd dep relate)".to_string()]);
        let seen = fake.requests();
        assert!(seen.len() <= 2, "{seen:?}");
        assert!(seen.iter().all(|r| r[..2] == ["content", "list"] && r.contains(&"--id".to_string())), "{seen:?}");
    }

    #[test]
    fn a_bead_the_store_did_not_return_is_unreadable() {
        let client = LcClient::over(fixture(), true);
        let src = DoorSource::load(&client, Set::Ids(vec!["sp-gone".into()]), || Ok(HashMap::new()));
        let (code, lines) = lint(&src, &cfg());
        assert_eq!(code, 1);
        assert_eq!(lines, vec!["bead: sp-gone: unreadable (bd show failed)".to_string()]);
    }

    #[test]
    fn lint_all_reads_the_set_once_and_the_absent_blocker_by_id() {
        let fake = Arc::new(Content::new(vec![
            bead("sp-a", &["needs-op-test"], &["sp-d"]),
            json!({"id": "sp-d", "status": "closed", "issue_type": "task", "labels": ["needs-op-test", "repo:r"], "dependencies": []}),
        ]));
        let client = LcClient::over(fake.clone(), true);
        let src = DoorSource::load(&client, Set::All, || Ok(HashMap::new()));
        let (_, lines) = lint(&src, &cfg());
        assert_eq!(lines.len(), 1, "{lines:?}");
        let seen = fake.requests();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert!(!seen[0].contains(&"--id".to_string()) && seen[1].contains(&"--id".to_string()), "{seen:?}");
    }

    #[test]
    fn ideas_read_open_rows_under_the_label_with_no_limit() {
        let fake = Arc::new(Content::new(vec![]));
        let rows = ideas(&LcClient::over(fake.clone(), true), "idea-x").unwrap();
        assert!(rows.is_empty());
        let want: Vec<String> = ["content", "list", "--status", "open", "--label", "idea-x", "--limit", "0", "--json"].map(String::from).to_vec();
        assert_eq!(fake.requests(), vec![want]);
    }
}
