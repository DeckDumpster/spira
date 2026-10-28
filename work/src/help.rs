//! The builder's manual. Every verb `work` serves has one [`Entry`]; every refusal `work` can
//! give is an [`R`], and its text lives only in an entry. Code refuses by naming an `R`, so a
//! refusal with no entry does not compile, and `work prime` prints what the code says.

use std::fmt::Write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum R {
    UnknownVerb,
    ReservedFlag,
    NotBound,
    ForeignBead,
    ForFlag,
    TwoBodies,
    BodyRead,
    NoBinding,
    SocketUnreachable,
    SubmitNoTip,
    SubmitTipArg,
    StatuteUsage,
    StatuteStdin,
    StatuteConfig,
    StatuteConfigUnset,
    StatuteRegistry,
    StatuteReport,
    StatuteNoteNotRecorded,
    CratesGit,
    CratesCargo,
    CratesSelect,
    HelpUsage,
    HelpUnknown,
    LawUsage,
    LawNotFound,
    ReviewArgs,
    ReviewShow,
    ReviewGit,
    ReviewConfig,
    ReviewAgentUnset,
    ReviewTimedOut,
    ReviewFailed,
    ChecksUsage,
    ChecksGit,
    ChecksConfig,
}

impl R {
    pub const ALL: &'static [R] = &[
        R::UnknownVerb, R::ReservedFlag, R::NotBound, R::ForeignBead, R::ForFlag, R::TwoBodies, R::BodyRead, R::NoBinding, R::SocketUnreachable, R::SubmitNoTip,
        R::SubmitTipArg, R::StatuteUsage, R::StatuteStdin, R::StatuteConfig, R::StatuteConfigUnset, R::StatuteRegistry, R::StatuteReport,
        R::StatuteNoteNotRecorded, R::CratesGit, R::CratesCargo, R::CratesSelect, R::HelpUsage, R::HelpUnknown, R::LawUsage, R::LawNotFound,
        R::ReviewArgs, R::ReviewShow, R::ReviewGit, R::ReviewConfig, R::ReviewAgentUnset, R::ReviewTimedOut, R::ReviewFailed,
        R::ChecksUsage, R::ChecksGit, R::ChecksConfig,
    ];
}

pub struct Refusal {
    pub id: R,
    pub text: &'static str,
    pub fix: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Read,
    Ask,
    Followups,
    Law,
    Verify,
    Review,
    Submit,
    Lane,
}

impl Group {
    pub const FLOW: &'static [Group] = &[Group::Read, Group::Ask, Group::Followups, Group::Law, Group::Verify, Group::Review, Group::Submit, Group::Lane];

    fn title(self) -> &'static str {
        match self {
            Group::Read => "1. Reading the work item",
            Group::Ask => "2. Asking, noting, and being blocked",
            Group::Followups => "3. Follow-ups and splits",
            Group::Law => "4. The law",
            Group::Verify => "5. Verifying",
            Group::Review => "6. Review",
            Group::Submit => "7. Submitting",
            Group::Lane => "8. Other verbs (reach depends on your persona)",
        }
    }

    fn note(self) -> &'static str {
        match self {
            Group::Verify => "Run suites only through testenv; `testenv --help` is its manual.",
            Group::Lane => "The broker refuses a verb your persona's allow row does not name; that refusal is its own text.",
            _ => "",
        }
    }
}

pub struct Entry {
    pub verb: &'static str,
    pub group: Group,
    pub does: &'static str,
    pub when: &'static str,
    pub args: &'static [&'static str],
    pub checks: &'static [&'static str],
    pub refusals: &'static [Refusal],
}

