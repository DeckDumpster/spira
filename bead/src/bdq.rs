//! Pure argv-in, decision-out classifiers over bd's argv: what the write guards
//! ([`crate::guard`]) judge, and the small filters other crates share. No store, no
//! subprocess, no filesystem.

use spira_config::repos::Registry;

// =========================================================================================
// Argv parsing shared shape. Each of the three bash fence functions parses its own `"$@"`
// with a slightly different flag set, so each gets its own small state machine below rather
// than one "generic" parser — matching flags precisely is the whole point of a parity port,
// and the three loops genuinely disagree about which flags they recognise (schema-delete's
// loop has no `--labels`/`-l` case at all, unlike destructive's).
// =========================================================================================

/// Title/description/labels extracted by the ask-shape check's loop: `--title`/
/// `--title=`/a bare positional all set `title` (first non-flag arg after the leading
/// `create` token is skipped; the next sets `title`, and only if nothing has set it yet);
/// `-d`/`--description`/`--description=` set `desc`; `--labels`/`-l`/`--labels=` set
/// `labels` (last one wins, matching bash's unconditional overwrite in both branches).
fn ask_fields(args: &[String]) -> (String, String, String) {
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

/// A create's title, description argument and labels, as the ask-shape fence reads them.
pub fn create_fields(args: &[String]) -> (String, String, String) {
    ask_fields(args)
}

/// Title/description extracted by `_bdq_check_schema_delete`'s loop — identical shape to
/// [`ask_fields`] but with NO `--labels`/`-l` case at all: a `--labels` token there
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

/// The first `repo:<val>` token out of a comma-joined labels string.
fn repo_label_value(labels: &str) -> Option<&str> {
    labels.split(',').find_map(|t| t.trim().strip_prefix("repo:"))
}

/// Every `-l`/`--labels`/`--labels=` value in argv, joined with commas: bd accepts the flag
/// more than once, and a `repo:` label in any of them is the bead's.
fn all_labels_value(args: &[String]) -> String {
    let mut values: Vec<&str> = Vec::new();
    let mut next_is_labels = false;
    for arg in args {
        if next_is_labels {
            values.push(arg);
            next_is_labels = false;
        } else if arg == "--labels" || arg == "-l" {
            next_is_labels = true;
        } else if let Some(v) = arg.strip_prefix("--labels=") {
            values.push(v);
        }
    }
    values.join(",")
}

/// The repo a create's `--labels` name, for the lifecycle row it owes.
pub fn created_repo(args: &[String]) -> Option<String> {
    repo_label_value(&all_labels_value(args)).map(str::to_string)
}

/// Whether a `repo:<val>` claim is acceptable: no claim at all (`val` empty — the same
/// `[ -z "$repo_val" ]` bash's own fence short-circuits on), the home repo, or a name the
/// map carries. Shared core of `check_repo_label` (the argv-parsing entry point) and
/// incident's `invalid_repo_label` (decide.rs), which holds no argv and so calls this directly
/// with its own already-known `home_repo`/`known_repos`.
/// One table of "what is a valid repo claim" for both, rather than the empty-value edge case
/// living in one copy and not the other.
pub fn repo_label_allowed(repo_val: &str, home_repo: &str, known: &[String]) -> bool {
    repo_val.is_empty() || repo_val == home_repo || known.iter().any(|n| n == repo_val)
}

/// The repo-label fence over the caller's repo registry. Returns the refusal message
/// (already newline-terminated, ready for stderr), or `None` when the call is allowed.
pub fn check_repo_label(args: &[String], registry: &Registry) -> Option<String> {
    let labels = all_labels_value(args);
    let repo_val = repo_label_value(&labels)?;
    let mut names = registry.names();
    if repo_label_allowed(repo_val, registry.home_repo(), &names) {
        return None;
    }
    names.sort();
    let valid = if names.is_empty() { "<map not found>".to_string() } else { names.join(" ") };
    Some(format!("spira: repo:{repo_val} is not in the repo map; valid keys: {valid}\n"))
}

/// A bead's state is its lifecycle row's (sp-6oimlm): bdq refuses every verb that writes bd's
/// status — `close`, `reopen`, `defer`, `undefer`, and `update` with `--status`/`-s`/`--defer` —
/// so the store can never move on its own and drift from the row. On 2026-10-07 33 beads sat
/// deferred in bd while their rows read READY, and every dwarf called onto one exited in seconds.
/// The exit is the machine's own door; `SPIRA_BDQ_STATE_WRITE=<reason>` overrides, and the
/// caller logs it.
pub fn check_state_verb(args: &[String]) -> Option<&'static str> {
    let verb = args.first().map(String::as_str)?;
    let writes = match verb {
        "close" | "reopen" | "defer" | "undefer" => true,
        "update" => args.iter().skip(1).any(|a| {
            matches!(a.as_str(), "--status" | "-s" | "--defer") || a.starts_with("--status=") || a.starts_with("--defer=")
        }),
        _ => false,
    };
    if !writes {
        return None;
    }
    Some(match verb {
        "close" => "spira-lc close <id> --reason-file - (or `work close-other` for a session)",
        "reopen" | "undefer" => "spira-lc reopen <id> <cause> <actor> (or `work reopen <id> --evidence ...`)",
        "defer" => "spira-lc hold <id> wait <reason> (a timed snooze is a wait hold on the row)",
        _ => "the lifecycle machine's own verb (spira-lc close / reopen / hold); bd's status is not a bead's state",
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

/// The ask label belongs on a decision: a question, a stated default and one of the four
/// operator classes. Anything else is worked by the overseer/ops queue without it.
pub fn check_ask_shape(args: &[String], ask_label: &str) -> Option<String> {
    let (title, desc, labels) = ask_fields(args);
    if ask_label.is_empty() || !labels.split(',').any(|l| l == ask_label) {
        return None;
    }
    let has_question = title.contains('?') || desc.contains("## Question");
    let has_default = desc.lines().any(|l| l.trim_start().to_ascii_lowercase().starts_with("default:") && l.trim().len() > "default:".len());
    let has_class = desc.lines().any(|l| {
        let l = l.trim().to_ascii_lowercase();
        l.strip_prefix("class:").is_some_and(|c| ["permissions", "policy", "destructive", "design"].contains(&c.trim()))
    });
    if has_question && has_default && has_class {
        return None;
    }
    Some(format!(
        "spira: refusing label {ask_label} — it is the operator's decision queue and this bead states no complete decision (question: {has_question}, default: {has_default}, class: {has_class}).\n\
         Exit: put a question (a '?' in the title or a '## Question' section), a 'Default: <what you would do>' line and a 'Class: permissions|policy|destructive|design' line in --description,\n\
         or drop {ask_label} and label it overseer so the Concierge or Ops works it.\n"
    ))
}

/// The schema-migrations-DELETE regex, kept as a pure matcher.
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

/// Whether this call creates beads by any verb: `create` and its kin, `import`, and the
/// `mol` verbs that spawn issues from a proto. Each must leave a lifecycle row per bead.
pub fn creates_beads(args: &[String]) -> bool {
    if args.iter().any(|a| a == "--dry-run") {
        return false;
    }
    let verb = |i: usize| args.get(i).map(String::as_str);
    match verb(0) {
        Some("import") => true,
        Some("mol") => match verb(1) {
            Some("pour" | "bond") => true,
            Some("wisp") => !matches!(verb(2), Some("list" | "gc")),
            _ => false,
        },
        _ => is_create(args),
    }
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

/// A bead created with a `--type` that never carries a lifecycle row (`nonwork::NO_ROW_TYPES`):
/// an alert or write probe is an `event`, and a row would make it claimable and strand-visible.
pub fn creates_rowless_type(args: &[String]) -> bool {
    let ty = args
        .windows(2)
        .find(|w| w[0] == "--type" || w[0] == "-t")
        .map(|w| w[1].as_str())
        .or_else(|| args.iter().find_map(|a| a.strip_prefix("--type=")));
    !spira_config::nonwork::is_work_type(ty)
}

// =========================================================================================
// The czar fence's dispatch decision: whether the fence applies to a call at all, and with
// which class. The fence itself is `crate::guard::czar`.
// =========================================================================================

/// Whether the czar-fence dispatch applies to this call, and the class to pass
/// `czar-fence.sh` if so. Mirrors the bash's `case "${1:-}" in reopen) ...; update|close)
/// ...; esac`, gated on `rune == "czar"` and a non-empty class.
pub fn czar_fence_class<'a>(
    rune: &str,
    czar_class: &'a str,
    czar_trigger_bead: &str,
    args: &[String],
) -> Option<&'a str> {
    if rune != "czar" || czar_class.is_empty() {
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

/// `date -u +%Y-%m-%dT%H:%M:%S.%NZ` of `t`.
pub fn utc_stamp_nanos(t: std::time::SystemTime) -> String {
    let d = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() % 86_400;
    format!("{}T{:02}:{:02}:{:02}.{:09}Z", spira_otel::utc_date(t), secs / 3600, secs / 60 % 60, secs % 60, d.subsec_nanos())
}

// =========================================================================================
// json_only
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

// =========================================================================================
// Retry-loop decision
// =========================================================================================

/// Whether the retry loop should attempt again: bash's
/// `[ "$_bdq_rc" -eq 0 ] || [ "$_bdq_try" -ge "$_bdq_tries" ] || ! grep -q "invalid connection" "$_bdq_err"`
/// decides when to BREAK; this is the negation (when to keep going), spelled out positively
/// so the caller's loop reads as "retry while this is true".
pub fn should_retry(rc: i32, try_n: u32, max_tries: u32, stderr_has_invalid_connection: bool) -> bool {
    rc != 0 && try_n < max_tries.max(1) && stderr_has_invalid_connection
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::Path;

    #[test_id(t93407l)]
    #[test]
    fn utc_stamp_nanos_matches_date_formatting() {
        let t = std::time::UNIX_EPOCH + std::time::Duration::new(1_762_200_000, 5);
        assert_eq!(utc_stamp_nanos(t), "2025-11-03T20:00:00.000000005Z");
    }

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn registry_from_env(env: &BTreeMap<String, String>, home: &Path) -> Registry {
        Registry::new(None, env, home)
    }

    fn s(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    // -- check_repo_label -------------------------------------------------------------------

    #[test_id(tf4wldc)]
    #[test]
    fn every_state_verb_is_refused_and_names_the_machines_door() {
        for argv in [&["close", "sp-a", "--reason", "x"][..], &["reopen", "sp-a"], &["defer", "sp-a"], &["undefer", "sp-a"],
            &["update", "sp-a", "--status", "open"], &["update", "sp-a", "-s", "closed"], &["update", "sp-a", "--status=open"],
            &["update", "sp-a", "--defer", "2026-10-08"]] {
            let exit = check_state_verb(&s(argv)).unwrap_or_else(|| panic!("{argv:?} was not refused"));
            assert!(exit.contains("spira-lc") || exit.contains("lifecycle"), "{argv:?}: {exit}");
        }
        assert!(check_state_verb(&s(&["reopen", "sp-a"])).unwrap().contains("spira-lc reopen"));
    }

    #[test_id(tc2tiw4)]
    #[test]
    fn reads_notes_labels_and_creates_are_not_state_verbs() {
        for argv in [&["show", "sp-a"][..], &["list", "--status", "open"], &["note", "sp-a", "--stdin"], &["label", "add", "sp-a", "x"],
            &["update", "sp-a", "--add-label", "x"], &["update", "sp-a", "--assignee", ""], &["create", "t", "--labels", "spira"], &[]] {
            assert_eq!(check_state_verb(&s(argv)), None, "{argv:?}");
        }
    }

    // covers: UC-config-store-preflight-13
    #[test_id(t7v2bl1)]
    #[test]
    fn repo_label_allows_home_repo_and_known_repos() {
        let e = env(&[("SPIRA_HOME_REPO", "spira")]);
        let reg = registry_from_env(&e, Path::new("x"));
        assert_eq!(check_repo_label(&s(&["create", "title", "--labels", "plan,repo:spira"]), &reg), None);
    }

    // covers: UC-config-store-preflight-13
    #[test_id(tx3sjvh)]
    #[test]
    fn repo_label_refuses_unknown_repo_and_names_valid_keys() {
        let map = "spira | /srv/spira | push | origin/main | |\nwidget | /srv/widget | pr | origin/main | |\n";
        let reg = Registry::new(Some(map), &env(&[("SPIRA_HOME_REPO", "homerepo")]), Path::new("x"));
        let msg = check_repo_label(&s(&["create", "title", "--labels", "plan,repo:nope"]), &reg);
        let msg = msg.expect("refused");
        assert!(msg.contains("repo:nope"), "{msg}");
        assert!(msg.contains("spira"), "{msg}");
        assert!(msg.contains("widget"), "{msg}");
    }

    /// `repo:WORD` where WORD starts with a letter: a literal repository name. `repo:<…>`
    /// placeholders and `repo:${…}` references do not match.
    fn bare_repo_literals(text: &str) -> Vec<String> {
        text.match_indices("repo:")
            .filter_map(|(i, _)| {
                let rest = &text[i + 5..];
                let word: String = rest.chars().take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-').collect();
                (word.chars().next().is_some_and(|c| c.is_ascii_lowercase())).then(|| format!("repo:{word}"))
            })
            .collect()
    }

    // covers: UC-config-store-preflight-14
    #[test_id(teydy3m)]
    #[test]
    fn no_shipped_runes_persona_names_a_repository_by_literal() {
        let planted = "bd create -l \"plan,repo:sentinel-test-literal\" and repo:<name> and repo:${SPIRA_HOME_REPO}";
        assert_eq!(bare_repo_literals(planted), vec!["repo:sentinel-test-literal"], "positive control: the matcher fires on a literal only");

        let dir = spira_config::resolve::locate_home_for_process().unwrap().join("runes");
        let mut checked = 0;
        let mut found = Vec::new();
        for e in std::fs::read_dir(&dir).unwrap().filter_map(Result::ok) {
            let p = e.path();
            if !p.extension().is_some_and(|x| x == "md" || x == "rune") {
                continue;
            }
            checked += 1;
            for lit in bare_repo_literals(&std::fs::read_to_string(&p).unwrap()) {
                found.push(format!("{}: {lit}", p.file_name().unwrap().to_string_lossy()));
            }
        }
        assert!(checked >= 10, "the sweep saw the shipped personas: {checked}");
        assert!(found.is_empty(), "personas that hardcode a repository: {found:?}");
    }

    #[test_id(tvjzo9k)]
    #[test]
    fn repo_label_no_repo_label_at_all_is_allowed() {
        let reg = registry_from_env(&env(&[]), Path::new("x"));
        assert_eq!(check_repo_label(&s(&["create", "title", "--labels", "plan"]), &reg), None);
    }

    #[test_id(tjk1npq)]
    #[test]
    fn repo_label_in_a_non_last_labels_flag_is_still_checked() {
        let map = "spira | /srv/spira | push | origin/main | |\n";
        let reg = Registry::new(Some(map), &env(&[("SPIRA_HOME_REPO", "homerepo")]), Path::new("x"));
        let msg = check_repo_label(&s(&["create", "title", "-l", "repo:nope", "-l", "plan"]), &reg);
        assert!(msg.expect("refused").contains("repo:nope"));
        assert_eq!(check_repo_label(&s(&["create", "title", "-l", "repo:spira", "--labels=plan"]), &reg), None);
    }

    #[test_id(thn8jnf)]
    #[test]
    fn created_repo_reads_the_repo_label_from_every_labels_flag() {
        assert_eq!(created_repo(&s(&["create", "T", "-l", "repo:deck", "-l", "plan"])), Some("deck".into()));
        assert_eq!(created_repo(&s(&["create", "T", "-l", "plan", "--labels=a, repo:deck"])), Some("deck".into()));
        assert_eq!(created_repo(&s(&["create", "T", "-l", "plan"])), None);
    }

    // -- check_schema_delete -----------------------------------------------------------------

    #[test_id(ts02y9t)]
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

    #[test_id(tdcecp3)]
    #[test]
    fn ask_shape_refuses_a_bare_alarm_and_accepts_a_decision() {
        let alarm = s(&["create", "SLOW QUERY on dolt", "--labels", "needs-ryan,overseer"]); // literal-ok: fixture/fallback
        assert!(check_ask_shape(&alarm, "needs-ryan").unwrap().contains("Exit:")); // literal-ok: fixture/fallback
        let ok = s(&["create", "Grant the token?", "-l", "needs-ryan", "-d", "## Question\nq\nDefault: grant\nClass: permissions"]); // literal-ok: fixture/fallback
        assert_eq!(check_ask_shape(&ok, "needs-ryan"), None); // literal-ok: fixture/fallback
        let design = s(&["create", "Pick a shape?", "-l", "needs-ryan", "-d", "## Question\nq\nDefault: a\nClass: design"]); // literal-ok: fixture/fallback
        assert_eq!(check_ask_shape(&design, "needs-ryan"), None); // literal-ok: fixture/fallback
        let bad_class = s(&["create", "Pick one?", "-l", "needs-ryan", "-d", "Default: a\nClass: architecture"]); // literal-ok: fixture/fallback
        assert!(check_ask_shape(&bad_class, "needs-ryan").is_some()); // literal-ok: fixture/fallback
        assert_eq!(check_ask_shape(&s(&["create", "SLOW QUERY", "-l", "overseer"]), "needs-ryan"), None); // literal-ok: fixture/fallback
    }

    // covers: UC-safety-fences-13
    #[test_id(tnt6md6)]
    #[test]
    fn schema_delete_is_detected_regardless_of_case_or_spacing() {
        assert!(check_schema_delete(&s(&["create", "DELETE   FROM schema_migrations"])).is_some());
        assert!(check_schema_delete(&s(&["create", "clean", "-d", "please delete from schema_migrations now"]))
            .is_some());
        assert_eq!(check_schema_delete(&s(&["create", "delete from somewhere_else"])), None);
    }

    #[test_id(tmr42gf)]
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

    #[test_id(tv3a2f5)]
    #[test]
    fn a_no_row_type_create_leaves_no_lifecycle_row() {
        for ty in ["event", "epic", "decision", "gate"] {
            assert!(creates_rowless_type(&s(&["create", "t", "--type", ty])), "{ty}");
        }
        assert!(creates_rowless_type(&s(&["create", "t", "--type=event"])));
        assert!(creates_rowless_type(&s(&["create", "t", "-t", "event"])));
        assert!(!creates_rowless_type(&s(&["create", "t", "--type", "task"])));
        assert!(!creates_rowless_type(&s(&["create", "event"])));
    }

    #[test_id(tyfyto2)]
    #[test]
    fn creates_closed_reads_the_status_flag() {
        assert!(creates_closed(&s(&["create", "t", "--status", "closed"])));
        assert!(creates_closed(&s(&["create", "t", "--status=closed"])));
        assert!(!creates_closed(&s(&["create", "t", "--status", "open"])));
        assert!(!creates_closed(&s(&["create", "closed"])));
    }

    #[test_id(tse52du)]
    #[test]
    fn is_create_only_true_for_the_create_verb() {
        assert!(is_create(&s(&["create", "x"])));
        assert!(is_create(&s(&["q", "x"])));
        assert!(is_create(&s(&["create-form"])));
        assert!(!is_create(&s(&["update", "sp-a"])));
        assert!(!is_create(&s(&[])));
    }

    #[test_id(t18o2eh)]
    #[test]
    fn creates_beads_covers_import_and_mol_spawns() {
        assert!(creates_beads(&s(&["create", "x"])));
        assert!(creates_beads(&s(&["import", "f.jsonl"])));
        assert!(creates_beads(&s(&["import", "-"])));
        assert!(creates_beads(&s(&["mol", "pour", "p"])));
        assert!(creates_beads(&s(&["mol", "wisp", "p"])));
        assert!(creates_beads(&s(&["mol", "wisp", "create", "p"])));
        assert!(creates_beads(&s(&["mol", "bond", "a", "b"])));
        assert!(!creates_beads(&s(&["mol", "wisp", "list"])));
        assert!(!creates_beads(&s(&["mol", "wisp", "gc"])));
        assert!(!creates_beads(&s(&["mol", "show", "m"])));
        assert!(!creates_beads(&s(&["import", "f", "--dry-run"])));
        assert!(!creates_beads(&s(&["mol", "pour", "p", "--dry-run"])));
        assert!(!creates_beads(&s(&["update", "sp-a"])));
    }

    #[test_id(t7nxvip)]
    #[test]
    fn import_and_mol_are_not_create_fenced() {
        assert!(!is_create(&s(&["import", "f"])));
        assert!(!is_create(&s(&["mol", "pour", "p"])));
    }

    // -- czar fence dispatch ------------------------------------------------------------------

    #[test_id(ttjkkkn)]
    #[test]
    fn czar_fence_fires_on_reopen_when_czar_with_class() {
        assert_eq!(czar_fence_class("czar", "deadlock", "", &s(&["reopen", "sp-a"])), Some("deadlock"));
    }

    #[test_id(ty1vklz)]
    #[test]
    fn czar_fence_skips_non_czar_or_empty_class() {
        assert_eq!(czar_fence_class("builder", "deadlock", "", &s(&["reopen", "sp-a"])), None);
        assert_eq!(czar_fence_class("czar", "", "", &s(&["reopen", "sp-a"])), None);
    }

    #[test_id(tlm7auy)]
    #[test]
    fn czar_fence_update_close_skip_the_trigger_bead_itself() {
        assert_eq!(czar_fence_class("czar", "deadlock", "sp-trigger", &s(&["update", "sp-trigger"])), None);
        assert_eq!(
            czar_fence_class("czar", "deadlock", "sp-trigger", &s(&["update", "sp-other"])),
            Some("deadlock")
        );
        assert_eq!(czar_fence_class("czar", "deadlock", "", &s(&["close", "sp-a"])), Some("deadlock"));
    }

    #[test_id(tuoq5si)]
    #[test]
    fn czar_fence_ignores_other_verbs() {
        assert_eq!(czar_fence_class("czar", "deadlock", "", &s(&["create", "x"])), None);
        assert_eq!(czar_fence_class("czar", "deadlock", "", &s(&["list"])), None);
    }

    // -- json_only ----------------------------------------------------------------------------

    #[test_id(tb4jvgw)]
    #[test]
    fn json_only_drops_a_warning_banner_before_the_json() {
        assert_eq!(json_only("warning: schema drift\n[{\"a\":1}]\n"), "[{\"a\":1}]\n");
        assert_eq!(json_only("{\"a\":1}\n"), "{\"a\":1}\n");
    }

    #[test_id(tkzq61w)]
    #[test]
    fn json_only_requires_column_one_no_leading_whitespace_tolerance() {
        // Matches sed's `^[[{]` exactly: a line that starts with whitespace before the
        // bracket is NOT a match, same as the bash original.
        assert_eq!(json_only("  [1]\n"), "");
    }

    #[test_id(t91asv1)]
    #[test]
    fn json_only_no_json_line_is_empty() {
        assert_eq!(json_only("just a warning\nand another line\n"), "");
    }

    #[test_id(tcfz5p3)]
    #[test]
    fn json_only_keeps_a_trailing_non_json_line_too() {
        // sed prints from the first match to the end of input, unconditionally.
        assert_eq!(json_only("[1]\nnot json\n"), "[1]\nnot json\n");
    }

    // -- should_retry -------------------------------------------------------------------------

    #[test_id(tp3upsz)]
    #[test]
    fn should_retry_only_on_invalid_connection_within_budget() {
        assert!(should_retry(1, 1, 2, true));
        assert!(!should_retry(0, 1, 2, true), "success never retries");
        assert!(!should_retry(1, 2, 2, true), "budget exhausted");
        assert!(!should_retry(1, 1, 2, false), "a different error never retries");
    }

    #[test_id(tf12g34)]
    #[test]
    fn should_retry_zero_retries_configured_still_allows_one_attempt() {
        // bash: tries="0" from env; try=1 after the first attempt, "1 -ge 0" is true -> break.
        // One attempt always happens; this function only governs whether a SECOND is tried.
        assert!(!should_retry(1, 1, 0, true));
    }
}
