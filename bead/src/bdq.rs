//! `bdq` — the harness's one chokepoint for invoking `bd`, and the three create-time safety
//! fences every caller goes through on the way there. Ported from `spira/lib.sh`'s `bdq`,
//! `_bdq_check_repo_label`, `_bdq_check_destructive`, `_bdq_check_schema_delete`, `ghq`,
//! `bdjson`, `json_only` and `json_count` (sp-w3h16, wave 4.14, wave4-decomposition.md row A
//! and row B, safety note (c1)).
//!
//! Pure, argv-in/decision-out logic lives here so it is unit-testable with no database, no
//! subprocess and no filesystem. The actual `bd`/`czar-fence.sh`/`bdsim.py` subprocess work,
//! and the retry loop that drives it, live in `src/bin/bdq.rs` — the same split `lib.rs`/
//! `main.rs` already draws for `bead file`/`bead lint`.

use std::collections::BTreeMap;

use spira_config::repos::Registry;

// =========================================================================================
// Argv parsing shared shape. Each of the three bash fence functions parses its own `"$@"`
// with a slightly different flag set, so each gets its own small state machine below rather
// than one "generic" parser — matching flags precisely is the whole point of a parity port,
// and the three loops genuinely disagree about which flags they recognise (schema-delete's
// loop has no `--labels`/`-l` case at all, unlike destructive's).
// =========================================================================================

/// `_bdq_check_repo_label`'s own loop: the LAST of `--labels <v>` / `-l <v>` / `--labels=<v>`
/// anywhere in argv wins (no "create" skip, no title/description tracking at all).
fn last_labels_value(args: &[String]) -> String {
    let mut labels = String::new();
    let mut next_is_labels = false;
    for arg in args {
        if next_is_labels {
            labels = arg.clone();
            next_is_labels = false;
            continue;
        }
        if arg == "--labels" || arg == "-l" {
            next_is_labels = true;
        } else if let Some(v) = arg.strip_prefix("--labels=") {
            labels = v.to_string();
        }
    }
    labels
}

/// Title/description/labels extracted by `_bdq_check_destructive`'s loop: `--title`/
/// `--title=`/a bare positional all set `title` (first non-flag arg after the leading
/// `create` token is skipped; the next sets `title`, and only if nothing has set it yet);
/// `-d`/`--description`/`--description=` set `desc`; `--labels`/`-l`/`--labels=` set
/// `labels` (last one wins, matching bash's unconditional overwrite in both branches).
fn destructive_fields(args: &[String]) -> (String, String, String) {
    let mut labels = String::new();
    let mut title = String::new();
    let mut desc = String::new();
    let mut next: &str = "";
    let mut saw_create = false;
    let mut positioned = false;
    for arg in args {
        if !next.is_empty() {
            match next {
                "labels" => labels = arg.clone(),
                "title" => {
                    title = arg.clone();
                    positioned = true;
                }
                "description" => desc = arg.clone(),
                _ => {}
            }
            next = "";
            continue;
        }
        if arg == "--labels" || arg == "-l" {
            next = "labels";
        } else if let Some(v) = arg.strip_prefix("--labels=") {
            labels = v.to_string();
        } else if arg == "--title" {
            next = "title";
        } else if let Some(v) = arg.strip_prefix("--title=") {
            title = v.to_string();
            positioned = true;
        } else if arg == "-d" || arg == "--description" {
            next = "description";
        } else if let Some(v) = arg.strip_prefix("--description=") {
            desc = v.to_string();
        } else if arg.starts_with('-') {
            // recognised-but-unhandled or unknown flag: skip, matching bash's `-*) ;;`.
        } else if !saw_create {
            saw_create = true; // this positional is the literal "create"
        } else if !positioned {
            title = arg.clone();
            positioned = true;
        }
    }
    (title, desc, labels)
}

