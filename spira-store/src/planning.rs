//! The planning plane's write rule: which bd calls change planning data, so each can be
//! followed by the stamp that moves the planning watermark (`spira-lc plan stamp`).

/// bd verbs that never change a bead, an edge, a label, a comment or a memory. Anything else is
/// a planning write: a verb missing here costs a spare stamp, while a write missing from a
/// write list would let a poller miss a change.
const READS: &[&str] = &[
    "show", "list", "count", "ready", "blocked", "query", "search", "stats", "status", "children", "graph", "find", "export", "history", "memories", "sql", "state",
    "version", "help", "info", "where", "doctor", "prime", "onboard", "quickstart", "human",
];

/// bd's own settings and setup: not planning data, and `kv` holds the watermark. A sync, pull or
/// restore does change beads, so those stamp.
const NOT_PLANNING: &[&str] = &["kv", "config", "migrate", "init", "hooks"];

/// Subcommands that read under a verb whose other subcommands write.
const READ_SUBCOMMANDS: &[(&str, &[&str])] = &[("dep", &["list", "tree", "cycles"]), ("label", &["list", "list-all"]), ("comments", &[]), ("gate", &["list", "show"])];

/// Whether `bd <args>` (without `-C <db>`) writes the planning plane.
pub fn is_planning_write<S: AsRef<str>>(args: &[S]) -> bool {
    let mut words = args.iter().map(AsRef::as_ref).filter(|a| !a.starts_with('-'));
    let Some(verb) = words.next() else { return false };
    if READS.contains(&verb) || NOT_PLANNING.contains(&verb) {
        return false;
    }
    match READ_SUBCOMMANDS.iter().find(|(v, _)| *v == verb) {
        Some(("comments", _)) => words.next() == Some("add"),
        Some((_, reads)) => !words.next().is_some_and(|sub| reads.contains(&sub)),
        None => true,
    }
}

/// The bead whose claim inputs a planning write changes, and the blockers it newly names: a
/// dependency added or removed (the dependent), a priority, parent, status or defer change, or
/// the split label gained or lost.
pub fn claim_input_change<S: AsRef<str>>(args: &[S]) -> Option<(String, Vec<String>)> {
    let all: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
    let words: Vec<&str> = all.iter().copied().filter(|a| !a.starts_with('-')).collect();
    let bead = |i: usize| words.get(i).map(|w| w.to_string());
    match words.first().copied()? {
        "dep" if matches!(words.get(1).copied(), Some("add")) => Some((bead(2)?, vec![bead(3)?])),
        "dep" if matches!(words.get(1).copied(), Some("remove" | "rm")) => Some((bead(2)?, vec![])),
        "label" if matches!(words.get(1).copied(), Some("add" | "remove" | "rm")) && words.get(3).copied() == Some(spira_config::nonwork::SPLIT_LABEL) => Some((bead(2)?, vec![])),
        "update" if all.iter().any(|a| matches!(*a, "--priority" | "-p" | "--parent" | "--status" | "-s" | "--defer") || a.starts_with("--priority=") || a.starts_with("--parent=") || a.starts_with("--status=") || a.starts_with("--defer=")) => Some((bead(1)?, vec![])),
        _ => None,
    }
}