const fn lane(verb: &'static str, does: &'static str, when: &'static str, args: &'static [&'static str]) -> Entry {
    Entry {
        verb,
        group: Group::Lane,
        does,
        when,
        args,
        checks: &["the broker's allow row for your persona names this verb"],
        refusals: &[],
    }
}

pub const GENERAL: Entry = Entry {
    verb: "(any verb)",
    group: Group::Read,
    does: "Rules every verb shares.",
    when: "Always.",
    args: &[],
    checks: &[
        "the verb is one `work` serves",
        "no argument is `--actor` or `--stdin`: the client sends those itself",
        "a bound verb runs only inside a bead's environment and names no bead but the bound one",
        "at most one body (`--body-file`, `--reason-file`, `--why`, or a lone `-`) per request",
        "the socket answers; a verb that cannot reach it exits 2 (\"cannot tell\"), which is safe to retry",
    ],
    refusals: &[
        Refusal { id: R::UnknownVerb, text: "unknown verb {verb:?} (want {want})", fix: "use one of the verbs listed; `work prime` shows them all" },
        Refusal { id: R::ReservedFlag, text: "{flag} is the client's to send, never the caller's", fix: "drop {flag}; the client supplies your persona and the request body itself" },
        Refusal { id: R::NotBound, text: "this environment is not bound to a bead (SPIRA_WORK_BEAD_ID), and {verb} acts on the bound bead", fix: "run it from a bead's own session, or use a verb that names its target" },
        Refusal { id: R::ForeignBead, text: "{foreign} is not the bound bead ({bound})", fix: "name only your own bead; `superseded-by` and the `--follow-on`/`--escalation` values of submit and done are the only places another bead belongs" },
        Refusal { id: R::TwoBodies, text: "more than one body in one request{detail}", fix: "put one body on stdin or in one file; fold the rest into it" },
        Refusal { id: R::BodyRead, text: "{err}", fix: "check the path you passed to the body flag, or pipe the text on stdin" },
        Refusal { id: R::NoBinding, text: "cannot tell: SPIRA_WORK_BEAD_ID is not set — this environment is not bound to a bead", fix: "run inside the bead's session; exit 2 means nothing was done and a retry is safe" },
        Refusal { id: R::SocketUnreachable, text: "cannot tell: {err}", fix: "nothing was recorded; retry. If it persists, leave a note with the evidence and exit non-zero" },
    ],
};

pub static ENTRIES: &[Entry] = &[
    Entry {
        verb: "show",
        group: Group::Read,
        does: "Prints your bead: description, labels, notes, children.",
        when: "First, and again after any amendment or reopen.",
        args: &[],
        checks: &["the bead is the bound one"],
        refusals: &[],
    },
    lane("read", "Prints another bead.", "You need a bead your own names (a dependency, a parent).", &["<bead-id>"]),
    lane("list", "Lists beads matching a filter.", "You need to find work by state or label.", &["filter flags as the persona allows"]),
    lane("search", "Searches beads by text.", "You are looking for a bead whose id you do not have.", &["<text>"]),
    Entry {
        verb: "note",
        group: Group::Ask,
        does: "Leaves a plain note on your bead. No lifecycle effect.",
        when: "You want evidence or a decision recorded where the next reader will see it.",
        args: &["<text> | -   (text on stdin)"],
        checks: &["the bead is the bound one"],
        refusals: &[],
    },
    lane("ask", "Posts an ask to the operator's mailbox.", "A decision outside your bead needs a human; always give a default and a class basis.", &["--subject <q>", "--kind question", "--class <permissions|policy|destructive|design>", "--default <what you would do>", "--body-file -"]),
    Entry {
        verb: "blocked",
        group: Group::Ask,
        does: "Holds your bead and files the ask in one step.",
        when: "Only the operator can answer, and you cannot finish without it.",
        args: &["\"<the question>\"", "--default \"<what you would do by default>\""],
        checks: &["the bead is the bound one", "a default is given"],
        refusals: &[],
    },
    Entry {
        verb: "ack",
        group: Group::Ask,
        does: "Records your answer to a live amendment of the bead.",
        when: "`work show` lists an amendment awaiting your answer.",
        args: &["<amendment-id>", "--accept | --decline (with a reason)"],
        checks: &["the amendment belongs to your bead"],
        refusals: &[],
    },
    Entry {
        verb: "file-followup",
        group: Group::Followups,
        does: "Files a bead for work that follows from yours, parented to it.",
        when: "You found work you must not do now. A builder child needs a body with a `## Build` section naming the artifact and where.",
        args: &["\"<title>\"", "--repo <name>   (required: where the child's work lives)", "--body-file -   (body on stdin)"],
        checks: &["the parent is your bead", "the child never carries your `branch:` label"],
        refusals: &[Refusal { id: R::ForFlag, text: "--for is the client's to send (it files as your own persona)", fix: "drop --for" }],
    },
    Entry {
        verb: "split",
        group: Group::Followups,
        does: "Files a bead for a piece of your own bead's scope, parented to it.",
        when: "Part of this bead's scope is better landed on its own.",
        args: &["\"<title>\"", "--repo <name>   (required)", "--body-file -"],
        checks: &["the parent is your bead", "the child never carries your `branch:` label"],
        refusals: &[],
    },
    Entry {
        verb: "statute-check",
        group: Group::Law,
        does: "Checks a plan against the statutes before a governed decision and records the plan and the slugs on the bead.",
        when: "Before you add a program, service, daemon or unit; a language or file type; a dependency; or a protocol. A plan that adds none records nothing.",
        args: &["\"<one-paragraph plan>\" | -   (plan on stdin)"],
        checks: &["the registry and the statute mirror are configured and readable", "every statute returned has its full text", "the note is recorded before the text is printed"],
        refusals: &[
            Refusal { id: R::StatuteUsage, text: "usage: work statute-check \"<one-paragraph plan>\" | work statute-check - < plan", fix: "pass exactly one argument: the plan, or `-` with the plan on stdin" },
            Refusal { id: R::StatuteStdin, text: "reading stdin: {err}", fix: "pass the plan as an argument instead" },
            Refusal { id: R::StatuteConfig, text: "refusing: {err}", fix: "the statute settings are missing from your environment; report it in a note and exit non-zero" },
            Refusal { id: R::StatuteConfigUnset, text: "refusing: SPIRA_STATUTE_GUARDS and SPIRA_STATUTE_DIR must both be set; an unconfigured check is not a pass", fix: "do not proceed as if checked; report the missing settings in a note and exit non-zero" },
            Refusal { id: R::StatuteRegistry, text: "refusing: cannot read the statute registry {path}: {err}", fix: "the check cannot run; report it in a note and exit non-zero rather than proceeding unchecked" },
            Refusal { id: R::StatuteReport, text: "refusing: {err}", fix: "a returned statute has no text in the mirror; report it in a note, the check is incomplete" },
            Refusal { id: R::StatuteNoteNotRecorded, text: "the note was not recorded on the bead (exit {code}): {err}", fix: "retry; an unrecorded check does not count" },
        ],
    },
    Entry {
        verb: "law",
        group: Group::Law,
        does: "Prints the statute code from the `law/` tree of your worktree: the table of contents, one part, one numbered section, or one statute by slug.",
        when: "Before a choice the statutes govern, to read the rule itself rather than a summary of it.",
        args: &["[<part> | <part>.<n> | <slug>]   (default: the table of contents)"],
        checks: &["it reads the tracked tree of the worktree it runs in; no store, no socket"],
        refusals: &[
            Refusal { id: R::LawUsage, text: "usage: work law [<part> | <part>.<n> | <slug>]", fix: "pass at most one argument" },
            Refusal { id: R::LawNotFound, text: "{err}", fix: "pick a part, section or slug from the table of contents printed with it" },
        ],
    },
    Entry {
        verb: "crates",
        group: Group::Verify,
        does: "Prints the `-p` words for your diff: touched crates plus their direct dependents.",
        when: "Before `cargo test`/`build`/`check`: `cargo test $(work crates)`. Empty output means nothing buildable changed. Never use `--workspace`.",
        args: &["[<base>]   (default local/main)"],
        checks: &["run it inside your worktree, where git and cargo can see the diff"],
        refusals: &[
            Refusal { id: R::CratesGit, text: "git diff {base}...HEAD failed", fix: "check the base ref exists (`local/main` by default) and that you are in your worktree" },
            Refusal { id: R::CratesCargo, text: "cargo metadata failed", fix: "run it from inside the workspace" },
            Refusal { id: R::CratesSelect, text: "{err}", fix: "the diff could not be mapped to crates; test the crate you edited by name with `cargo test -p <crate>`" },
        ],
    },
    Entry {
        verb: "checks",
        group: Group::Verify,
        does: "Runs each `[[check]]` row the repository declares in `.spira/checks.toml`, each bounded by its budget, and prints `-- <name>: ok` or `FAIL` per row.",
        when: "Before you submit, to run the checks the repository itself asks of a finished change. `--list` prints the rows without running them.",
        args: &["[--list]"],
        checks: &["run it inside your worktree", "exits non-zero when any row fails, naming each failed row"],
        refusals: &[
            Refusal { id: R::ChecksUsage, text: "usage: work checks [--list]", fix: "pass no argument, or `--list`" },
            Refusal { id: R::ChecksGit, text: "not inside a git worktree", fix: "run it from inside your worktree" },
            Refusal { id: R::ChecksConfig, text: "refusing: .spira/checks.toml is unreadable: {err}", fix: "fix the file's TOML; a repository with no checks omits the file" },
        ],
    },
    Entry {
        verb: "review",
        group: Group::Review,
        does: "Launches one read-only reviewer session over your bead's text and your branch's diff, and prints its findings, one `- file:line — what to change` per line. It changes nothing.",
        when: "Before you submit. Answer each finding in your submission.",
        args: &["[--timeout SECONDS]   (default 600)", "[--base REF]   (default local/main)"],
        checks: &["the bead's text is read through `work show`", "the reviewer runs with your persona's configured model", "a reviewer that outlives the timeout is stopped; exit 124"],
        refusals: &[
            Refusal { id: R::ReviewArgs, text: "{err}", fix: "`work review [--timeout SECONDS] [--base REF]`; the timeout is a whole number of seconds, at least 1" },
            Refusal { id: R::ReviewShow, text: "work show refused (exit {code}): {err}", fix: "run it from your bead's own session; `work show` must answer first" },
            Refusal { id: R::ReviewGit, text: "git diff {base}...HEAD failed", fix: "check the base ref exists (`local/main` by default) and that you are in your worktree" },
            Refusal { id: R::ReviewConfig, text: "refusing: {err}", fix: "the reviewer's settings are missing from your environment; report it in a note and submit without a review" },
            Refusal { id: R::ReviewAgentUnset, text: "refusing: SPIRA_AGENT is not set", fix: "the reviewer cannot be launched; report it in a note and submit without a review" },
            Refusal { id: R::ReviewTimedOut, text: "timed out after {secs}s; the reviewer was stopped and no findings were returned", fix: "retry with a longer `--timeout`, or submit and say no review was returned" },
            Refusal { id: R::ReviewFailed, text: "{err}", fix: "the reviewer session failed; retry once, then submit and say no review was returned" },
        ],
    },
    Entry {
        verb: "submit",
        group: Group::Submit,
        does: "Moves your bead to SUBMITTED; the landing pass certifies and lands it.",
        when: "The work is committed on your branch, its commit subject names your bead, and you rebased onto local/main. Do not run the full gate first.",
        args: &["--met \"<measured value and where>\" | --contradicted \"<evidence>\" | --short \"<value>\" with --follow-on <id> or --escalation <id>   (only when the description carries a `Target:` line)"],
        checks: &[
            "the tip is read from your branch, never taken from you",
            "a bead with a `Target:` line is submitted with a verdict; a submission without one is refused by the broker",
            "a --short verdict names a follow-on or escalation you filed",
        ],
        refusals: &[
            Refusal { id: R::SubmitNoTip, text: "no tip could be read from the worktree", fix: "run it from your worktree after committing; your branch is spira/<bead>" },
            Refusal { id: R::SubmitTipArg, text: "{flag} is read from the worktree, never taken from the caller", fix: "drop {flag}; commit and run `work submit` bare" },
        ],
    },
    Entry {
        verb: "done",
        group: Group::Submit,
        does: "Closes a bead whose deliverable is not code on this branch (child beads, a document, mail).",
        when: "You produced something other than a commit.",
        args: &["--delivers \"<what you produced, or where it lives>\"", "verdict flags as for submit"],
        checks: &["the bead is the bound one", "a bead with a `Target:` line carries a verdict"],
        refusals: &[],
    },
    Entry {
        verb: "superseded-by",
        group: Group::Submit,
        does: "Requests that your bead be retired because another bead already landed its work. Holds it; does not close it.",
        when: "You verified the successor's commit is on local/main (`git log local/main | grep <id>`).",
        args: &["<successor-id>"],
        checks: &["the successor is the only other bead you may name"],
        refusals: &[],
    },
    lane("note-on", "Leaves a note on another bead.", "A lane persona annotates work it does not own.", &["<bead-id>", "<text>"]),
    lane("label-add", "Adds a label to a bead.", "A lane persona classifies work.", &["<bead-id>", "<label>"]),
    lane("label-remove", "Removes a label from a bead.", "A lane persona reclassifies work.", &["<bead-id>", "<label>"]),
    lane("dep-add", "Adds a dependency between beads.", "A lane persona records a real blocking edge.", &["<bead-id>", "<depends-on-id>"]),
    lane("relate", "Relates two beads without blocking.", "A lane persona links related work.", &["<bead-id>", "<other-id>"]),
    lane("reopen", "Reopens a bead.", "A lane persona returns work to the queue.", &["<bead-id>"]),
    lane("close-other", "Closes another bead with a reason.", "A lane persona retires work.", &["<bead-id>", "<reason>"]),
    lane("close-ask", "Closes an ask in the operator mailbox.", "The decision that settled it is known.", &["<ask-id>", "<what decided it>"]),
    lane("file", "Files a bead outside any parent.", "A lane persona with a remit to file.", &["<title>", "--kind <kind>", "--body-file -"]),
    lane("groom", "Runs the groomer.", "Grooming persona only.", &["<sub>"]),
    lane("incident", "Runs the incident tool.", "Operations persona only.", &["<sub>"]),
    lane("sop", "Reads or writes standing procedures.", "Operations persona only.", &["<sub>"]),
    lane("census", "Runs the census.", "Operations persona only.", &[]),
    lane("queue", "Runs queue steps.", "Queue persona only.", &["<sub>"]),
    lane("landing-pass", "Runs the landing pass.", "Landing persona only.", &["<sub>"]),
    lane("strand", "Runs the stranded-work tool.", "Operations persona only.", &["<sub>"]),
    lane("fence", "Runs the fence tool.", "Operations persona only.", &["<sub>"]),
    lane("hel", "Runs the hel tool.", "Operations persona only.", &["<sub>"]),
    lane("mender", "Runs the mender tool.", "Mender persona only.", &["diagnose|outcome <path> ... (text on stdin)"]),
    Entry {
        verb: "prime",
        group: Group::Read,
        does: "Prints this whole manual, generated from the code.",
        when: "Before you begin.",
        args: &[],
        checks: &[],
        refusals: &[],
    },
    Entry {
        verb: "help",
        group: Group::Read,
        does: "Prints one verb's entry: what it does, its arguments, what it checks, and each refusal with its fix.",
        when: "A verb refused you, or you are about to use one for the first time.",
        args: &["<verb>"],
        checks: &["the verb is one `work` serves"],
        refusals: &[
            Refusal { id: R::HelpUsage, text: "usage: work help <verb>", fix: "name one verb; `work prime` lists them all" },
            Refusal { id: R::HelpUnknown, text: "no verb {verb:?}", fix: "run `work prime` for the verbs" },
        ],
    },
];

pub fn entry(verb: &str) -> Option<&'static Entry> {
    ENTRIES.iter().find(|e| e.verb == verb)
}

