//! The control store's verbs against the dolt path's goldens (`tools/lc-parity/goldens`): the seed's
//! recorded steps are replayed onto a fresh control store, then each case's steps, and every
//! answer must match its golden byte for byte. A case whose golden this store cannot reproduce
//! is named in `DIVERGES` with the reason; one named there that matches fails too, so the list
//! cannot outlive the difference it records.


use super::is_verb;
use crate::callers::{self};
struct Step {
    argv: Vec<String>,
    /// What the capture gave on stdin, which the CLI inlines as `--json` for the verbs that
    /// take a payload; any other verb given stdin is not served.
    stdin: Option<String>,
}

/// The verbs whose payload the CLI reads from stdin before it reaches either store.
const PAYLOAD_VERBS: &[&str] = &["claim-verdicts", "batch-progress"];

/// The argv either store receives: a payload verb's stdin travels as `--json <body>`.
fn delivered_argv(step: &Step) -> Vec<String> {
    match &step.stdin {
        Some(body) => crate::cutover::inline_payload_from(step.argv.clone(), &mut body.as_bytes()),
        None => step.argv.clone(),
    }
}

/// A harness log line's wall-clock stamp as the capture writes it (`capture.sh`'s `normalize`).
fn normalized(s: &str) -> String {
    let stamped = |l: &str| {
        let b = l.as_bytes();
        b.len() >= 28
            && l[20..].starts_with(" spira: ")
            && b[19] == b'Z'
            && b[..19].iter().enumerate().all(|(i, c)| match i {
                4 | 7 => *c == b'-',
                10 => *c == b'T',
                13 | 16 => *c == b':',
                _ => c.is_ascii_digit(),
            })
    };
    s.split_inclusive('\n')
        .map(|l| {
            if stamped(l) {
                format!("@WALL@{}", &l[20..])
            } else {
                l.to_string()
            }
        })
        .collect()
}

/// The verbs `main` answers before the caller verbs, so none of them is a caller verb here.
const BEFORE_CALLERS: &[&str] = &[
    "close-on-land",
    "unclaim",
    "reconcile-closed",
    "reconcile-asks",
    "reconcile-epics",
    "reconcile-content",
    "drop-orphans",
    "close",
    "drop-bead",
    "drop-row",
    "reopen",
    "bd-supersede",
    "bd-note",
    "content",
    "close-epic",
    "unpublished",
    "content-landed",
    "drop",
];

/// The verbs under test: this module's and the caller verbs built on them. The feed verbs
/// answer a caller verb that reads them, and their own goldens are another module's to prove.
fn served_verb(verb: &str) -> bool {
    is_verb(verb) || (callers::is_verb(verb) && !BEFORE_CALLERS.contains(&verb))
}

/// Whether the control store answers this step: a served verb, on a machine it keeps when it
/// is an event, given stdin only when it takes a payload.
fn served(step: &Step) -> bool {
    let argv = &step.argv;
    let machine = argv.get(1).map(String::as_str);
    let stdin_ok = step.stdin.is_none() || PAYLOAD_VERBS.contains(&argv[0].as_str());
    stdin_ok
        && served_verb(&argv[0])
        && (argv[0] != "event" || matches!(machine, Some("bead" | "batch" | "delivery")))
}

fn bare(argv: &[String]) -> Step {
    Step {
        argv: argv.to_vec(),
        stdin: None,
    }
}

#[test]
fn a_log_lines_wall_stamp_is_written_as_the_capture_writes_it_and_nothing_else_is() {
    assert_eq!(
        normalized("2026-11-01T09:00:00Z spira: lc: x\nplain\n2026-11-01 spira: y"),
        "@WALL@ spira: lc: x\nplain\n2026-11-01 spira: y"
    );
    assert_eq!(
        normalized("x 2026-11-01T09:00:00Z spira: z"),
        "x 2026-11-01T09:00:00Z spira: z"
    );
}

#[test]
fn a_payload_verbs_stdin_reaches_the_store_as_its_json_flag_and_only_it_is_served() {
    let argv: Vec<String> = ["batch-progress", "b-open"].map(String::from).into();
    let step = Step {
        stdin: Some("{\"pass\":1}".into()),
        ..bare(&argv)
    };
    assert_eq!(
        delivered_argv(&step),
        ["batch-progress", "b-open", "--json", "{\"pass\":1}"]
    );
    assert!(served(&step));
    let show: Vec<String> = ["show", "sp-a"].map(String::from).into();
    assert!(!served(&Step {
        stdin: Some("x".into()),
        ..bare(&show)
    }));
}
