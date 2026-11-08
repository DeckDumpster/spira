//! Pure logic for the `work` client: which bead a raw argv is allowed to name, and how a
//! verb typed by the dwarf becomes the richer argv spira-lc's `work` dispatch expects. No
//! I/O here — `main.rs` is the only thing that touches git, the environment or a socket —
//! so every rule below is covered without a fixture.

pub mod help;
pub mod review;
pub mod trusted;
use help::{refusal, R};

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
    // `sp-<alnum>` then any number of `.<digits>` child parts (sp-oqf8c.3, sp-zs04v.3.1): a
    // child bead must be able to bind and submit like any other (the broker's own check
    // already allows the dot; this one refused every child bead).
    let mut parts = s[3..].split('.');
    let head = parts.next().unwrap_or("");
    !head.is_empty()
        && head.chars().all(|c| c.is_ascii_alphanumeric())
        && parts.all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// A `[[check]]` row's command, run from the worktree root with no shell.
pub fn done_argv(run: &str) -> Vec<String> {
    habits::config::argv(run, &[])
}

/// The ref `work checks` selects the affected crates against, as `work crates` does.
pub const CHECKS_BASE: &str = "local/main";
/// The per-worktree file (under the worktree's git dir) a passing `work checks` is recorded in.
pub const CHECKS_PASS_FILE: &str = "spira-checks.pass";
/// The token in a `[[check]]` row's `run` that becomes the crates the diff affects.
pub const AFFECTED: &str = "{affected}";

/// The crates a branch's diff affects, as `{affected}` in a `[[check]]` row expands it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Affected {
    /// A workspace-wide build input changed (root `Cargo.toml`, `Cargo.lock`, the toolchain pin,
    /// `.cargo/`): `--workspace`.
    Workspace,
    /// The touched crates and every transitive dependent; empty when no crate is touched.
    Crates(Vec<String>),
}

/// sp-8oc3xj: the round's unit tier's own selection (`testenv::unit::write_crates`, which writes
/// `target/unit-tests.crates`, calls `gate::compose::select_crates(.., true)`), so the submit
/// lint and the round check the same crates (law-test-selection-and-plan-are-one-source).
pub fn affected_scope(diff_raw: &[u8], metadata_json: &str) -> Result<Affected, String> {
    use gate::compose::{classify, parse_diff_raw, parse_metadata, select_crates, Class};
    let members = parse_metadata(metadata_json)?;
    if parse_diff_raw(diff_raw).iter().any(|c| classify(c, &members) == Class::Workspace) {
        return Ok(Affected::Workspace);
    }
    select_crates(diff_raw, metadata_json, true).map(Affected::Crates)
}

/// A row's argv with `{affected}` expanded to `--workspace` or `-p <crate>` words. `None`: the
/// row names `{affected}` and the diff affects no crate, so there is nothing for it to check.
pub fn check_argv(run: &str, affected: &Affected) -> Option<Vec<String>> {
    let argv = done_argv(run);
    if !argv.iter().any(|t| t == AFFECTED) {
        return Some(argv);
    }
    let words: Vec<String> = match affected {
        Affected::Workspace => vec!["--workspace".to_string()],
        Affected::Crates(c) if c.is_empty() => return None,
        Affected::Crates(c) => c.iter().flat_map(|c| ["-p".to_string(), c.clone()]).collect(),
    };
    Some(argv.into_iter().flat_map(|t| if t == AFFECTED { words.clone() } else { vec![t] }).collect())
}

/// What a pass of `work checks` is recorded against: the tip, the base the crates were selected
/// against, and the checks file's text.
pub fn checks_pass_key(head: &str, base: &str, checks_toml: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    checks_toml.hash(&mut h);
    format!("{head} {base} {:016x}", h.finish())
}

/// Whether `work submit` may skip the checks: they passed at this exact key and the tree holds
/// no tracked change, so what passed is the tip submitted.
pub fn checks_already_passed(recorded: Option<&str>, key: &str, clean: bool) -> bool {
    clean && recorded.map(str::trim) == Some(key)
}

/// A cargo row run under `spira-admit run` when builds are admitted (`RUSTC_WRAPPER` is
/// spira-admit): the row waits for its compile slot first, and its `timeout` starts only once
/// admitted; the rustcs under it inherit the lease and never queue or yield again.
pub fn admitted(argv: Vec<String>, rustc_wrapper: Option<&str>, who: &str) -> Vec<String> {
    let Some(w) = rustc_wrapper.filter(|w| std::path::Path::new(w).file_name().is_some_and(|n| n == "spira-admit")) else { return argv };
    [w, "run", "--pool", "compile", "--who", who, "--"].iter().map(|s| s.to_string()).chain(argv).collect()
}