/// Title/description extracted by `_bdq_check_schema_delete`'s loop — identical shape to
/// [`destructive_fields`] but with NO `--labels`/`-l` case at all: a `--labels` token there
/// falls into the bare `-*)` skip branch instead of consuming the next arg as a value.
fn schema_delete_fields(args: &[String]) -> (String, String) {
    let mut title = String::new();
    let mut desc = String::new();
    let mut next: &str = "";
    let mut saw_create = false;
    let mut positioned = false;
    for arg in args {
        if !next.is_empty() {
            match next {
                "title" => {
                    title = arg.clone();
                    positioned = true;
                }
                "description" => desc = arg.clone(),
                _ => {}
            }
            next = "";
            continue;
        }
        if arg == "--title" {
            next = "title";
        } else if let Some(v) = arg.strip_prefix("--title=") {
            title = v.to_string();
            positioned = true;
        } else if arg == "-d" || arg == "--description" {
            next = "description";
        } else if let Some(v) = arg.strip_prefix("--description=") {
            desc = v.to_string();
        } else if arg.starts_with('-') {
            // includes --labels/-l: unrecognised here, skipped without consuming a value.
        } else if !saw_create {
            saw_create = true;
        } else if !positioned {
            title = arg.clone();
            positioned = true;
        }
    }
    (title, desc)
}

// =========================================================================================
// The three create-time fences.
// =========================================================================================

/// The `repo:<val>` token out of a comma-joined labels string, same extraction
/// `_bdq_check_repo_label` and incident.sh's own (ported) check both need: the LAST
/// `repo:`-prefixed token wins when more than one is present in the same labels string
/// (matching `last_labels_value`'s "last --labels flag wins" shape one level up, for the
/// argv-based caller; a plain already-joined labels string, as incident's caller already
/// has one, has at most one `repo:` token in practice, so "first match" and "last match"
/// agree there).
fn repo_label_value(labels: &str) -> Option<&str> {
    labels.split(',').find_map(|t| t.strip_prefix("repo:"))
}

/// Whether a `repo:<val>` claim is acceptable: no claim at all (`val` empty — the same
/// `[ -z "$repo_val" ]` bash's own fence short-circuits on), the home repo, or a name the
/// map carries. Shared core of `check_repo_label` (bdq's own argv-parsing entry point) and
/// incident's `invalid_repo_label` (decide.rs — `incident.sh` never goes through `bdq`'s argv
/// at all, so it calls this directly with its own already-known `home_repo`/`known_repos`).
/// One table of "what is a valid repo claim" for both, rather than the empty-value edge case
/// living in one copy and not the other.
pub fn repo_label_allowed(repo_val: &str, home_repo: &str, known: &[String]) -> bool {
    repo_val.is_empty() || repo_val == home_repo || known.iter().any(|n| n == repo_val)
}

/// `_bdq_check_repo_label`. `registry` is this process's repo registry (built from
/// `$SPIRA_REPO_MAP`/`$SPIRA_HOME`/`$SPIRA_REPO`/`$SPIRA_REPO_DERIVED`/`$SPIRA_HOME_REPO`,
/// read by the caller — see `src/bin/bdq.rs::build_registry`). Returns the refusal message
/// (already newline-terminated, ready for stderr), or `None` when the call is allowed.
pub fn check_repo_label(args: &[String], registry: &Registry) -> Option<String> {
    let labels = last_labels_value(args);
    if labels.is_empty() {
        return None;
    }
    let Some(repo_val) = repo_label_value(&labels) else { return None };
    let mut names = registry.names();
    if repo_label_allowed(repo_val, registry.home_repo(), &names) {
        return None;
    }
    names.sort();
    let valid = if names.is_empty() { "<map not found>".to_string() } else { names.join(" ") };
    Some(format!("spira: repo:{repo_val} is not in the repo map; valid keys: {valid}\n"))
}

