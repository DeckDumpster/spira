//! The planning plane's write rule: which bd calls change planning data, and the stamp that
//! must follow each one so the planning watermark moves (`spira-lc plan stamp`).

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

/// Runs `spira-lc plan stamp` after a planning write `caller` made. A stamp that fails is
/// reported, never raised: the write it follows has already landed.
pub fn stamp_after(caller: &str) {
    let stamped = crate::lc_call::LcClient::configured().and_then(|c| c.within(crate::lc_call::LC_TIMEOUT).call(&["plan", "stamp"]).map_err(String::from));
    match stamped {
        Ok(r) if r.code == 0 => {}
        Ok(r) => eprintln!("{caller}: the planning write landed but the watermark did not advance: {}", r.text()),
        Err(e) => eprintln!("{caller}: the planning write landed but the watermark did not advance: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::is_planning_write;

    #[test]
    fn a_write_to_a_bead_an_edge_or_a_label_is_a_planning_write_and_a_read_is_not() {
        for w in [&["create", "t"][..], &["update", "sp-a", "--status", "open"], &["close", "sp-a"], &["note", "sp-a", "x"], &["dep", "add", "a", "b"], &["dep", "relate", "a", "b"], &["label", "add", "a", "l"], &["comments", "add", "a", "x"], &["supersede", "a", "--with", "b"], &["remember", "--key", "k", "v"], &["set-state", "a", "k=v"], &["gate", "resolve", "g"], &["dolt", "pull"], &["sync"]] {
            assert!(is_planning_write(w), "{w:?}");
        }
        for r in [&["show", "sp-a", "--json"][..], &["list", "--all"], &["dep", "list", "a"], &["label", "list", "a"], &["comments", "a"], &["children", "e"], &["sql", "select 1"], &["kv", "set", "k", "1"], &["config", "set", "x", "y"], &["migrate", "schema"], &["gate", "list"], &["--json"], &[]] {
            assert!(!is_planning_write(r), "{r:?}");
        }
    }
}
