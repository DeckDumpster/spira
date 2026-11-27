//! The cutover round's own surface (sp-o7nbr): batch creation, and the three cross-machine
//! cascades — land, settle, abandon — that a batch's own transition emits to every member's
//! delivery and bead row in one transaction (design §3.1.3's "the batch machine emits its
//! members' delivery exits"). `show`/`list`/`history`/`event` (main.rs) are unchanged; this
//! module is additive.
//!
//! Row creation is a constructor, not a transition — `LifecycleRow::filed`/`BatchRow::cut`/
//! `DeliveryRow::start_queue` already say so in `lifecycle` — so `create-bead` and the batch
//! row inserted by `cut` log no event of their own, exactly as replay never needs one to
//! reach the same initial row. The `Deliver`/`MemberAdded`/`Cut` transitions inside `cut`,
//! and every step inside `land`/`settle`/`abandon-batch`, are real events on real rows and
//! are logged like any other transition.

use lifecycle::{batch, bead};


pub(crate) const REFUSED: i32 = 3;

/// A lifecycle key: `prefix`, `-`, then a non-empty tail of alphanumerics, `.`, `-` or `_` not ending in `-`.
pub fn is_row_key(prefix: &str, x: &str) -> bool {
    let tail = x.split_at_checked(prefix.len()).filter(|(head, _)| head.eq_ignore_ascii_case(prefix)).and_then(|(_, rest)| rest.strip_prefix('-'));
    tail.is_some_and(|t| !t.is_empty() && !t.ends_with('-') && t.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')))
}

/// This installation's bead id prefix, pinned at the first successful read; a refusal is not kept.
pub fn row_prefix() -> Result<String, String> {
    static PREFIX: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    if let Some(kept) = PREFIX.get() {
        return Ok(kept.clone());
    }
    let read = spira_config::process::cfg("SPIRA_ID_PREFIX")?;
    if read.is_empty() {
        return Err("SPIRA_ID_PREFIX is empty: spira.id_prefix names the prefix of this installation's bead ids — refusing".into());
    }
    Ok(PREFIX.get_or_init(|| read).clone())
}

/// The planning reader `create-bead` asks for a bead's type: `plan items` rows of the ids given.
pub type PlanReader<'a> = dyn FnMut(&[&str]) -> Result<Vec<serde_json::Value>, String> + 'a;

/// `create-bead`'s refusal of a bead whose type has no lifecycle record (an epic, a decision,
/// …), named; `None` lets the create go ahead. The type is the planning plane's, read through
/// `read`; a bead the planning plane cannot answer for is created, so a filing is never stranded
/// rowless by a planning read that failed.
pub fn refuse_no_record_type(prefix: &str, args: &[String], read: &mut PlanReader) -> Option<(i32, String)> {
    let id = args.first().filter(|i| is_row_key(prefix, i))?;
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
    if args.first().map(String::as_str) == Some("git-fact") && args.get(1).map(String::as_str) == Some("put") && args.last().map(String::as_str) == Some("--json") {
        args.pop();
    }
    let verb = (args.first().map(String::as_str), args.get(1).map(String::as_str));
    let carries_stdin_request = verb.0 == Some("work-request") && matches!(verb.1, Some("put" | "finish")) && !args.iter().any(|a| a == "--stdin");
    let reads_stdin = matches!(
        verb,
        (Some("batch-progress" | "publish-readiness"), _)
            | (Some("section"), Some("apply-spec" | "publish-status"))
            | (Some("intent"), Some("put"))
            | (Some("git-fact"), Some("put"))
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
    if carries_stdin_request {
        let mut body = String::new();
        let _ = input.read_to_string(&mut body);
        args.push("--stdin".into());
        args.push(body);
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
/// commit in the prerequisite's own range above the batch's base, read from the ranges recorded for
/// `(repo, base, tip)`. A member with no recorded range yields no edge: the declared `stack` still applies.
pub(crate) fn branch_containment<B: state_store::Backend>(
    store: &crate::control::store::Store<B>,
    batch_row: &batch::BatchRow,
    members: &[(String, String)],
) -> std::collections::BTreeSet<(String, String)> {
    let mut edges = std::collections::BTreeSet::new();
    let Some(base) = batch_row.base.as_deref() else { return edges };
    let ranges: Vec<(&String, std::collections::BTreeSet<String>)> = members
        .iter()
        .filter_map(|(id, tip)| store.git_fact_range(&batch_row.repo, base, tip).ok().flatten().map(|c| (id, c)))
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
    member_stacks: &[(String, bead::LifecycleRow)],
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

#[cfg(test)]
mod row_key_tests {
    use super::is_row_key;

    #[test_id(toa98dq)]
    #[test]
    fn only_an_id_carrying_the_given_prefix_and_its_hyphen_is_a_key() {
        for key in ["zz-abc", "zz-a.1"] {
            assert!(is_row_key("zz", key), "{key}");
        }
        for not in ["sp-abc", "zz-", "zzz-a", "zz-a-", "zz"] {
            assert!(!is_row_key("zz", not), "{not}");
        }
    }

    #[test_id(txz9fca)]
    #[test]
    fn under_the_sp_prefix_the_tail_and_case_rules_hold() {
        for key in ["sp-a", "SP-a", "Sp-A1", "sp-a.1", "sp-a-b", "sp-a_b", "sp-a.", "sp-_", "sp--a"] {
            assert!(is_row_key("sp", key), "{key}");
        }
        for not in ["", "sp", "sp-", "spa", "sp-a b", "sp-a-", "-x", "s\u{e9}-a", "sp-\u{e9}", "see sp-a"] {
            assert!(!is_row_key("sp", not), "{not:?}");
        }
    }
}

#[cfg(test)]
mod inline_tests {
    use super::inline_payload_from;

    #[test_id(tzzionh)]
    #[test]
    fn publish_readiness_carries_its_stdin_body_as_json() {
        let argv = vec!["publish-readiness".to_string()];
        let out = inline_payload_from(argv, &mut r#"{"entries":[]}"#.as_bytes());
        assert_eq!(out, ["publish-readiness", "--json", r#"{"entries":[]}"#]);
    }
}

#[cfg(test)]
mod containment_tests {
    use super::branch_containment;
    use crate::control::store::Store;
    use runtime::ports::VirtualClock;
    use state_store::{Config, Contract, MemBackend, Ports, Request};

    fn store() -> Store<MemBackend> {
        let clock = VirtualClock::new(100_000);
        Store::new(Contract::open(MemBackend::new(), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: 1_000 }).unwrap(), 1_000)
    }

    fn record(s: &Store<MemBackend>, repo: &str, tip: &str, commits: &[&str]) {
        let commits: Vec<String> = commits.iter().map(|c| c.to_string()).collect();
        s.put_git_fact_range(repo, "base", tip, &commits, &Request::new(&format!("t/{repo}/{tip}"), "batcher", "t")).unwrap();
    }

    fn members() -> Vec<(String, String)> {
        vec![("sp-dep".into(), "tip-dep".into()), ("sp-pre".into(), "tip-pre".into())]
    }

    #[test_id(t1jwe9w)]
    #[test]
    fn a_member_whose_range_holds_anothers_whole_range_is_its_dependent() {
        let s = store();
        record(&s, "dumpstar", "tip-pre", &["c1"]);
        record(&s, "dumpstar", "tip-dep", &["c1", "c2"]);
        let row = lifecycle::batch::BatchRow::cut("b1", "dumpstar", "h", "base");
        let edges = branch_containment(&s, &row, &members());
        assert_eq!(edges, std::collections::BTreeSet::from([("sp-dep".to_string(), "sp-pre".to_string())]));
    }

    #[test_id(tr82fg4)]
    #[test]
    fn with_no_range_recorded_no_edge_is_found() {
        let s = store();
        let row = lifecycle::batch::BatchRow::cut("b1", "dumpstar", "h", "base");
        assert!(branch_containment(&s, &row, &members()).is_empty());
        record(&s, "dumpstar", "tip-dep", &["c1", "c2"]);
        assert!(branch_containment(&s, &row, &members()).is_empty(), "one range alone contains nothing");
    }

    #[test_id(t3ofcac)]
    #[test]
    fn ranges_recorded_for_another_repo_or_base_are_not_read() {
        let s = store();
        record(&s, "dumpstar", "tip-pre", &["c1"]);
        record(&s, "dumpstar", "tip-dep", &["c1", "c2"]);
        let other_repo = lifecycle::batch::BatchRow::cut("b1", "nowhere", "h", "base");
        assert!(branch_containment(&s, &other_repo, &members()).is_empty());
        let other_base = lifecycle::batch::BatchRow::cut("b1", "dumpstar", "h", "base2");
        assert!(branch_containment(&s, &other_base, &members()).is_empty());
    }
}
