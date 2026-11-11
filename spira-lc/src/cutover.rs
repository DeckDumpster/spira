//! The cutover round's own surface (sp-o7nbr): batch creation, and the three cross-machine
//! cascades — land, settle, abandon — that a batch's own transition emits to every member's
//! delivery and bead row in one transaction (design §3.1.3's "the batch machine emits its
//! members' delivery exits"). `show`/`list`/`history`/`event` (main.rs) are unchanged; this
//! module is additive.
//!
//! Row creation is a constructor, not a transition — `BeadRow::filed`/`BatchRow::cut`/
//! `DeliveryRow::start_queue` already say so in `lifecycle` — so `create-bead` and the batch
//! row inserted by `cut` log no event of their own, exactly as replay never needs one to
//! reach the same initial row. The `Deliver`/`MemberAdded`/`Cut` transitions inside `cut`,
//! and every step inside `land`/`settle`/`abandon-batch`, are real events on real rows and
//! are logged like any other transition.

use lifecycle::{batch, bead};

use crate::db::Conn;
use crate::rows;

const CANNOT_TELL: i32 = 2;
pub(crate) const REFUSED: i32 = 3;

pub fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

pub fn q(s: &str) -> String {
    format!("'{}'", rows::escape(s))
}

/// A lifecycle key: `sp-` then a non-empty tail of alphanumerics, `.`, `-` or `_` — prose and bare prefixes are not.
pub fn is_row_key(x: &str) -> bool {
    x.len() > 3
        && x[..3].eq_ignore_ascii_case("sp-")
        && x[3..].chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        && x.chars().last().is_some_and(|c| c != '-')
}

/// The planning reader `create-bead` asks for a bead's type: `plan items` rows of the ids given.
pub type PlanReader<'a> = dyn FnMut(&[&str]) -> Result<Vec<serde_json::Value>, String> + 'a;

/// `create-bead`'s refusal of a bead whose type has no lifecycle record (an epic, a decision,
/// …), named; `None` lets the create go ahead. The type is the planning plane's, read through
/// `read`; a bead the planning plane cannot answer for is created, so a filing is never stranded
/// rowless by a planning read that failed.
pub fn refuse_no_record_type(args: &[String], read: &mut PlanReader) -> Option<(i32, String)> {
    let id = args.first().filter(|i| is_row_key(i))?;
    let items = read(&[id.as_str()]).ok()?;
    let kind = items.iter().find(|i| i.get("id").and_then(serde_json::Value::as_str) == Some(id.as_str()))?.get("issue_type")?.as_str()?;
    (!spira_config::nonwork::is_work_type(Some(kind))).then(|| {
        (REFUSED, format!("create-bead: {id} is a {kind}, a type with no lifecycle record (its state is the planning plane's) — nothing created"))
    })
}

/// The service never sees this process's stdin, so `batch-progress`'s payload is read here and
/// travels as `--json <body>`.
pub fn inline_stdin_payload(args: Vec<String>) -> Vec<String> {
    inline_payload_from(args, &mut std::io::stdin())
}

pub(crate) fn inline_payload_from(mut args: Vec<String>, input: &mut dyn std::io::Read) -> Vec<String> {
    let verb = (args.first().map(String::as_str), args.get(1).map(String::as_str));
    let reads_stdin = matches!(
        verb,
        (Some("batch-progress" | "claim-verdicts" | "publish-readiness"), _)
            | (Some("section"), Some("apply-spec" | "publish-status"))
            | (Some("intent"), Some("put"))
    );
    if reads_stdin && !args.iter().any(|a| a == "--json") {
        let mut body = String::new();
        let _ = input.read_to_string(&mut body);
        args.push("--json".into());
        args.push(body);
    }
    // `rc <verb> --json -`: the body is stdin, carried inline so the service reads it too.
    if args.first().map(String::as_str) == Some(spira_lc::rc::VERB) {
        if let Some(i) = args.iter().position(|a| a == "--json").map(|i| i + 1).filter(|&i| args.get(i).is_some_and(|b| b == "-")) {
            let mut body = String::new();
            let _ = input.read_to_string(&mut body);
            args[i] = body;
        }
    }
    let ids_on_stdin = args.first().is_some_and(|v| v == "plan") && args.windows(2).any(|w| w[0] == "--ids" && w[1] == "-");
    if ids_on_stdin {
        let mut body = String::new();
        let _ = input.read_to_string(&mut body);
        let ids = body.split(|c: char| c == ',' || c.is_whitespace()).filter(|x| !x.is_empty()).collect::<Vec<_>>().join(",");
        if let Some(i) = args.windows(2).position(|w| w[0] == "--ids" && w[1] == "-") {
            args[i + 1] = ids;
        }
    }
    args
}

