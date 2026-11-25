//! The `bead` tool's reads, all through the content door (`spira-lc content`).

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use spira_store::lc_call::LcClient;
use spira_store::lc_state::Row as LifecycleRow;

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

fn refused(r: spira_store::lc_call::Reply) -> String {
    format!("exited {}: {}", r.code, if r.err.trim().is_empty() { r.out.trim() } else { r.err.trim() })
}

fn write(client: &LcClient, argv: &[&str], body: Option<&str>) -> Result<String, String> {
    let sent = match body {
        Some(b) => client.call_stdin(argv, b),
        None => client.call(argv),
    };
    match sent {
        Ok(r) if r.code == 0 => Ok(r.out),
        Ok(r) => Err(refused(r)),
        Err(e) => Err(e.to_string()),
    }
}

/// `bead.sh file --for <persona> --repo <repo>` through the door: the new bead's id. The door
/// applies the filing contract and writes the bead's lifecycle row.
pub fn file_work(client: &LcClient, persona: &str, repo: &str, title: &str, body: &str) -> Result<String, String> {
    file_under(client, &crate::filing::work_labels(&spira_config::process::spira_home(), persona, repo)?, persona, repo, title, body)
}

fn file_under(client: &LcClient, labels: &str, persona: &str, repo: &str, title: &str, body: &str) -> Result<String, String> {
    let out = write(client, &["content", "create", title, "--labels", labels, "--body-file", "-", "--silent"], Some(body))?;
    let id = out.lines().rev().map(str::trim).find(|l| !l.is_empty()).ok_or("the door printed no id")?.to_string();
    spira_otel::oneshot::event("spira_beads_filed", 1, &[("persona", persona), ("repo", repo), ("kind", "work")]);
    Ok(id)
}

/// `bead.sh label add <id> <label>` through the door.
pub fn label_add(client: &LcClient, id: &str, label: &str) -> Result<(), String> {
    write(client, &["content", "label", "add", id, label], None).map(|_| ())
}

/// What `bead.sh amend` does to a description.
pub enum Change<'a> {
    Replace(&'a str),
    Append(&'a str),
}

/// `bead.sh amend <id>` through the door: the description changed, then `note` recorded. A bead a
/// builder holds keeps its description (the door refuses an unacknowledged edit): the note is
/// recorded alone, with an appended text added to it, so the holder reads the change.
pub fn amend(client: &LcClient, id: &str, change: Change, note: &str) -> Result<(), String> {
    let (new, added) = match change {
        Change::Replace(b) => (b.to_string(), None),
        Change::Append(b) => {
            let shown = write(client, &["content", "show", id, "--json"], None)?;
            let old = crate::parse_description(&shown).ok_or_else(|| format!("cannot read the current description of {id}"))?;
            (crate::appended_body(&old, b, &spira_otel::utc_date(std::time::SystemTime::now())), Some(b))
        }
    };
    let mut note = note.to_string();
    if let Err(e) = write(client, &["content", "update", id, "--body-file", "-"], Some(&new)) {
        if !crate::claimdesc::is_refusal(&e) {
            return Err(e);
        }
        if let Some(b) = added {
            note = format!("{note}\n\n{b}");
        }
    }
    write(client, &["content", "note", id, "--stdin"], Some(&note)).map(|_| ())
}

/// Every bead-tool fork in the Rust under `dir`, as `file:line: text`.
pub fn forks_a_bead_tool_under(dir: &std::path::Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(d) = pending.pop() {
        for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let source = std::fs::read_to_string(&path).unwrap_or_default();
                found.extend(forks_a_bead_tool(&source).into_iter().map(|l| format!("{}:{l}", path.display())));
            }
        }
    }
    found
}

/// The lines of `source` (a crate's Rust) that name a bead tool to run: `bead.sh`, `bdq`, or a
/// spawn of `bead`. Comments are not code.
pub fn forks_a_bead_tool(source: &str) -> Vec<String> {
    let spawners = ["Command::new(", "bounded(", "bounded_for("];
    source
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            let code = l.split("//").next().unwrap_or("");
            code.contains("\"bead.sh\"") || code.contains("\"bdq\"") || spawners.iter().any(|s| code.contains(&format!("{s}\"bead\"")))
        })
        .map(|(n, l)| format!("{}: {}", n + 1, l.trim()))
        .collect()
}

