//! Pure logic for the `work` client: which bead a raw argv is allowed to name, and how a
//! verb typed by the aeon becomes the richer argv spira-lc's `work` dispatch expects. No
//! I/O here — `main.rs` is the only thing that touches git, the environment or a socket —
//! so every rule below is covered without a fixture.

pub const CANNOT_TELL: i32 = 2;
pub const REFUSED: i32 = 3;

/// `sp-<alphanumeric>`, case-insensitive: the harness's bead id shape (see any `sp-XXXXX`
/// in this repo's own history). Deliberately loose — a false positive here just means an
/// unrelated-looking token gets refused when it didn't need to be, which is the safe
/// direction; a false negative would let a foreign bead id slip through unchecked.
pub fn looks_like_bead_id(s: &str) -> bool {
    if s.get(0..3).filter(|p| p.eq_ignore_ascii_case("sp-")).is_none() {
        return false;
    }
    let rest = &s[3..];
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric())
}

/// The one enforcement point for "a verb naming any bead other than the bound one is
/// refused" (acceptance criterion): every token in the verb's own arguments is checked,
/// not just a designated slot, because a designated slot can be bypassed by whatever else
/// the model hands us. `superseded-by`'s first argument is the sole, deliberate exception
/// — it names a *different* bead by design (the successor), never the bead acted upon.
pub fn foreign_bead_reference<'a>(verb: &str, bound: &str, args: &'a [String]) -> Option<&'a str> {
    for (i, a) in args.iter().enumerate() {
        if verb == "superseded-by" && i == 0 {
            continue;
        }
        if looks_like_bead_id(a) && !a.eq_ignore_ascii_case(bound) {
            return Some(a);
        }
    }
    None
}

/// The verbs bound to the summoned bead: they act on it and nothing else.
pub const BOUND_VERBS: &[&str] = &["show", "note", "submit", "done", "blocked", "file-followup", "split", "superseded-by"];

/// The verbs that name their own target (sp-st0mm): reads of other beads, a lane persona's
/// writes to other beads, the operator mailbox, and the harness tools that reach bd. Which
/// persona may run which is the broker's allow table (spira-lc/src/work.rs `ALLOW`), not
/// this client's — a client-side check is one the caller could skip.
pub const LANE_VERBS: &[&str] = &[
    "ask", "read", "list", "search", "note-on", "label-add", "label-remove", "dep-add", "relate", "reopen", "close-other", "file", "groom", "incident",
    "sop", "census", "queue", "landing-pass", "strand", "fence",
];

pub const VERBS: &[&str] = &[
    "show", "note", "submit", "done", "blocked", "file-followup", "split", "superseded-by", "ask", "read", "list", "search", "note-on", "label-add",
    "label-remove", "dep-add", "relate", "reopen", "close-other", "file", "groom", "incident", "sop", "census", "queue", "landing-pass", "strand",
    "fence",
];

/// The tokens the client appends itself; a caller typing one could otherwise name another
/// persona (`--actor`) or smuggle a body (`--stdin`) past the broker's trailer parse.
pub const RESERVED: &[&str] = &["--actor", "--stdin"];

/// A bound verb needs `SPIRA_WORK_BEAD_ID`; a lane verb runs unbound too (the archivist
/// has no bead of its own), and sends `-` in the bound slot.
pub fn needs_binding(verb: &str) -> bool {
    BOUND_VERBS.contains(&verb)
}

/// The flags whose value is a file the BROKER cannot read (it is another user, in another
/// directory): the client reads it and sends it as the request's stdin instead.
const FILE_FLAGS: &[&str] = &["--body-file", "--reason-file", "--why"];