/// The destructive-vocabulary regexes, shared by `check_destructive` (bdq's own
/// argv-parsing entry point) and incident's `destructive_phrase` (decide.rs — `incident.sh`
/// never goes through `bdq`'s argv either, so it builds its own "<title> <desc>" text and
/// calls this directly). One pattern table for both, so a tightened regex here reaches every
/// caller and a narrower hand-rolled substring check in a second copy cannot quietly accept
/// text the real fence would have refused (or refuse text the real fence would have let
/// through — `\bdaemon-reload\b` not matching "mydaemon-reloaded" is the test that pins
/// this). Case-insensitive; returns the matched phrase in its ORIGINAL case, same as bash's
/// `grep -io` capture.
pub fn destructive_match(text: &str) -> Option<String> {
    const PATTERNS: &[&str] = &[
        r"world\.sh +stop",
        r"spira-world +down",
        r"systemd/install\.sh",
        r"\bdaemon-reload\b",
        r"systemctl +(stop|restart) +spira-",
        r"schema +migrat",
        r"world +stopped",
    ];
    for p in PATTERNS {
        let re = regex::RegexBuilder::new(p).case_insensitive(true).build().expect("static pattern");
        if let Some(m) = re.find(text) {
            return Some(m.as_str().to_string());
        }
    }
    None
}

/// `_bdq_check_destructive`. `ask_label` is `$SPIRA_ASK_LABEL` (conf.sh always exports it;
/// the bash original hard-refuses to run at all with it unset — callers here are expected to
/// have already resolved it the same way, see `src/bin/bdq.rs`).
pub fn check_destructive(args: &[String], ask_label: &str) -> Option<String> {
    let (title, desc, labels) = destructive_fields(args);
    if labels.split(',').any(|l| l == ask_label) {
        return None;
    }
    // Mirrors bash's `[ -z "${text# }" ]`: text is always "<title> <desc>", which strips to
    // empty iff both are empty — i.e. iff text is exactly one space.
    let text = format!("{title} {desc}");
    if text == " " {
        return None;
    }
    destructive_match(&text).map(|phrase| {
        format!(
            "spira: bead contains \"{phrase}\" — procedures that halt the harness require needs-ryan.\nAdd needs-ryan to --labels, or reword to remove the destructive step.\n", // literal-ok: fixture/fallback
        )
    })
}

/// `label add <ask>` and `update --add-label <ask>` skip the create-time shape check, so
/// they are refused outright: an ask is created whole, never labelled on afterwards.
pub fn check_ask_label_write(args: &[String], ask_label: &str) -> Option<String> {
    if ask_label.is_empty() {
        return None;
    }
    let writes = match args.first().map(String::as_str) {
        Some("label") => args.get(1).map(String::as_str) == Some("add") && args.iter().skip(3).any(|a| a == ask_label),
        Some("update") => args.windows(2).any(|w| matches!(w[0].as_str(), "--add-label" | "--set-labels") && w[1].split(',').any(|l| l == ask_label)),
        _ => false,
    };
    if !writes {
        return None;
    }
    Some(format!(
        "spira: refusing to add {ask_label} to an existing bead — it is the operator's decision queue and an ask is created whole.\n\
         Exit: post the decision with `work ask` (question, default, class), or label the bead overseer (and the no-loop label to stop dispatch) so the Concierge or Ops works it.\n" // literal-ok: fixture/fallback
    ))
}

/// The schema-migrations-DELETE regex, shared the same way [`destructive_match`] is.
pub fn schema_delete_match(text: &str) -> bool {
    static PATTERN: &str = r"delete[[:space:]]+from[[:space:]]+schema_migrations";
    let re = regex::RegexBuilder::new(PATTERN).case_insensitive(true).build().expect("static pattern");
    re.is_match(text)
}

/// `_bdq_check_schema_delete`. Not bypassed by `needs-ryan` (sp-1khst) — this check takes no
/// `ask_label` at all.
pub fn check_schema_delete(args: &[String]) -> Option<String> {
    let (title, desc) = schema_delete_fields(args);
    let text = format!("{title} {desc}");
    if text == " " {
        return None;
    }
    if schema_delete_match(&text) {
        return Some(
            "spira: bead contains \"DELETE FROM schema_migrations\" — this SQL is refused\n\
             even with an operator ask because it was escalated and approved three times while wrong.\n\
             Run `bd migrate schema` and include its output in the escalation instead.\n\
             The correct response to a real mismatch is rebuilding bd (see bd-pin.sh),\n\
             not deleting migration rows from the database.\n"
                .to_string(),
        );
    }
    None
}

/// `[ "${1:-}" = create ]` — whether this call is a `bd create`, which is when all three
/// fences above run.
pub fn is_create(args: &[String]) -> bool {
    matches!(args.first().map(String::as_str), Some("create" | "create-form" | "q"))
}