/// `(dependent, prerequisite)` for every pair of members where the dependent's branch holds a
/// commit in the prerequisite's own range above the batch's base. A repository, base or tip
/// git cannot resolve yields no edge: the declared `stack` still applies.
pub(crate) fn branch_containment(batch_row: &batch::BatchRow, members: &[(String, String)]) -> std::collections::BTreeSet<(String, String)> {
    let empty = std::collections::BTreeSet::new();
    let Some(base) = batch_row.base.as_deref() else { return empty };
    let Ok(cfg) = crate::repo_config::from_process() else { return empty };
    let Some(section) = cfg.repos.get(&batch_row.repo) else { return empty };
    containment_in_repo(std::path::Path::new(&section.path), base, members)
}

fn containment_in_repo(repo: &std::path::Path, base: &str, members: &[(String, String)]) -> std::collections::BTreeSet<(String, String)> {
    let mut edges = std::collections::BTreeSet::new();
    let ranges: Vec<(&String, std::collections::BTreeSet<String>)> = members
        .iter()
        .filter_map(|(id, tip)| crate::git_evidence::commits_above(repo, base, tip).map(|c| (id, c)))
        .collect();
    for (dependent, theirs) in &ranges {
        for (prereq, ours) in &ranges {
            if dependent != prereq && !ours.is_empty() && ours.len() < theirs.len() && ours.is_subset(theirs) {
                edges.insert(((*dependent).clone(), (*prereq).clone()));
            }
        }
    }
    edges
}

/// The pure frontier walk, split out so the closure is unit-testable without a live `dolt`.
pub(crate) fn stacked_dependents_from(
    member_stacks: &[(String, bead::BeadRow)],
    tips: &std::collections::BTreeMap<String, String>,
    contained: &std::collections::BTreeSet<(String, String)>,
    roots: &[String],
) -> Vec<(String, String)> {
    let mut visited: std::collections::BTreeSet<String> = roots.iter().cloned().collect();
    let mut frontier: Vec<String> = roots.to_vec();
    let mut dependents = Vec::new();
    while let Some(prereq) = frontier.pop() {
        for (id, row) in member_stacks {
            if visited.contains(id) {
                continue;
            }
            let declared = row.stack.get(&prereq).is_some_and(|given_tip| tips.get(&prereq) == Some(given_tip));
            if declared || contained.contains(&(id.clone(), prereq.clone())) {
                visited.insert(id.clone());
                dependents.push((id.clone(), prereq.clone()));
                frontier.push(id.clone());
            }
        }
    }
    dependents
}

/// `event-continuity`: for each (machine, lc_key), every applied event's from_state must equal
/// the previous applied event's to_state. Prints the first break per key as one JSON line.
/// A break listed in `event-continuity.acknowledged` (seq, machine, key, reason; tab-separated,
/// under the run directory) is printed with its reason and passes; any other exits 3.
/// `--acknowledge --through SEQ --reason R` records every unacknowledged break at or below SEQ.
pub fn cmd_event_continuity(args: &[String], conn: &Conn) -> (i32, String) {
    let ack_path = match crate::lifecycle_run_dir() {
        Ok(d) => d.join("event-continuity.acknowledged"),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    let acked = match std::fs::read_to_string(&ack_path) {
        Ok(t) => match crate::continuity::parse_acked(&t) {
            Ok(a) => a,
            Err(l) => return (CANNOT_TELL, format!("cannot tell: {} has a malformed row: {l}", ack_path.display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {}: {e}", ack_path.display())),
    };
    let found = crate::continuity::scan(conn).map(|t| t.breaks());
    let breaks = match found {
        Ok(b) => b,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    if args.iter().any(|a| a == "--acknowledge") {
        let rows = match crate::continuity::ack_rows(&breaks, &acked, flag(args, "--reason"), flag(args, "--through")) {
            Ok(r) => r,
            Err(m) => return (REFUSED, m),
        };
        use std::io::Write;
        let wrote = std::fs::OpenOptions::new().create(true).append(true).open(&ack_path).and_then(|mut f| f.write_all(rows.as_bytes()));
        return match wrote {
            Ok(()) => (0, format!("acknowledged {} break(s)", rows.lines().count())),
            Err(e) => (CANNOT_TELL, format!("cannot tell: {}: {e}", ack_path.display())),
        };
    }
    let (code, report) = crate::continuity::judge(&breaks, &acked);
    (code, crate::continuity::render(&report))
}


#[cfg(test)]
mod inline_tests {
    use super::inline_payload_from;

    #[test]
    fn publish_readiness_carries_its_stdin_body_as_json() {
        let argv = vec!["publish-readiness".to_string()];
        let out = inline_payload_from(argv, &mut r#"{"entries":[]}"#.as_bytes());
        assert_eq!(out, ["publish-readiness", "--json", r#"{"entries":[]}"#]);
    }
}