/// The ids whose claim inputs (parent, labels, blocking edges) a successful write of `args`
/// changed, `stdout` being what the write printed: every bead a create verb made, the dependent
/// of a `dep` write, the bead of a label write or of an `update` of its parent or labels.
pub fn claim_inputs_changed<S: AsRef<str>>(args: &[S], stdout: &str) -> Vec<String> {
    let all: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
    let words: Vec<&str> = all.iter().copied().filter(|a| !a.starts_with('-')).collect();
    let word = |i: usize| words.get(i).map(|w| w.to_string()).into_iter().collect::<Vec<_>>();
    match words.first().copied() {
        Some("create" | "import" | "mol" | "q") => crate::lifecycle_row::created_ids(stdout),
        Some("dep") if words.get(1).is_some_and(|sub| !READ_SUBCOMMANDS.iter().any(|(v, reads)| *v == "dep" && reads.contains(sub))) => word(2),
        Some("label") if matches!(words.get(1).copied(), Some("add" | "remove" | "rm")) => word(2),
        Some("update") if all.iter().any(|a| ["--parent", "--add-label", "--remove-label"].iter().any(|f| a == f || a.starts_with(&format!("{f}=")))) => word(1),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{claim_input_change, claim_inputs_changed, is_planning_write};

    #[test_id(t6fxhj6)]
    #[test]
    fn a_dependency_a_priority_or_the_split_label_change_a_beads_claim_inputs_and_a_note_does_not() {
        let got = |a: &[&str]| claim_input_change(a);
        assert_eq!(got(&["dep", "add", "sp-a", "sp-b"]), Some(("sp-a".into(), vec!["sp-b".into()])));
        assert_eq!(got(&["dep", "remove", "sp-a", "sp-b"]), Some(("sp-a".into(), vec![])));
        assert_eq!(got(&["update", "sp-a", "--priority", "0"]), Some(("sp-a".into(), vec![])));
        assert_eq!(got(&["label", "add", "sp-a", spira_config::nonwork::SPLIT_LABEL]), Some(("sp-a".into(), vec![])));
        for none in [&["label", "add", "sp-a", "plan"][..], &["note", "sp-a", "x"], &["update", "sp-a", "--assignee", "x"], &["dep", "relate", "a", "b"], &["create", "t"]] {
            assert_eq!(got(none), None, "{none:?}");
        }
    }

    #[test_id(t88r55e)]
    #[test]
    fn a_write_that_moves_a_parent_a_label_or_an_edge_names_the_bead_whose_claim_inputs_it_changed() {
        let got = |a: &[&str], out: &str| claim_inputs_changed(a, out);
        assert_eq!(got(&["dep", "add", "sp-a", "sp-b"], ""), ["sp-a"]);
        assert_eq!(got(&["dep", "remove", "sp-a", "sp-b"], ""), ["sp-a"]);
        assert_eq!(got(&["label", "add", "sp-a", "x"], ""), ["sp-a"]);
        assert_eq!(got(&["update", "sp-a", "--parent", "sp-e"], ""), ["sp-a"]);
        assert_eq!(got(&["update", "sp-a", "--add-label", "x"], ""), ["sp-a"]);
        assert_eq!(got(&["create", "T", "--json"], "{\"ids\":[\"sp-n\",\"sp-m\"]}"), ["sp-n", "sp-m"]);
        assert_eq!(got(&["create", "T", "--silent"], "sp-n\n"), ["sp-n"]);
        for none in [&["note", "sp-a", "x"][..], &["update", "sp-a", "--assignee", "x"], &["dep", "list", "sp-a"], &["label", "list", "sp-a"], &["show", "sp-a"]] {
            assert!(got(none, "sp-a\n").is_empty(), "{none:?}");
        }
    }

    #[test_id(tnbvpsc)]
    #[test]
    fn a_write_to_a_bead_an_edge_or_a_label_is_a_planning_write_and_a_read_is_not() {
        for w in [&["create", "t"][..], &["update", "sp-a", "--status", "open"], &["close", "sp-a"], &["note", "sp-a", "x"], &["dep", "add", "a", "b"], &["dep", "relate", "a", "b"], &["label", "add", "a", "l"], &["comments", "add", "a", "x"], &["supersede", "a", "--with", "b"], &["remember", "--key", "k", "v"], &["set-state", "a", "k=v"], &["gate", "resolve", "g"], &["forget", "k"], &["gate", "check", "--type", "gh:run"], &["dolt", "pull"], &["sync"]] {
            assert!(is_planning_write(w), "{w:?}");
        }
        for r in [&["show", "sp-a", "--json"][..], &["list", "--all"], &["dep", "list", "a"], &["label", "list", "a"], &["comments", "a"], &["children", "e"], &["sql", "select 1"], &["kv", "set", "k", "1"], &["config", "set", "x", "y"], &["migrate", "schema"], &["gate", "list"], &["--json"], &[]] {
            assert!(!is_planning_write(r), "{r:?}");
        }
    }
}