/// A bead created already closed (`--status closed`) is never claimed, so it needs no
/// lifecycle row. Such a bead is an insight — a non-work record, filed closed — so the value
/// is read as one (`spira_config::nonwork`, sp-mve9i).
pub fn creates_closed(args: &[String]) -> bool {
    use spira_config::nonwork::{is_closed, Kind};
    let flag = "--status";
    let value = args
        .windows(2)
        .find(|w| w[0] == flag)
        .map(|w| w[1].as_str())
        .or_else(|| args.iter().find_map(|a| a.strip_prefix(flag).and_then(|v| v.strip_prefix('='))));
    value.is_some_and(|v| is_closed(Kind::Insight, v))
}

// =========================================================================================
// The czar fence's dispatch decision (the fence itself is `czar-fence.sh`, a subprocess —
// see `src/bin/bdq.rs`). Pure: "should the caller shell out to czar-fence.sh at all, and
// with which class."
// =========================================================================================

/// Whether `bdq`'s czar-fence dispatch applies to this call, and the class to pass
/// `czar-fence.sh` if so. Mirrors the bash's `case "${1:-}" in reopen) ...; update|close)
/// ...; esac`, gated on `fayth == "czar"` and a non-empty class.
pub fn czar_fence_class<'a>(
    fayth: &str,
    czar_class: &'a str,
    czar_trigger_bead: &str,
    args: &[String],
) -> Option<&'a str> {
    if fayth != "czar" || czar_class.is_empty() {
        return None;
    }
    match args.first().map(String::as_str) {
        Some("reopen") => Some(czar_class),
        Some("update") | Some("close") => {
            let second = args.get(1).map(String::as_str).unwrap_or("");
            let trigger = if czar_trigger_bead.is_empty() { "__none__" } else { czar_trigger_bead };
            if second != trigger {
                Some(czar_class)
            } else {
                None
            }
        }
        _ => None,
    }
}

// =========================================================================================
// bdjson / json_only / json_count
// =========================================================================================

/// `json_only`: `sed -n '/^[[{]/,$p'` — find the first line that starts (at column 1, no
/// leading-whitespace tolerance) with `[` or `{`, and keep that line and everything after it,
/// verbatim. Everything before it (a warning banner `bd --json` printed on stdout) is
/// dropped. No match at all -> empty string, matching `sed -n` printing nothing.
pub fn json_only(input: &str) -> &str {
    let mut offset = 0usize;
    for line in input.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        if body.starts_with('[') || body.starts_with('{') {
            return &input[offset..];
        }
        offset += line.len();
    }
    ""
}

/// `json_count`: parse stdin as JSON; an array's length, 1 for any other value, 0 for
/// anything that fails to parse (including empty input) — never a hard error, matching the
/// bash's `d = []` fallback on any python exception.
pub fn json_count(input: &str) -> u64 {
    match serde_json::from_str::<serde_json::Value>(input) {
        Ok(serde_json::Value::Array(a)) => a.len() as u64,
        Ok(_) => 1,
        Err(_) => 0,
    }
}

// =========================================================================================
// Retry-loop decision (the subprocess calls themselves are in src/bin/bdq.rs).
// =========================================================================================

/// Whether the retry loop should attempt again: bash's
/// `[ "$_bdq_rc" -eq 0 ] || [ "$_bdq_try" -ge "$_bdq_tries" ] || ! grep -q "invalid connection" "$_bdq_err"`
/// decides when to BREAK; this is the negation (when to keep going), spelled out positively
/// so the caller's loop reads as "retry while this is true".
pub fn should_retry(rc: i32, try_n: u32, max_tries: u32, stderr_has_invalid_connection: bool) -> bool {
    rc != 0 && try_n < max_tries.max(1) && stderr_has_invalid_connection
}

const READ_VERBS: &[&str] = &["show", "list", "count", "ready", "blocked", "query", "search", "stats", "status", "children", "dep", "graph", "find", "export"];