/// Turns the caller's file and `-` arguments into the one body the request carries: a
/// `--body-file F` (F not `-`) is read here and becomes `--body-file -`; a lone `-` (or a
/// file flag's `-`) reads stdin. At most one body per request. Pure but for the two readers.
pub fn collect_body(args: &[String], read_file: impl Fn(&str) -> Result<String, String>, read_stdin: impl FnOnce() -> Result<String, String>) -> Result<(Vec<String>, Option<String>), String> {
    let mut out = Vec::with_capacity(args.len());
    let mut body: Option<String> = None;
    let mut want_stdin = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if FILE_FLAGS.contains(&a.as_str()) {
            if let Some(path) = args.get(i + 1).filter(|p| p.as_str() != "-") {
                if body.is_some() || want_stdin {
                    return Err(format!("work: more than one body in one request ({a} {path})"));
                }
                body = Some(read_file(path).map_err(|e| format!("work: reading {path}: {e}"))?);
                out.extend([a.clone(), "-".to_string()]);
                i += 2;
                continue;
            }
        }
        if a == "-" {
            if body.is_some() || want_stdin {
                return Err("work: more than one body in one request".to_string());
            }
            want_stdin = true;
        }
        out.push(a.clone());
        i += 1;
    }
    if want_stdin {
        body = Some(read_stdin().map_err(|e| format!("work: reading stdin: {e}"))?);
    }
    Ok((out, body))
}

/// Builds the argv `work.rs` on the server expects: `[bound, verb, ...]`. Evidence the
/// aeon's own environment must supply (git's HEAD for `submit`, the actor) is threaded in
/// by the caller, already computed — this function only assembles the wire request, it
/// never reaches out for anything itself.
pub fn build_request(bound: &str, verb: &str, verb_args: &[String], tip: Option<&str>, actor: &str) -> Result<Vec<String>, String> {
    build_request_with_body(bound, verb, verb_args, tip, actor, None)
}

/// [`build_request`], with the body [`collect_body`] gathered carried as `--stdin <text>`
/// just before the client's own `--actor`.
pub fn build_request_with_body(bound: &str, verb: &str, verb_args: &[String], tip: Option<&str>, actor: &str, body: Option<&str>) -> Result<Vec<String>, String> {
    if !VERBS.contains(&verb) {
        return Err(format!("work: unknown verb {verb:?} (want {})", VERBS.join(", ")));
    }
    if let Some(r) = verb_args.iter().find(|a| RESERVED.contains(&a.as_str())) {
        return Err(format!("work {verb}: {r} is the client's to send, never the caller's; refused"));
    }
    if matches!(verb, "file-followup" | "split") && verb_args.iter().any(|a| a == "--for") {
        return Err(format!("work {verb}: --for is the client's to send (it files as your own persona); refused"));
    }
    if LANE_VERBS.contains(&verb) {
        let mut req = vec!["work".to_string(), bound.to_string(), verb.to_string()];
        req.extend(verb_args.iter().cloned());
        if let Some(b) = body {
            req.extend(["--stdin".to_string(), b.to_string()]);
        }
        req.extend(["--actor".to_string(), actor.to_string()]);
        return Ok(req);
    }
    if !looks_like_bead_id(bound) {
        return Err(format!("work {verb}: this environment is not bound to a bead (SPIRA_WORK_BEAD_ID), and {verb} acts on the bound bead; refused"));
    }
    if let Some(foreign) = foreign_bead_reference(verb, bound, verb_args) {
        return Err(format!("work: {foreign} is not the bound bead ({bound}); refused"));
    }

    let mut req = vec!["work".to_string(), bound.to_string(), verb.to_string()];
    match verb {
        "show" => {}
        "note" => {
            req.extend(verb_args.iter().cloned());
            if let Some(b) = body {
                req.extend(["--stdin".to_string(), b.to_string()]);
            }
        }
        "submit" => {
            let Some(tip) = tip else { return Err("work submit: no tip could be read from the worktree".to_string()) };
            req.extend(["--tip".to_string(), tip.to_string(), "--actor".to_string(), actor.to_string()]);
        }
        "file-followup" | "split" => {
            // The aeon supplies only the title (design §3.5's surface has no --for): the
            // layer files as the same persona the aeon itself is running as, the same
            // identity --actor already carries.
            req.extend(verb_args.iter().cloned());
            req.extend(["--for".to_string(), actor.to_string(), "--actor".to_string(), actor.to_string()]);
        }
        "done" | "blocked" | "superseded-by" => {
            req.extend(verb_args.iter().cloned());
            req.extend(["--actor".to_string(), actor.to_string()]);
        }
        _ => unreachable!(),
    }
    Ok(req)
}