fn find(id: R) -> Option<(&'static Entry, &'static Refusal)> {
    std::iter::once(&GENERAL).chain(ENTRIES.iter()).find_map(|e| e.refusals.iter().find(|r| r.id == id).map(|r| (e, r)))
}

fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut s = template.to_string();
    for (k, v) in vars {
        s = s.replace(&format!("{{{k}}}"), v).replace(&format!("{{{k}:?}}"), &format!("{v:?}"));
    }
    s
}

/// The refusal as the verb prints it: its text and its fix, from the entry that documents it.
pub fn refusal(id: R, verb: &str, vars: &[(&str, &str)]) -> String {
    let (_, r) = find(id).expect("every R has a refusal row (tested)");
    let mut all = vars.to_vec();
    if !vars.iter().any(|(k, _)| *k == "verb") {
        all.push(("verb", verb));
    }
    let text = fill(r.text, &all);
    let fix = fill(r.fix, &all);
    if verb.is_empty() {
        format!("work: {text} — {fix}")
    } else if text.starts_with("cannot tell") {
        format!("{text} — {fix}")
    } else {
        format!("work {verb}: {text} — {fix}")
    }
}

pub fn render_entry(e: &Entry) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "### work {}", e.verb);
    let _ = writeln!(s, "{}", e.does);
    let _ = writeln!(s, "Use when: {}", e.when);
    if !e.args.is_empty() {
        let _ = writeln!(s, "Arguments:");
        for a in e.args {
            let _ = writeln!(s, "  {a}");
        }
    }
    if !e.checks.is_empty() {
        let _ = writeln!(s, "Checks:");
        for c in e.checks {
            let _ = writeln!(s, "  - {c}");
        }
    }
    if !e.refusals.is_empty() {
        let _ = writeln!(s, "Refusals:");
        for r in e.refusals {
            let _ = writeln!(s, "  - {}\n    Fix: {}", r.text, r.fix);
        }
    }
    s
}