/// The longest wait spira-admit reported (`admitted to compile slot N after Ns`).
pub fn admission_wait_secs(stderr: &str) -> Option<u64> {
    stderr
        .lines()
        .filter_map(|l| l.split_once("admitted to compile slot ")?.1.split_once(" after ")?.1.strip_suffix('s')?.trim().parse().ok())
        .max()
}

/// The one enforcement point for "a verb naming any bead other than the bound one is
/// refused" (acceptance criterion): every token in the verb's own arguments is checked,
/// not just a designated slot, because a designated slot can be bypassed by whatever else
/// the model hands us. `superseded-by`'s first argument is the sole, deliberate exception
/// — it names a *different* bead by design (the successor), never the bead acted upon.
/// The flags of a target-bead verdict whose value names the follow-on bead or escalation the
/// dwarf filed: another bead by design, validated by the broker.
const TARGET_EXIT_FLAGS: &[&str] = &["--follow-on", "--escalation"];

pub fn foreign_bead_reference<'a>(verb: &str, bound: &str, args: &'a [String]) -> Option<&'a str> {
    for (i, a) in args.iter().enumerate() {
        if verb == "superseded-by" && i == 0 {
            continue;
        }
        if i > 0 && matches!(verb, "submit" | "done") && TARGET_EXIT_FLAGS.contains(&args[i - 1].as_str()) {
            continue;
        }
        if looks_like_bead_id(a) && !a.eq_ignore_ascii_case(bound) {
            return Some(a);
        }
    }
    None
}

/// The verbs bound to the called bead: they act on it and nothing else.
pub const BOUND_VERBS: &[&str] = &["show", "note", "submit", "done", "blocked", "file-followup", "split", "superseded-by", "ack"];

/// The verbs that name their own target (sp-st0mm): reads of other beads, a lane persona's
/// writes to other beads, the operator mailbox, and the harness tools that reach bd. Which
/// persona may run which is the broker's allow table (spira-lc/src/work.rs `ALLOW`), not
/// this client's — a client-side check is one the caller could skip.
pub const LANE_VERBS: &[&str] = &[
    "ask", "read", "list", "search", "note-on", "label-add", "label-remove", "dep-add", "relate", "reopen", "close-other", "close-ask", "file", "groom", "incident",
    "sop", "census", "queue", "landing-pass", "strand", "fence", "hel", "mender",
];

pub const VERBS: &[&str] = &[
    "show", "note", "submit", "done", "blocked", "file-followup", "split", "superseded-by", "ack", "ask", "read", "list", "search", "note-on", "label-add",
    "label-remove", "dep-add", "relate", "reopen", "close-other", "close-ask", "file", "groom", "incident", "sop", "census", "queue", "landing-pass", "strand",
    "fence", "hel", "mender",
];

/// Verbs answered in this process, never sent to the socket.
pub const LOCAL_VERBS: &[&str] = &[
    "crates", "statute-check", "review", "law", "checks", "prime", "help",
];

/// The `-p` selection for a branch's diff: every workspace member it touches plus its direct
/// dependents — the gate's own selection (`gate::compose`), never `--workspace`. A
/// workspace-wide input (root manifest, lockfile, toolchain pin) names every member.
pub fn crates_selection(diff_raw: &[u8], metadata_json: &str) -> Result<Vec<String>, String> {
    gate::compose::select_crates(diff_raw, metadata_json, false)
}

/// What `work statute-check` owes for a plan: `None` when it matches no governed decision (no
/// check, no note); otherwise the full text of every statute returned, and the note recorded on
/// the bead naming the plan and the statutes. A returned statute whose text is missing from
/// the mirror is an error, never a silent omission.
pub fn statute_check_report(registry: &str, plan: &str, text_of: impl Fn(&str) -> Option<String>) -> Result<Option<(String, String)>, String> {
    let hits = bead::statute::plan_check(registry, plan)?;
    if hits.is_empty() {
        return Ok(None);
    }
    let mut out = String::new();
    for h in &hits {
        let text = text_of(&h.slug).ok_or_else(|| format!("{} was returned but its text is not in the statute mirror", h.slug))?;
        out.push_str(&format!("{} (matched {:?})\n{}\n\n", h.slug, h.matched, text.trim()));
    }
    let slugs = hits.iter().map(|h| h.slug.as_str()).collect::<Vec<_>>().join(", ");
    let note = format!("Statute check before a governed decision.\nPlan: {}\nReturned: {slugs}", plan.trim());
    Ok(Some((out, note)))
}