/// `lifecycle_enforce` off (DESIGN.md §3): every verb is refused with [`REFUSED`] before the
/// socket is touched. The lifecycle machine is not this host's record, so there is nothing
/// for `work` to talk to, and a no-op "success" would tell the aeon a submit or a close
/// happened when nothing did. The message names the legacy path the off-mode brief gives.
pub fn off_refusal(verb: &str) -> String {
    let legacy = match verb {
        "show" => "`bd show <id>`",
        "note" => "`bd note <id>`",
        "submit" | "done" | "superseded-by" => "closing the bead with evidence through `bd`, as your brief's finishing section says",
        "blocked" => "the escalation your brief names",
        "file-followup" | "split" => "`bead.sh file`",
        _ => "the bd commands your brief names",
    };
    format!(
        "work {verb}: refused — lifecycle_enforce is off, so the lifecycle machine is not this host's record and `work` never contacts it; nothing was done. Use {legacy} instead."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_bound_bead_everywhere() {
        assert!(foreign_bead_reference("done", "sp-3m1p9", &["--delivers".into(), "sp-3m1p9".into()]).is_none());
    }

    #[test]
    fn refuses_a_foreign_bead_in_note() {
        assert_eq!(foreign_bead_reference("note", "sp-3m1p9", &["see sp-other1".into()]), None);
        // A token must be *exactly* a bead id shape, not merely contain one as a substring —
        // free text like a note body is not scanned word-by-word here.
        assert_eq!(foreign_bead_reference("note", "sp-3m1p9", &["sp-other1".into()]), Some("sp-other1"));
    }

    #[test]
    fn refuses_a_foreign_bead_as_a_flag_value() {
        assert_eq!(foreign_bead_reference("done", "sp-3m1p9", &["--delivers".into(), "sp-other1".into()]), Some("sp-other1"));
    }

    #[test]
    fn superseded_by_names_a_different_bead_by_design() {
        assert_eq!(foreign_bead_reference("superseded-by", "sp-3m1p9", &["sp-other1".into()]), None);
    }

    #[test]
    fn superseded_by_still_refuses_a_second_foreign_reference() {
        assert_eq!(
            foreign_bead_reference("superseded-by", "sp-3m1p9", &["sp-other1".into(), "sp-other2".into()]),
            Some("sp-other2")
        );
    }

    #[test]
    fn build_request_show_is_bare() {
        assert_eq!(build_request("sp-3m1p9", "show", &[], None, "a").unwrap(), vec!["work", "sp-3m1p9", "show"]);
    }

    #[test]
    fn build_request_submit_needs_a_tip() {
        assert!(build_request("sp-3m1p9", "submit", &[], None, "a").is_err());
        assert_eq!(
            build_request("sp-3m1p9", "submit", &[], Some("deadbeef"), "a").unwrap(),
            vec!["work", "sp-3m1p9", "submit", "--tip", "deadbeef", "--actor", "a"]
        );
    }

    #[test]
    fn build_request_never_lets_submit_take_a_tip_argument() {
        // The design is explicit: submit never takes the tip as an argument. Any
        // user-supplied verb_args for `submit` are simply not part of the wire request —
        // there is no flag name that reaches it.
        let req = build_request("sp-3m1p9", "submit", &["--tip".into(), "usersupplied".into()], Some("real"), "a").unwrap();
        assert!(!req.contains(&"usersupplied".to_string()));
    }

    #[test]
    fn build_request_refuses_foreign_bead_before_touching_the_network() {
        assert!(build_request("sp-3m1p9", "note", &["sp-other1".into()], None, "a").is_err());
    }

    #[test]
    fn build_request_file_followup_infers_persona_from_actor() {
        let req = build_request("sp-3m1p9", "file-followup", &["a title".into()], None, "builder").unwrap();
        assert_eq!(req, vec!["work", "sp-3m1p9", "file-followup", "a title", "--for", "builder", "--actor", "builder"]);
    }

    #[test]
    fn off_refusal_names_the_switch_and_a_legacy_path_for_every_verb() {
        for v in VERBS {
            let m = off_refusal(v);
            assert!(m.contains("lifecycle_enforce is off") && m.contains("nothing was done") && m.contains("Use "), "{m}");
        }
        assert!(off_refusal("submit").contains("bd"));
    }

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn a_lane_verb_names_another_bead_and_carries_the_persona() {
        assert_eq!(
            build_request("sp-3m1p9", "note-on", &v(&["sp-other1", "hi"]), None, "groomer").unwrap(),
            v(&["work", "sp-3m1p9", "note-on", "sp-other1", "hi", "--actor", "groomer"])
        );
    }

    #[test]
    fn a_lane_verb_runs_unbound() {
        assert_eq!(
            build_request_with_body("-", "file", &v(&["t", "--kind", "insight", "--body-file", "-"]), None, "archivist", Some("b")).unwrap(),
            v(&["work", "-", "file", "t", "--kind", "insight", "--body-file", "-", "--stdin", "b", "--actor", "archivist"])
        );
        assert!(needs_binding("note") && !needs_binding("ask"));
    }

    #[test]
    fn a_bound_verb_unbound_is_refused() {
        assert!(build_request("-", "note", &v(&["hi"]), None, "a").is_err());
    }

    #[test]
    fn the_caller_never_names_its_own_persona() {
        assert!(build_request("sp-3m1p9", "queue", &v(&["eject", "sp-x", "--actor", "czar"]), None, "builder").is_err());
        assert!(build_request("sp-3m1p9", "done", &v(&["--delivers", "x", "--actor", "czar"]), None, "builder").is_err());
        assert!(build_request("sp-3m1p9", "note-on", &v(&["sp-x", "--stdin", "y"]), None, "czar").is_err());
        assert!(build_request("sp-3m1p9", "split", &v(&["t", "--for", "ops"]), None, "builder").is_err());
    }

    #[test]
    fn note_carries_a_stdin_body() {
        assert_eq!(
            build_request_with_body("sp-3m1p9", "note", &v(&["-"]), None, "a", Some("prose")).unwrap(),
            v(&["work", "sp-3m1p9", "note", "-", "--stdin", "prose"])
        );
    }

    #[test]
    fn collect_body_reads_a_file_flag_or_stdin_once() {
        let file = |p: &str| if p == "/w/f" { Ok("from file".to_string()) } else { Err("no".to_string()) };
        let (a, b) = collect_body(&v(&["t", "--body-file", "/w/f"]), file, || panic!("stdin not wanted")).unwrap();
        assert_eq!((a, b.as_deref()), (v(&["t", "--body-file", "-"]), Some("from file")));
        let (a, b) = collect_body(&v(&["sop-x", "-"]), file, || Ok("piped".to_string())).unwrap();
        assert_eq!((a, b.as_deref()), (v(&["sop-x", "-"]), Some("piped")));
        let (_, b) = collect_body(&v(&["--why", "-"]), file, || Ok("w".to_string())).unwrap();
        assert_eq!(b.as_deref(), Some("w"));
        assert!(collect_body(&v(&["--body-file", "/w/f", "-"]), file, || Ok(String::new())).is_err());
        let (_, b) = collect_body(&v(&["show"]), file, || panic!("stdin not wanted")).unwrap();
        assert!(b.is_none());
    }

    #[test]
    fn build_request_refuses_unknown_verb() {
        assert!(build_request("sp-3m1p9", "close", &[], None, "a").is_err());
    }
}