/// A read may be retried on any dropped connection. A write only when bd never got as far as
/// the statement ("failed to open database"), so it cannot have committed.
pub fn retryable(args: &[String], stderr: &str) -> bool {
    if !stderr.contains("invalid connection") {
        return false;
    }
    let verb = args.iter().find(|a| !a.starts_with('-')).map(String::as_str).unwrap_or("");
    READ_VERBS.contains(&verb) || stderr.contains("failed to open database")
}

/// Sleep before attempt `try_n + 1`: base, then doubling.
pub fn backoff_ms(base_ms: u64, try_n: u32) -> u64 {
    base_ms.saturating_mul(1u64 << (try_n.saturating_sub(1)).min(10))
}

/// A repo registry built the same way `spira-config`'s own CLI builds one (see
/// `spira-config/src/main.rs::repo_registry`): `$SPIRA_REPO_MAP`/`$SPIRA_HOME`/`$SPIRA_REPO`/
/// `$SPIRA_REPO_DERIVED`/`$SPIRA_HOME_REPO` read straight out of an env map the caller
/// supplies — never self-located beyond that. Exposed here (not just duplicated in
/// `src/bin/bdq.rs`) so a test can build one without going through `std::env`.
pub fn registry_from_env(env: &BTreeMap<String, String>, home: &std::path::Path) -> Registry {
    let map_text = env
        .get("SPIRA_REPO_MAP")
        .filter(|p| !p.is_empty())
        .and_then(|p| std::fs::read_to_string(p).ok());
    Registry::new(map_text.as_deref(), env, home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn s(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    // -- check_repo_label -------------------------------------------------------------------

    #[test]
    fn repo_label_allows_home_repo_and_known_repos() {
        let e = env(&[("SPIRA_HOME_REPO", "spira")]);
        let reg = registry_from_env(&e, Path::new("/x"));
        assert_eq!(check_repo_label(&s(&["create", "title", "--labels", "plan,repo:spira"]), &reg), None);
    }

    #[test]
    fn repo_label_refuses_unknown_repo_and_names_valid_keys() {
        let map = "spira | /srv/spira | push | origin/main | |\nwidget | /srv/widget | pr | origin/main | |\n";
        let reg = Registry::new(Some(map), &env(&[("SPIRA_HOME_REPO", "homerepo")]), Path::new("/x"));
        let msg = check_repo_label(&s(&["create", "title", "--labels", "plan,repo:nope"]), &reg);
        let msg = msg.expect("refused");
        assert!(msg.contains("repo:nope"), "{msg}");
        assert!(msg.contains("spira"), "{msg}");
        assert!(msg.contains("widget"), "{msg}");
    }

    #[test]
    fn repo_label_no_repo_label_at_all_is_allowed() {
        let reg = registry_from_env(&env(&[]), Path::new("/x"));
        assert_eq!(check_repo_label(&s(&["create", "title", "--labels", "plan"]), &reg), None);
    }

    #[test]
    fn repo_label_last_labels_flag_wins() {
        // Two --labels occurrences: the second overwrites the first, matching bash's
        // unconditional overwrite with no break.
        let map = "spira | /srv/spira | push | origin/main | |\n";
        let reg = Registry::new(Some(map), &env(&[("SPIRA_HOME_REPO", "homerepo")]), Path::new("/x"));
        assert_eq!(
            check_repo_label(&s(&["create", "title", "--labels", "repo:nope", "--labels", "repo:spira"]), &reg),
            None
        );
    }

    // -- check_destructive ------------------------------------------------------------------

    #[test]
    fn destructive_matches_and_is_bypassed_by_ask_label() {
        assert!(check_destructive(&s(&["create", "needs the world stopped"]), "needs-ryan").is_some()); // literal-ok: fixture/fallback
        assert_eq!(
            check_destructive(&s(&["create", "needs the world stopped", "--labels", "needs-ryan"]), "needs-ryan"), // literal-ok: fixture/fallback
            None
        );
    }

    #[test]
    fn destructive_matched_text_preserves_original_case() {
        let msg = check_destructive(&s(&["create", "Needs The WORLD STOPPED now"]), "needs-ryan").unwrap(); // literal-ok: fixture/fallback
        assert!(msg.contains("WORLD STOPPED"), "{msg}");
    }

    #[test]
    fn destructive_systemctl_alternation_matches_stop_and_restart() {
        assert!(check_destructive(&s(&["create", "run systemctl restart spira-gate"]), "needs-ryan").is_some()); // literal-ok: fixture/fallback
        assert!(check_destructive(&s(&["create", "run systemctl stop spira-gate"]), "needs-ryan").is_some()); // literal-ok: fixture/fallback
    }

    #[test]
    fn destructive_ordinary_bead_is_allowed() {
        assert_eq!(check_destructive(&s(&["create", "ordinary title", "-d", "ordinary body"]), "needs-ryan"), None); // literal-ok: fixture/fallback
    }

    #[test]
    fn destructive_daemon_reload_is_word_bounded() {
        assert!(check_destructive(&s(&["create", "run daemon-reload now"]), "needs-ryan").is_some()); // literal-ok: fixture/fallback
        assert_eq!(check_destructive(&s(&["create", "mydaemon-reloaded"]), "needs-ryan"), None); // literal-ok: fixture/fallback
    }

    #[test]
    fn destructive_description_flag_is_read() {
        assert!(check_destructive(&s(&["create", "clean title", "-d", "world.sh stop please"]), "needs-ryan").is_some()); // literal-ok: fixture/fallback
    }

    // -- check_schema_delete -----------------------------------------------------------------

    #[test]
    fn ask_label_cannot_be_added_to_an_existing_bead() {
        let a = "needs-ryan"; // literal-ok: fixture/fallback
        assert!(check_ask_label_write(&s(&["label", "add", "sp-1", a]), a).unwrap().contains("Exit:"));
        assert!(check_ask_label_write(&s(&["update", "sp-1", "--add-label", a]), a).is_some());
        assert!(check_ask_label_write(&s(&["update", "sp-1", "--add-label", "x,needs-ryan"]), a).is_some()); // literal-ok: fixture/fallback
        assert_eq!(check_ask_label_write(&s(&["label", "add", "sp-1", "overseer"]), a), None);
        assert_eq!(check_ask_label_write(&s(&["label", "remove", "sp-1", a]), a), None);
        assert_eq!(check_ask_label_write(&s(&["update", "sp-1", "--remove-label", a]), a), None);
        assert_eq!(check_ask_label_write(&s(&["label", "add", "sp-1", a]), ""), None);
    }

    #[test]
    fn schema_delete_is_detected_regardless_of_case_or_spacing() {
        assert!(check_schema_delete(&s(&["create", "DELETE   FROM schema_migrations"])).is_some());
        assert!(check_schema_delete(&s(&["create", "clean", "-d", "please delete from schema_migrations now"]))
            .is_some());
        assert_eq!(check_schema_delete(&s(&["create", "delete from somewhere_else"])), None);
    }

    #[test]
    fn schema_delete_labels_flag_is_not_a_bypass_and_is_not_misread_as_title() {
        // --labels is unrecognised by this parser; its value must not leak into title.
        let r = check_schema_delete(&s(&[
            "create",
            "clean title",
            "--labels",
            "needs-ryan", // literal-ok: fixture/fallback
            "-d",
            "DELETE FROM schema_migrations",
        ]));
        assert!(r.is_some());
    }

    // -- is_create ----------------------------------------------------------------------------

    #[test]
    fn creates_closed_reads_the_status_flag() {
        assert!(creates_closed(&s(&["create", "t", "--status", "closed"])));
        assert!(creates_closed(&s(&["create", "t", "--status=closed"])));
        assert!(!creates_closed(&s(&["create", "t", "--status", "open"])));
        assert!(!creates_closed(&s(&["create", "closed"])));
    }

    #[test]
    fn is_create_only_true_for_the_create_verb() {
        assert!(is_create(&s(&["create", "x"])));
        assert!(is_create(&s(&["q", "x"])));
        assert!(is_create(&s(&["create-form"])));
        assert!(!is_create(&s(&["update", "sp-a"])));
        assert!(!is_create(&s(&[])));
    }

    // -- czar fence dispatch ------------------------------------------------------------------

    #[test]
    fn czar_fence_fires_on_reopen_when_czar_with_class() {
        assert_eq!(czar_fence_class("czar", "deadlock", "", &s(&["reopen", "sp-a"])), Some("deadlock"));
    }

    #[test]
    fn czar_fence_skips_non_czar_or_empty_class() {
        assert_eq!(czar_fence_class("builder", "deadlock", "", &s(&["reopen", "sp-a"])), None);
        assert_eq!(czar_fence_class("czar", "", "", &s(&["reopen", "sp-a"])), None);
    }

    #[test]
    fn czar_fence_update_close_skip_the_trigger_bead_itself() {
        assert_eq!(czar_fence_class("czar", "deadlock", "sp-trigger", &s(&["update", "sp-trigger"])), None);
        assert_eq!(
            czar_fence_class("czar", "deadlock", "sp-trigger", &s(&["update", "sp-other"])),
            Some("deadlock")
        );
        assert_eq!(czar_fence_class("czar", "deadlock", "", &s(&["close", "sp-a"])), Some("deadlock"));
    }

    #[test]
    fn czar_fence_ignores_other_verbs() {
        assert_eq!(czar_fence_class("czar", "deadlock", "", &s(&["create", "x"])), None);
        assert_eq!(czar_fence_class("czar", "deadlock", "", &s(&["list"])), None);
    }

    // -- json_only ----------------------------------------------------------------------------

    #[test]
    fn json_only_drops_a_warning_banner_before_the_json() {
        assert_eq!(json_only("warning: schema drift\n[{\"a\":1}]\n"), "[{\"a\":1}]\n");
        assert_eq!(json_only("{\"a\":1}\n"), "{\"a\":1}\n");
    }

    #[test]
    fn json_only_requires_column_one_no_leading_whitespace_tolerance() {
        // Matches sed's `^[[{]` exactly: a line that starts with whitespace before the
        // bracket is NOT a match, same as the bash original.
        assert_eq!(json_only("  [1]\n"), "");
    }

    #[test]
    fn json_only_no_json_line_is_empty() {
        assert_eq!(json_only("just a warning\nand another line\n"), "");
    }

    #[test]
    fn json_only_keeps_a_trailing_non_json_line_too() {
        // sed prints from the first match to the end of input, unconditionally.
        assert_eq!(json_only("[1]\nnot json\n"), "[1]\nnot json\n");
    }

    // -- json_count ---------------------------------------------------------------------------

    #[test]
    fn json_count_array_length() {
        assert_eq!(json_count("[1,2,3]"), 3);
        assert_eq!(json_count("[]"), 0);
    }

    #[test]
    fn json_count_object_is_one() {
        assert_eq!(json_count("{\"a\":1}"), 1);
    }

    #[test]
    fn json_count_unparseable_is_zero() {
        assert_eq!(json_count(""), 0);
        assert_eq!(json_count("not json"), 0);
    }

    // -- should_retry -------------------------------------------------------------------------

    #[test]
    fn should_retry_only_on_invalid_connection_within_budget() {
        assert!(should_retry(1, 1, 2, true));
        assert!(!should_retry(0, 1, 2, true), "success never retries");
        assert!(!should_retry(1, 2, 2, true), "budget exhausted");
        assert!(!should_retry(1, 1, 2, false), "a different error never retries");
    }

    #[test]
    fn retryable_reads_on_any_drop_writes_only_before_the_statement() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(retryable(&a(&["show", "x"]), "Error: invalid connection"));
        assert!(retryable(&a(&["--json", "count"]), "Error: invalid connection"));
        assert!(!retryable(&a(&["close", "x"]), "Error: invalid connection"));
        assert!(retryable(&a(&["close", "x"]), "failed to open database: invalid connection"));
        assert!(!retryable(&a(&["show", "x"]), "bead not found"));
    }

    #[test]
    fn backoff_doubles() {
        assert_eq!(backoff_ms(1000, 1), 1000);
        assert_eq!(backoff_ms(1000, 2), 2000);
    }

    #[test]
    fn should_retry_zero_retries_configured_still_allows_one_attempt() {
        // bash: tries="0" from env; try=1 after the first attempt, "1 -ge 0" is true -> break.
        // One attempt always happens; this function only governs whether a SECOND is tried.
        assert!(!should_retry(1, 1, 0, true));
    }
}
