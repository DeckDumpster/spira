//! Pure logic for the `work` client: which bead a raw argv is allowed to name, and how a
//! verb typed by the aeon becomes the richer argv spira-lc's `work` dispatch expects. No
//! I/O here — `main.rs` is the only thing that touches git, the environment or a socket —
//! so every rule below is covered without a fixture.

pub const CANNOT_TELL: i32 = 2;

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

pub const VERBS: &[&str] = &["show", "note", "submit", "done", "blocked", "file-followup", "split", "superseded-by"];

/// Builds the argv `work.rs` on the server expects: `[bound, verb, ...]`. Evidence the
/// aeon's own environment must supply (git's HEAD for `submit`, the actor) is threaded in
/// by the caller, already computed — this function only assembles the wire request, it
/// never reaches out for anything itself.
pub fn build_request(bound: &str, verb: &str, verb_args: &[String], tip: Option<&str>, actor: &str) -> Result<Vec<String>, String> {
    if !VERBS.contains(&verb) {
        return Err(format!("work: unknown verb {verb:?} (want {})", VERBS.join(", ")));
    }
    if let Some(foreign) = foreign_bead_reference(verb, bound, verb_args) {
        return Err(format!("work: {foreign} is not the bound bead ({bound}); refused"));
    }

    let mut req = vec!["work".to_string(), bound.to_string(), verb.to_string()];
    match verb {
        "show" => {}
        "note" => req.extend(verb_args.iter().cloned()),
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
    fn build_request_refuses_unknown_verb() {
        assert!(build_request("sp-3m1p9", "close", &[], None, "a").is_err());
    }
}