/// The cargo target a `work checks` build writes to: the session's `CARGO_TARGET_DIR` — its owned
/// build directory (sp-ke0uk8), selectively cleaned for this worktree at lease — when it has one,
/// else the worktree's own `target/`. Never a second tree beneath it: one directory per bead.
pub fn check_target(var: Option<std::ffi::OsString>, root: &std::path::Path) -> std::path::PathBuf {
    var.map(std::path::PathBuf::from).unwrap_or_else(|| root.join("target"))
}

/// `crates` as the words to append to a cargo command: `-p a -p b`, empty when nothing buildable changed.
pub fn crates_words(crates: &[String]) -> String {
    crates.iter().map(|c| format!("-p {c}")).collect::<Vec<_>>().join(" ")
}

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
                    return Err(refusal(R::TwoBodies, "", &[("detail", &format!(" ({a} {path})"))]));
                }
                body = Some(read_file(path).map_err(|e| refusal(R::BodyRead, "", &[("err", &format!("reading {path}: {e}"))]))?);
                out.extend([a.clone(), "-".to_string()]);
                i += 2;
                continue;
            }
        }
        if a == "-" {
            if body.is_some() || want_stdin {
                return Err(refusal(R::TwoBodies, "", &[("detail", "")]));
            }
            want_stdin = true;
        }
        out.push(a.clone());
        i += 1;
    }
    if want_stdin {
        body = Some(read_stdin().map_err(|e| refusal(R::BodyRead, "", &[("err", &format!("reading stdin: {e}"))]))?);
    }
    Ok((out, body))
}

/// `mender diagnose|outcome` always carry their text on stdin; a lone `-` is dropped, since the
/// mender tool takes no such argument.
pub fn collect_mender_body(args: &[String], read_stdin: impl FnOnce() -> Result<String, String>) -> Result<(Vec<String>, Option<String>), String> {
    if !matches!(args.first().map(String::as_str), Some("diagnose" | "outcome")) {
        return Ok((args.to_vec(), None));
    }
    let rest: Vec<String> = args.iter().filter(|a| a.as_str() != "-").cloned().collect();
    let body = read_stdin().map_err(|e| refusal(R::BodyRead, "", &[("err", &format!("reading stdin: {e}"))]))?;
    Ok((rest, Some(body)))
}

/// Builds the argv `work.rs` on the server expects: `[bound, verb, ...]`. Evidence the
/// dwarf's own environment must supply (git's HEAD for `submit`, the actor) is threaded in
/// by the caller, already computed — this function only assembles the wire request, it
/// never reaches out for anything itself.
pub fn build_request(bound: &str, verb: &str, verb_args: &[String], tip: Option<&str>, actor: &str) -> Result<Vec<String>, String> {
    build_request_with_body(bound, verb, verb_args, tip, actor, None)
}