/// The label a parked idea carries; `schema.sh name idea` declares the same name.
pub const IDEA_LABEL: &str = "idea";

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

/// The lifecycle rows of the named beads.
type LifecycleOf<'a> = dyn Fn(&[String]) -> LifecycleRows + 'a;

/// Which beads `lint` judges.
pub enum Set {
    All,
    Ids(Vec<String>),
}

/// A lint source answering from rows read up front: the set, then the beads it points at.
pub struct DoorSource<'a> {
    ids: Vec<String>,
    rows: HashMap<String, Value>,
    lifecycle: Box<LifecycleOf<'a>>,
}

fn rows_of(client: &LcClient, args: &[String]) -> Result<Vec<Value>, String> {
    let (code, out) = read(client, args);
    if code != 0 {
        return Err(format!("content {} failed (exit {code})", args.first().map(String::as_str).unwrap_or("")));
    }
    match serde_json::from_str::<Value>(&out) {
        Ok(Value::Array(rows)) => Ok(rows),
        _ => Err("content list answered no rows array".to_string()),
    }
}

fn list_by_id(client: &LcClient, ids: &[String]) -> Result<Vec<Value>, String> {
    let mut all = Vec::new();
    for chunk in ids.chunks(CHUNK) {
        let args = ["list", "--all", "--limit", "0", "--json", "--id", &chunk.join(",")].map(String::from);
        all.extend(rows_of(client, &args)?);
    }
    Ok(all)
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
    pub fn load(client: &LcClient, set: Set, lifecycle: impl Fn(&[String]) -> LifecycleRows + 'a) -> Result<Self, String> {
        let (mut ids, listed) = match set {
            Set::All => {
                let rows = rows_of(client, &["list", "--limit", "0", "--json"].map(String::from))?;
                (parse_list_ids(&Value::Array(rows.clone()).to_string()), rows)
            }
            Set::Ids(ids) => (ids.clone(), list_by_id(client, &ids)?),
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
        for r in list_by_id(client, &wanted)? {
            if let Some(id) = r["id"].as_str().map(String::from) {
                rows.insert(id, r);
            }
        }
        Ok(DoorSource { ids, rows, lifecycle: Box::new(lifecycle) })
    }

    /// A source that only answers `lifecycle`.
    pub fn lifecycle_only(ids: Vec<String>, lifecycle: impl Fn(&[String]) -> LifecycleRows + 'a) -> Self {
        DoorSource { ids, rows: HashMap::new(), lifecycle: Box::new(lifecycle) }
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
        let mut named: Vec<String> = self.ids.iter().chain(self.rows.keys()).cloned().collect();
        named.sort();
        named.dedup();
        (self.lifecycle)(&named)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lint, LintCfg};
    use serde_json::json;
    use spira_store::lc_call::fake::Content;
    use std::sync::Arc;

    fn bead(id: &str, labels: &[&str], blocks: &[&str]) -> Value {
        json!({
            "id": id, "status": "open", "issue_type": "task", "description": "", "labels": labels.iter().copied().chain(["repo:r"]).collect::<Vec<_>>(),
            "dependencies": blocks.iter().map(|b| json!({"issue_id": id, "depends_on_id": b, "type": "blocks"})).collect::<Vec<_>>(),
        })
    }

    fn cfg() -> LintCfg {
        // literal-ok: test fixture label vocabulary, not the configured names
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

    #[test_id(tgal57l)]
    #[test]
    fn lint_reads_three_beads_and_their_blocker_in_two_requests() {
        let fake = fixture();
        let client = LcClient::over(fake.clone(), true);
        let ids = ["sp-a", "sp-b", "sp-c"].map(String::from).to_vec();
        let src = DoorSource::load(&client, Set::Ids(ids), |_| Ok(HashMap::new())).unwrap();
        let (code, lines) = lint(&src, &cfg());
        assert_eq!(code, 1);
        assert_eq!(lines, vec!["bead: sp-a: blocks edge to ask-labelled sp-d (asks must not block each other; use bd dep relate)".to_string()]);
        let seen = fake.requests();
        assert!(seen.len() <= 2, "{seen:?}");
        assert!(seen.iter().all(|r| r[..2] == ["content", "list"] && r.contains(&"--id".to_string())), "{seen:?}");
    }

    #[test_id(te93l4g)]
    #[test]
    fn the_lifecycle_read_is_of_the_ids_the_door_holds_never_the_whole_table() {
        let client = LcClient::over(fixture(), true);
        let asked = std::cell::RefCell::new(Vec::<Vec<String>>::new());
        let ids = ["sp-a", "sp-b"].map(String::from).to_vec();
        let src = DoorSource::load(&client, Set::Ids(ids), |ids| {
            asked.borrow_mut().push(ids.to_vec());
            Ok(HashMap::new())
        })
        .unwrap();
        src.lifecycle().unwrap();
        assert_eq!(*asked.borrow(), [["sp-a", "sp-b", "sp-d"].map(String::from).to_vec()]);
    }

    #[test_id(tvebezg)]
    #[test]
    fn a_bead_the_store_did_not_return_is_unreadable() {
        let client = LcClient::over(fixture(), true);
        let src = DoorSource::load(&client, Set::Ids(vec!["sp-gone".into()]), |_| Ok(HashMap::new())).unwrap();
        let (code, lines) = lint(&src, &cfg());
        assert_eq!(code, 1);
        assert_eq!(lines, vec!["bead: sp-gone: unreadable (bd show failed)".to_string()]);
    }

    #[test_id(tw0n24a)]
    #[test]
    fn lint_all_reads_the_set_once_and_the_absent_blocker_by_id() {
        let fake = Arc::new(Content::new(vec![
            bead("sp-a", &["needs-op-test"], &["sp-d"]),
            json!({"id": "sp-d", "status": "closed", "issue_type": "task", "labels": ["needs-op-test", "repo:r"], "dependencies": []}),
        ]));
        let client = LcClient::over(fake.clone(), true);
        let src = DoorSource::load(&client, Set::All, |_| Ok(HashMap::new())).unwrap();
        let (_, lines) = lint(&src, &cfg());
        assert_eq!(lines.len(), 1, "{lines:?}");
        let seen = fake.requests();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert!(!seen[0].contains(&"--id".to_string()) && seen[1].contains(&"--id".to_string()), "{seen:?}");
    }

    struct Down;
    impl spira_store::lc_call::Transport for Down {
        fn exchange(&self, _: &[String], _: Option<&str>, _: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
            Err("socket down".into())
        }
    }

    #[test_id(tq4ay2u)]
    #[test]
    fn a_door_that_cannot_be_reached_fails_the_load_rather_than_linting_nothing() {
        let client = LcClient::over(Arc::new(Down), true);
        assert!(DoorSource::load(&client, Set::All, |_| Ok(HashMap::new())).is_err());
        assert!(DoorSource::load(&client, Set::Ids(vec!["sp-a".into()]), |_| Ok(HashMap::new())).is_err());
    }

    #[test_id(tj4n8cz)]
    #[test]
    fn the_idea_label_is_the_name_the_schema_declares() {
        let schema = include_str!("../../spira/schema.sh");
        let declared = schema.lines().find(|l| l.trim_start().starts_with("idea)") && l.contains("printf")).expect("schema.sh declares an idea name");
        assert!(declared.contains(&format!("'{IDEA_LABEL}'")), "{declared}");
    }

    #[test_id(tmkdto9)]
    #[test]
    fn ideas_read_open_rows_under_the_label_with_no_limit() {
        let fake = Arc::new(Content::new(vec![]));
        let rows = ideas(&LcClient::over(fake.clone(), true), "idea-x").unwrap();
        assert!(rows.is_empty());
        let want: Vec<String> = ["content", "list", "--status", "open", "--label", "idea-x", "--limit", "0", "--json"].map(String::from).to_vec();
        assert_eq!(fake.requests(), vec![want]);
    }

    struct Door(std::sync::Mutex<Vec<(Vec<String>, Option<String>)>>, Vec<(&'static str, i32, &'static str)>);

    impl spira_store::lc_call::Transport for Door {
        fn exchange(&self, argv: &[String], stdin: Option<&str>, _: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
            self.0.lock().unwrap().push((argv.to_vec(), stdin.map(str::to_string)));
            let (_, code, text) = self.1.iter().find(|(v, _, _)| argv[1] == *v).copied().unwrap_or(("", 0, ""));
            Ok(spira_store::lc_call::Reply { code, out: if code == 0 { text.into() } else { String::new() }, err: if code == 0 { String::new() } else { text.into() }, read_ended: false })
        }
    }

    fn door(answers: Vec<(&'static str, i32, &'static str)>) -> (Arc<Door>, LcClient) {
        let d = Arc::new(Door(Default::default(), answers));
        (d.clone(), LcClient::over(d, true))
    }

    #[test_id(t50vpo3)]
    #[test]
    fn a_work_filing_reaches_the_door_as_one_create_with_the_body_on_stdin_and_its_id_is_the_last_line() {
        let (d, client) = door(vec![("create", 0, "noise\nsp-new\n")]);
        assert_eq!(file_under(&client, "lane-x,repo:deck", "builder", "deck", "T", "the body"), Ok("sp-new".to_string()));
        let sent = d.0.lock().unwrap().clone();
        let want: Vec<String> = ["content", "create", "T", "--labels", "lane-x,repo:deck", "--body-file", "-", "--silent"].map(String::from).to_vec();
        assert_eq!(sent, vec![(want, Some("the body".to_string()))]);
    }

    #[test_id(t7hi4jr)]
    #[test]
    fn a_filing_the_door_refuses_is_an_error_carrying_its_text() {
        let (_, client) = door(vec![("create", 2, "bead: refusing: no build spec\n")]);
        let e = file_under(&client, "l", "builder", "deck", "T", "b").unwrap_err();
        assert_eq!(e, "exited 2: bead: refusing: no build spec");
    }

    #[test_id(tm3pr6l)]
    #[test]
    fn an_amend_replaces_the_description_then_notes_it() {
        let (d, client) = door(vec![]);
        amend(&client, "sp-a", Change::Replace("new"), "moved").unwrap();
        let sent = d.0.lock().unwrap().clone();
        assert_eq!(sent[0], (["content", "update", "sp-a", "--body-file", "-"].map(String::from).to_vec(), Some("new".to_string())));
        assert_eq!(sent[1], (["content", "note", "sp-a", "--stdin"].map(String::from).to_vec(), Some("moved".to_string())));
    }

    #[test_id(tmclp98)]
    #[test]
    fn an_append_keeps_the_old_description_under_a_dated_separator() {
        let (d, client) = door(vec![("show", 0, r#"[{"description":"old"}]"#)]);
        amend(&client, "sp-a", Change::Append("more"), "again").unwrap();
        let sent = d.0.lock().unwrap().clone();
        let body = sent[1].1.clone().unwrap();
        assert!(body.starts_with("old\n\n--- appended ") && body.ends_with("---\n\nmore\n"), "{body}");
    }

    #[test_id(t13hwvo)]
    #[test]
    fn a_held_beads_amend_is_noted_alone_with_the_appended_text_and_a_refused_one_writes_nothing() {
        let claimed = "bead: sp-a is claimed by dwarf-sp-a (lease expires x) — editing. Pass --force-claimed \"<reason>\" to override.\n";
        let (d, client) = door(vec![("update", 1, claimed)]);
        amend(&client, "sp-a", Change::Replace("new"), "moved").unwrap();
        assert_eq!(d.0.lock().unwrap().last().unwrap().1.as_deref(), Some("moved"), "a replacement the holder is told of by the note");
        let (d, client) = door(vec![("show", 0, r#"[{"description":"old"}]"#), ("update", 1, claimed)]);
        amend(&client, "sp-a", Change::Append("more"), "again").unwrap();
        assert_eq!(d.0.lock().unwrap().last().unwrap().1.as_deref(), Some("again\n\nmore"));
        let (d, client) = door(vec![("update", 2, "bead: refusing: the text contradicts a statute\n")]);
        assert!(amend(&client, "sp-a", Change::Replace("new"), "moved").unwrap_err().contains("contradicts a statute"));
        assert_eq!(d.0.lock().unwrap().len(), 1, "no note follows a refused body");
    }

    #[test_id(tsxzgxi)]
    #[test]
    fn a_planted_bead_fork_is_found_and_the_door_filing_is_not() {
        let planted = ["let mut c = bounded_for(\"bead.sh\", \"120\");", "Command::new(\"bead\")", "let b = bounded_for(&self.bdq_path, \"1\"); x(\"bdq\")"];
        for p in planted {
            assert_eq!(forks_a_bead_tool(p).len(), 1, "{p}");
        }
        let door = "let id = bead::door::file_work(&lc, p, r, t, b)?; // was bead.sh\nlet s = doc[\"bead\"][\"state\"];";
        assert!(forks_a_bead_tool(door).is_empty());
    }
}