pub fn help(verb: &str) -> Result<String, String> {
    let e = entry(verb).or(if verb == "(any verb)" { Some(&GENERAL) } else { None });
    e.map(render_entry).ok_or_else(|| refusal(R::HelpUnknown, "help", &[("verb", verb)]))
}

pub fn prime() -> String {
    let mut s = String::from("# work — the builder's manual\n\nEvery verb below is run as `work <verb>`. Generated from the verb definitions; it cannot drift from them.\n\n");
    for g in Group::FLOW {
        let _ = writeln!(s, "## {}", g.title());
        if !g.note().is_empty() {
            let _ = writeln!(s, "{}\n", g.note());
        }
        let mut any = false;
        for e in ENTRIES.iter().filter(|e| e.group == *g) {
            any = true;
            s.push_str(&render_entry(e));
            s.push('\n');
        }
        if !any {
            s.push_str("(no verbs yet)\n\n");
        }
    }
    let _ = writeln!(s, "## Rules every verb shares");
    s.push_str(&render_entry(&GENERAL));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BOUND_VERBS, LANE_VERBS, LOCAL_VERBS, VERBS};

    #[test]
    fn every_served_verb_has_an_entry() {
        for v in VERBS.iter().chain(LOCAL_VERBS) {
            assert!(entry(v).is_some(), "work {v} has no help entry");
        }
        for e in ENTRIES {
            assert!(VERBS.contains(&e.verb) || LOCAL_VERBS.contains(&e.verb), "entry for {} names no verb", e.verb);
        }
    }

    #[test]
    fn no_entry_is_listed_twice() {
        for e in ENTRIES {
            assert_eq!(ENTRIES.iter().filter(|x| x.verb == e.verb).count(), 1, "{}", e.verb);
        }
    }

    #[test]
    fn every_refusal_has_one_row_with_a_fix() {
        for id in R::ALL {
            let rows = std::iter::once(&GENERAL).chain(ENTRIES.iter()).flat_map(|e| e.refusals.iter()).filter(|r| r.id == *id).count();
            assert_eq!(rows, 1, "{id:?} must have exactly one refusal row");
        }
        for r in std::iter::once(&GENERAL).chain(ENTRIES.iter()).flat_map(|e| e.refusals.iter()) {
            assert!(!r.fix.trim().is_empty() && !r.text.trim().is_empty(), "{:?}", r.id);
        }
    }

    #[test]
    fn the_broker_serves_the_verbs_the_client_names() {
        let src = include_str!("../../spira-lc/src/work.rs");
        let list = |name: &str| -> Vec<String> {
            let at = src.find(&format!("{name}: &[&str] = &[")).unwrap_or_else(|| panic!("{name} not found"));
            let body = &src[at..];
            let body = &body[body.find('[').map(|i| body[i + 1..].find('[').map(|j| i + 1 + j).unwrap_or(0)).unwrap()..];
            body[1..body.find(']').unwrap()].split(',').map(|t| t.trim().trim_matches('"').to_string()).filter(|t| !t.is_empty()).collect()
        };
        assert_eq!(list("BOUND_VERBS"), BOUND_VERBS);
        assert_eq!(list("LANE_VERBS"), LANE_VERBS);
    }

    #[test]
    fn help_submit_prints_each_refusal_and_its_fix() {
        let out = help("submit").unwrap();
        for r in entry("submit").unwrap().refusals {
            assert!(out.contains(r.text) && out.contains(r.fix));
        }
        assert!(help("nonesuch").is_err());
    }

    #[test]
    fn a_refusal_prints_the_text_the_manual_prints() {
        let msg = refusal(R::SubmitTipArg, "submit", &[("flag", "--tip")]);
        assert!(msg.contains("--tip is read from the worktree"), "{msg}");
        assert!(prime().contains("{flag} is read from the worktree"));
    }

    #[test]
    fn prime_matches_its_golden() {
        assert_eq!(prime(), include_str!("../tests/prime.golden"));
    }
}