/// [`build_request`], with the body [`collect_body`] gathered carried as `--stdin <text>`
/// just before the client's own `--actor`.
pub fn build_request_with_body(bound: &str, verb: &str, verb_args: &[String], tip: Option<&str>, actor: &str, body: Option<&str>) -> Result<Vec<String>, String> {
    if !VERBS.contains(&verb) {
        return Err(refusal(R::UnknownVerb, "", &[("verb", verb), ("want", &VERBS.join(", "))]));
    }
    if let Some(r) = verb_args.iter().find(|a| RESERVED.contains(&a.as_str())) {
        return Err(refusal(R::ReservedFlag, verb, &[("flag", r)]));
    }
    if matches!(verb, "file-followup" | "split") && verb_args.iter().any(|a| a == "--for") {
        return Err(refusal(R::ForFlag, verb, &[]));
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
        return Err(refusal(R::NotBound, verb, &[]));
    }
    if let Some(foreign) = foreign_bead_reference(verb, bound, verb_args) {
        return Err(refusal(R::ForeignBead, verb, &[("foreign", foreign), ("bound", bound)]));
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
            let Some(tip) = tip else { return Err(refusal(R::SubmitNoTip, "submit", &[])) };
            if let Some(t) = verb_args.iter().find(|a| matches!(a.as_str(), "--tip")) {
                return Err(refusal(R::SubmitTipArg, "submit", &[("flag", t)]));
            }
            req.extend(verb_args.iter().cloned());
            req.extend(["--tip".to_string(), tip.to_string(), "--actor".to_string(), actor.to_string()]);
        }
        "file-followup" | "split" => {
            // The dwarf supplies only the title (design §3.5's surface has no --for): the
            // layer files as the same persona the dwarf itself is running as, the same
            // identity --actor already carries.
            req.extend(verb_args.iter().cloned());
            req.extend(["--for".to_string(), actor.to_string()]);
            if let Some(b) = body {
                req.extend(["--stdin".to_string(), b.to_string()]);
            }
            req.extend(["--actor".to_string(), actor.to_string()]);
        }
        "done" | "blocked" | "superseded-by" | "ack" => {
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
    fn a_checks_build_resolves_to_the_sessions_owned_build_directory() {
        let root = std::path::Path::new("/w/sp-x");
        let owned = std::path::Path::new("/srv/ssd-b/dwarf-build/dir.3");
        assert_eq!(check_target(Some(owned.as_os_str().to_os_string()), root), owned, "the owned directory itself, never a second tree beneath it");
        assert_eq!(check_target(None, root), root.join("target"), "no session target: the worktree's own");
    }

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
    fn a_target_exit_may_name_the_follow_on_bead() {
        let args = ["--short".to_string(), "37 s".into(), "--follow-on".into(), "sp-next1".into()];
        assert_eq!(foreign_bead_reference("submit", "sp-3m1p9", &args), None);
        assert_eq!(foreign_bead_reference("done", "sp-3m1p9", &args), None);
        assert_eq!(foreign_bead_reference("note", "sp-3m1p9", &args), Some("sp-next1"));
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
        assert!(build_request("sp-3m1p9", "submit", &["--tip".into(), "usersupplied".into()], Some("real"), "a").is_err());
    }

    #[test]
    fn build_request_ack_carries_the_answer_and_the_actor() {
        assert_eq!(
            build_request("sp-3m1p9", "ack", &["am-fc37e168a5".into(), "--accept".into()], None, "warden").unwrap(),
            vec!["work", "sp-3m1p9", "ack", "am-fc37e168a5", "--accept", "--actor", "warden"]
        );
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
    fn file_followup_carries_a_stdin_body() {
        let req = build_request_with_body("sp-3m1p9", "file-followup", &["t".into(), "--body-file".into(), "-".into()], None, "builder", Some("## Build\nx")).unwrap();
        assert_eq!(req, vec!["work", "sp-3m1p9", "file-followup", "t", "--body-file", "-", "--for", "builder", "--stdin", "## Build\nx", "--actor", "builder"]);
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
    fn mender_diagnose_and_outcome_read_stdin_with_or_without_a_dash() {
        for args in [v(&["outcome", "/s", "return"]), v(&["outcome", "/s", "return", "-"])] {
            let (a, b) = collect_mender_body(&args, || Ok("why".to_string())).unwrap();
            assert_eq!((a.clone(), b.as_deref()), (v(&["outcome", "/s", "return"]), Some("why")));
            let req = build_request_with_body("-", "mender", &a, None, "mender", b.as_deref()).unwrap();
            assert_eq!(req, v(&["work", "-", "mender", "outcome", "/s", "return", "--stdin", "why", "--actor", "mender"]));
        }
        let (a, b) = collect_mender_body(&v(&["diagnose", "/s"]), || Ok("d".to_string())).unwrap();
        assert_eq!((a, b.as_deref()), (v(&["diagnose", "/s"]), Some("d")));
        let (a, b) = collect_mender_body(&v(&["pack", "/s"]), || panic!("stdin not wanted")).unwrap();
        assert_eq!((a, b), (v(&["pack", "/s"]), None));
    }

    #[test]
    fn build_request_refuses_unknown_verb() {
        assert!(build_request("sp-3m1p9", "close", &[], None, "a").is_err());
    }

    const META: &str = r#"{"workspace_root":"/w","workspace_members":["p#a 0.1.0 (path+file:///w/a)","p#b 0.1.0 (path+file:///w/b)","p#c 0.1.0 (path+file:///w/c)"],"packages":[
      {"id":"p#a 0.1.0 (path+file:///w/a)","name":"a","manifest_path":"/w/a/Cargo.toml","dependencies":[]},
      {"id":"p#b 0.1.0 (path+file:///w/b)","name":"b","manifest_path":"/w/b/Cargo.toml","dependencies":[{"name":"a","path":"/w/a"}]},
      {"id":"p#c 0.1.0 (path+file:///w/c)","name":"c","manifest_path":"/w/c/Cargo.toml","dependencies":[]}]}"#;

    fn raw(paths: &[&str]) -> Vec<u8> {
        paths.iter().map(|p| format!(":100644 100644 aaa bbb M\0{p}\0")).collect::<String>().into_bytes()
    }

    fn repo_clippy_row() -> String {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        habits::config::load_checks(root).unwrap().into_iter().find(|r| r.name == "clippy").expect("a clippy check row").run
    }

    #[test]
    fn the_clippy_check_names_only_a_touched_leaf_and_its_dependents() {
        let scope = affected_scope(&raw(&["a/src/lib.rs"]), META).unwrap();
        assert_eq!(check_argv(&repo_clippy_row(), &scope).unwrap(), v(&["cargo", "clippy", "-p", "a", "-p", "b", "--all-targets", "--", "-D", "warnings"]));
    }

    #[test]
    fn the_clippy_check_falls_back_to_the_workspace_only_on_a_workspace_input() {
        for input in ["Cargo.lock", "Cargo.toml", "rust-toolchain.toml", ".cargo/config.toml"] {
            let scope = affected_scope(&raw(&["c/src/lib.rs", input]), META).unwrap();
            assert_eq!(scope, Affected::Workspace, "{input}");
            assert_eq!(check_argv(&repo_clippy_row(), &scope).unwrap(), v(&["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]));
        }
    }

    #[test]
    fn the_clippy_check_is_skipped_when_the_diff_affects_no_crate() {
        let scope = affected_scope(&raw(&["docs/x.md", "spira/x.sh"]), META).unwrap();
        assert_eq!(scope, Affected::Crates(vec![]));
        assert_eq!(check_argv(&repo_clippy_row(), &scope), None);
        assert_eq!(check_argv("spira-lint --diff x", &scope).unwrap(), v(&["spira-lint", "--diff", "x"]));
    }

    #[test]
    fn submit_does_not_rerun_checks_that_already_passed_at_this_tip() {
        let key = checks_pass_key("t1", "b1", "[[check]]");
        assert!(checks_already_passed(Some(&key), &key, true), "work checks passed at this tip: submit runs nothing again");
        assert!(!checks_already_passed(Some(&key), &key, false), "a dirty tree is not the tip that passed");
        assert!(!checks_already_passed(None, &key, true));
        for other in [checks_pass_key("t2", "b1", "[[check]]"), checks_pass_key("t1", "b2", "[[check]]"), checks_pass_key("t1", "b1", "[[check]]\nx")] {
            assert!(!checks_already_passed(Some(&key), &other, true), "another tip, base or checks file runs them again");
        }
    }

    #[test]
    fn a_cargo_check_waits_for_admission_before_its_budget_starts() {
        let argv = v(&["timeout", "900", "cargo", "clippy", "-p", "a"]);
        assert_eq!(
            admitted(argv.clone(), Some("/r/bin/spira-admit"), "sp-x"),
            v(&["/r/bin/spira-admit", "run", "--pool", "compile", "--who", "sp-x", "--", "timeout", "900", "cargo", "clippy", "-p", "a"])
        );
        assert_eq!(admitted(argv.clone(), Some("sccache"), "sp-x"), argv, "builds that are not admitted are run as they are");
        assert_eq!(admitted(argv.clone(), None, "sp-x"), argv);
        assert_eq!(admission_wait_secs("x\nspira-admit: admitted to compile slot 2 after 244s\n"), Some(244));
        assert_eq!(admission_wait_secs("nothing waited"), None);
    }

    #[test]
    fn a_diff_touching_no_crate_selects_nothing() {
        assert!(crates_selection(&raw(&["docs/x.md"]), META).unwrap().is_empty());
    }
}

/// What `work law [<part> | <part>.<n> | §<part>.<n> | <slug>]` prints, read from the tracked `law/` tree
/// under `root`. `Err` carries the Part list when nothing matches.
pub fn law_view(root: &std::path::Path, arg: Option<&str>) -> Result<String, (String, String)> {
    let law = root.join("law");
    let index = std::fs::read_to_string(law.join("index.md")).map_err(|e| (format!("{}: {e}", law.join("index.md").display()), String::new()))?;
    let Some(arg) = arg else { return Ok(index) };
    let miss = || (format!("no Part or section '{arg}'; the Parts are:"), index.clone());
    let entries = |dir: &std::path::Path| -> Vec<(String, std::path::PathBuf)> {
        let mut v: Vec<_> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| (e.file_name().to_string_lossy().into_owned(), e.path())).collect();
        v.sort();
        v
    };
    let key_of = |name: &str| name.split_once('-').map(|(k, _)| k.to_string());
    let part_dir = |n: &str| entries(&law).into_iter().find(|(name, p)| p.is_dir() && key_of(name).as_deref() == Some(n));
    let read = |p: &std::path::Path| std::fs::read_to_string(p).map_err(|e| (format!("{}: {e}", p.display()), String::new()));
    if arg.chars().all(|c| c.is_ascii_digit()) && !arg.is_empty() {
        let (_, dir) = part_dir(arg).ok_or_else(miss)?;
        return read(&dir.join("index.md"));
    }
    let by_slug = |slug: &str| {
        let want = format!("{slug}.md");
        entries(&law).into_iter().filter(|(_, p)| p.is_dir()).find_map(|(_, dir)| entries(&dir).into_iter().find(|(name, _)| name.split_once('-').is_some_and(|(_, rest)| rest == want)).map(|(_, f)| f))
    };
    if let Some(toc) = std::fs::read_to_string(law.join("toc.toml")).ok().and_then(|t| rule::code::parse_toc(&t).ok()) {
        let pointers = match std::fs::read_to_string(law.join("pointers.toml")) {
            Ok(t) => rule::code::parse_pointers(&t).map_err(|e| (e, String::new()))?,
            Err(_) => rule::code::Pointers::default(),
        };
        return match rule::code::resolve(&toc, &pointers, arg) {
            Ok(r) => {
                let file = by_slug(&r.slug).ok_or_else(miss)?;
                let body = read(&file)?;
                Ok(r.hops.iter().fold(String::new(), |acc, h| acc + h + "\n") + &if r.hops.is_empty() { body } else { format!("\n{body}") })
            }
            Err(rule::code::Miss::Cycle(c)) => Err((format!("pointers form a cycle: {c}"), String::new())),
            Err(rule::code::Miss::Unknown) => Err(miss()),
        };
    }
    if let Some((part, n)) = arg.split_once('.') {
        if !part.is_empty() && !n.is_empty() && arg.chars().all(|c| c.is_ascii_digit() || c == '.') {
            let (_, dir) = part_dir(part).ok_or_else(miss)?;
            let (_, file) = entries(&dir).into_iter().find(|(name, _)| key_of(name).as_deref() == Some(arg)).ok_or_else(miss)?;
            return read(&file);
        }
    }
    by_slug(&format!("law-{}", arg.strip_prefix("law-").unwrap_or(arg))).ok_or_else(miss).and_then(|f| read(&f))
}

#[cfg(test)]
mod law_tests {
    use super::law_view;

    fn fixture() -> testkit::TempDir {
        let d = testkit::TempDir::new("work-law");
        let w = |p: &str, t: &str| {
            let path = d.join("law").join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, t).unwrap();
        };
        w("index.md", "TOP\n");
        w("1-alpha/index.md", "P1\n");
        w("1-alpha/1.1-law-one.md", "S11\n");
        w("2-beta/index.md", "P2\n");
        w("2-beta/2.1-law-three.md", "S21\n");
        w("2-beta/2.10-law-ten.md", "S210\n");
        d
    }

    #[test]
    fn it_prints_the_index_a_part_and_a_section() {
        let d = fixture();
        assert_eq!(law_view(&d, None).unwrap(), "TOP\n");
        assert_eq!(law_view(&d, Some("2")).unwrap(), "P2\n");
        assert_eq!(law_view(&d, Some("2.1")).unwrap(), "S21\n");
        assert_eq!(law_view(&d, Some("2.10")).unwrap(), "S210\n");
        assert_eq!(law_view(&d, Some("law-one")).unwrap(), "S11\n");
        assert_eq!(law_view(&d, Some("three")).unwrap(), "S21\n");
    }

    #[test]
    fn an_unknown_target_returns_the_part_list() {
        let d = fixture();
        for bad in ["3", "2.2", "9.1", "law-nope", "1.1.1"] {
            let (msg, list) = law_view(&d, Some(bad)).unwrap_err();
            assert!(msg.contains(bad), "{msg}");
            assert_eq!(list, "TOP\n", "{bad}");
        }
    }

    #[test]
    fn a_missing_tree_is_an_error_not_an_empty_list() {
        let d = testkit::TempDir::new("work-law-none");
        assert!(law_view(&d, None).unwrap_err().1.is_empty());
    }
}
