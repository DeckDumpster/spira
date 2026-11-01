//! The control store's verbs against the dolt path's goldens (`tools/lc-parity/goldens`): the seed's
//! recorded steps are replayed onto a fresh control store, then each case's steps, and every
//! answer must match its golden byte for byte. A case whose golden this store cannot reproduce
//! is named in `DIVERGES` with the reason; one named there that matches fails too, so the list
//! cannot outlive the difference it records.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use runtime::ports::VirtualClock;
use state_store::{Backend, Config, Contract, MemBackend, Ports};
use state_store_sqlite::{Options, SqliteBackend};
use testkit::TempDir;

use super::{is_verb, Call};
use crate::callers::{self, Machine};
use crate::control::store::Store;
use crate::control::verbs;

/// The capture's pinned clock (`capture.sh`'s `NOW_EPOCH`); no golden moves it.
const NOW: i64 = 1_767_225_600;
const FLOOR_MS: u64 = 1_000;

/// Seed steps whose verbs the control store does not serve yet. They are not replayed, so the
/// state they would have built is absent: the ask and the fact.
const UNSERVED_SEED: &[&str] = &["create-ask", "fact"];

/// Cases on served verbs whose goldens this store does not reproduce, and why.
const DIVERGES: &[(&str, &str)] = &[
    ("list", "title and priority are planning-plane fields the control plane does not keep"),
    ("list-live", "as list"),
    ("list-state", "title and priority are planning fields"),
    ("list-ids", "title and priority are planning fields"),
    ("create-bead", "title and priority are planning fields; list --ids shows them"),
    ("set-repo", "list --ids shows sp-ready's title and priority, planning fields"),
    ("set-repo-only-if", "as set-repo"),
    ("claim-verdicts", "list --ids shows the titles and priorities, planning fields"),
    ("express", "list --express shows sp-ready's title, a planning field"),
    ("supersede", "the dolt path retires the bead's mends through bd on a terminal move, and with bd absent from the capture its golden carries that failure on stderr; the control store retires no mends yet"),
    ("content-on-base", "as supersede: the golden's stderr is the dolt path's mend retirement failing on the absent bd"),
];

fn goldens() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../tools/lc-parity/goldens")
}

#[derive(Clone, Debug, PartialEq)]
struct Answer {
    rc: i32,
    stdout: String,
    stderr: String,
}

struct Step {
    argv: Vec<String>,
    /// What the capture gave on stdin, which the CLI inlines as `--json` for the verbs that
    /// take a payload; any other verb given stdin is not served.
    stdin: Option<String>,
    want: Answer,
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

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn steps(dir: &Path) -> Vec<Step> {
    let mut n = 1;
    let mut out = Vec::new();
    loop {
        let base = dir.join(format!("{n:02}"));
        if !base.with_extension("argv").exists() {
            return out;
        }
        assert!(
            !base.with_extension("now").exists(),
            "{}: a step that moves the clock is not replayed here",
            base.display()
        );
        let argv = read(&base.with_extension("argv"))
            .lines()
            .map(String::from)
            .collect();
        out.push(Step {
            argv,
            stdin: base
                .with_extension("stdin")
                .exists()
                .then(|| read(&base.with_extension("stdin"))),
            want: Answer {
                rc: read(&base.with_extension("rc")).trim().parse().unwrap(),
                stdout: read(&base.with_extension("stdout")),
                stderr: read(&base.with_extension("stderr")),
            },
        });
        n += 1;
    }
}

/// The verbs `main` answers before the caller verbs, so none of them is a caller verb here.
const BEFORE_CALLERS: &[&str] = &[
    "close-on-land",
    "unclaim",
    "reconcile-closed",
    "migrate-asks",
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

fn primitive(verb: &str) -> bool {
    is_verb(verb) || verbs::VERBS.contains(&verb)
}

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

struct Rig<'a, B: Backend> {
    store: &'a Store<B>,
    n: u64,
}

impl<B: Backend> Machine for Rig<'_, B> {
    fn call(&mut self, args: &[String]) -> (i32, String) {
        self.n += 1;
        let request = format!("parity/{}", self.n);
        match args.first().map(String::as_str) {
            Some(v) if is_verb(v) => super::run(
                self.store,
                args,
                &Call {
                    now: NOW,
                    request: &request,
                },
            ),
            Some(v) if verbs::VERBS.contains(&v) => verbs::run(self.store, args),
            other => panic!("a caller verb reached {other:?}, which is not served"),
        }
    }
}

/// One step as the binary prints it: a primitive's answer on stdout with a newline, a caller
/// verb's streams as `emit` writes them, each normalized as the capture normalized its golden.
fn run<B: Backend>(rig: &mut Rig<'_, B>, step: &Step) -> Answer {
    let argv = delivered_argv(step);
    let verb = argv[0].as_str();
    if primitive(verb) {
        let (rc, out) = rig.call(&argv);
        let stdout = if out.is_empty() {
            out
        } else {
            format!("{out}\n")
        };
        return Answer {
            rc,
            stdout: normalized(&stdout),
            stderr: String::new(),
        };
    }
    let a = callers::run(verb, &argv[1..], rig);
    let mut stdout = a.stdout;
    if !stdout.is_empty() && !stdout.ends_with('\n') {
        stdout.push('\n');
    }
    Answer {
        rc: a.code,
        stdout: normalized(&stdout),
        stderr: normalized(&a.stderr),
    }
}

fn store<B: Backend>(b: B, clock: &VirtualClock) -> Store<B> {
    let c = Contract::open(
        b,
        Ports::virtual_clock(clock),
        Config {
            min_lock_ttl_ms: FLOOR_MS,
        },
    )
    .unwrap();
    Store::new(c, FLOOR_MS)
}

fn seeded<B: Backend>(s: &Store<B>) {
    let mut rig = Rig { store: s, n: 0 };
    for (i, step) in steps(&goldens().join("_seed")).iter().enumerate() {
        if UNSERVED_SEED.contains(&step.argv[0].as_str()) {
            continue;
        }
        assert!(
            served(step),
            "seed step {} {:?} is neither served nor declared unserved",
            i + 1,
            step.argv
        );
        assert_eq!(
            run(&mut rig, step),
            step.want,
            "seed step {}: {:?}",
            i + 1,
            step.argv
        );
    }
}

/// Every case whose steps are all served, each on a fresh seeded store from `open`: the ones
/// whose answers differ from their goldens, with the first difference.
fn diverging<B: Backend>(open: impl Fn() -> Store<B>) -> Compared {
    let mut out = BTreeMap::new();
    let mut ran = BTreeSet::new();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(goldens())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().is_some_and(|n| n != "_seed") && p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let case = dir.file_name().unwrap().to_string_lossy().into_owned();
        let steps = steps(&dir);
        assert!(!steps.is_empty(), "{case} records no step");
        if !steps.iter().all(served) {
            continue;
        }
        ran.insert(case.clone());
        let s = open();
        seeded(&s);
        let mut rig = Rig {
            store: &s,
            n: 1_000,
        };
        for (i, step) in steps.iter().enumerate() {
            let got = run(&mut rig, step);
            if got != step.want {
                out.insert(
                    case.clone(),
                    format!(
                        "step {} {:?}: got {got:?}, want {:?}",
                        i + 1,
                        step.argv,
                        step.want
                    ),
                );
                break;
            }
        }
    }
    Compared {
        ran,
        diverging: out,
    }
}

struct Compared {
    ran: BTreeSet<String>,
    diverging: BTreeMap<String, String>,
}

/// The cases on served verbs that must match their goldens: fewer than this means the rig
/// stopped comparing something.
const MIN_MATCHING: usize = 54;

fn assert_parity(c: Compared) {
    let declared: BTreeSet<&str> = DIVERGES.iter().map(|(c, _)| *c).collect();
    let not_run: Vec<&&str> = declared.iter().filter(|d| !c.ran.contains(**d)).collect();
    assert!(
        not_run.is_empty(),
        "declared divergent but no longer compared: {not_run:?}"
    );
    let matching = c.ran.len() - c.diverging.len();
    let got = c.diverging;
    let unexplained: Vec<String> = got
        .iter()
        .filter(|(c, _)| !declared.contains(c.as_str()))
        .map(|(c, why)| format!("{c}: {why}"))
        .collect();
    let stale: Vec<&&str> = declared.iter().filter(|c| !got.contains_key(**c)).collect();
    assert!(
        unexplained.is_empty() && stale.is_empty(),
        "{matching} of {} compared cases match; differs from its golden:\n{}\ndeclared divergent but matches: {stale:?}",
        c.ran.len(),
        unexplained.join("\n")
    );
    assert!(
        matching >= MIN_MATCHING,
        "only {matching} served cases match their goldens, of {} compared",
        c.ran.len()
    );
}

fn at_now() -> VirtualClock {
    VirtualClock::new(NOW as u64 * 1_000)
}

#[test]
fn every_served_case_answers_as_its_golden_on_the_reference_backend() {
    assert_parity(diverging(|| store(MemBackend::new(), &at_now())));
}

#[test]
fn every_served_case_answers_as_its_golden_on_sqlite() {
    let dir = TempDir::new("spira-lc-parity");
    let n = std::cell::Cell::new(0);
    assert_parity(diverging(|| {
        n.set(n.get() + 1);
        let clock = at_now();
        let opts = Options {
            clock: Arc::new(clock.clone()),
            ..Options::default()
        };
        let path = dir.path().join(format!("case-{}.db", n.get()));
        store(SqliteBackend::open(&path, opts).unwrap(), &clock)
    }));
}

#[test]
fn the_parity_rig_reports_a_case_whose_answer_differs() {
    let c = diverging(|| {
        let s = store(MemBackend::new(), &at_now());
        // A record the seed never made, so `list` answers with one row more than its golden.
        let call = Call {
            now: NOW,
            request: "planted",
        };
        let argv: Vec<String> = ["create-bead", "sp-planted"].map(String::from).into();
        assert_eq!(super::run(&s, &argv, &call).0, 0);
        s
    });
    assert!(
        c.diverging.contains_key("list-state-verb"),
        "a planted READY record changes list-state READY: {:?}",
        c.diverging.keys().collect::<Vec<_>>()
    );
}

fn bare(argv: &[String]) -> Step {
    Step {
        argv: argv.to_vec(),
        stdin: None,
        want: Answer {
            rc: 0,
            stdout: String::new(),
            stderr: String::new(),
        },
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

#[test]
fn the_parity_rig_sees_a_wrong_answer() {
    let s = store(MemBackend::new(), &at_now());
    seeded(&s);
    let mut rig = Rig {
        store: &s,
        n: 1_000,
    };
    let argv: Vec<String> = ["state", "sp-working"].map(String::from).into();
    assert_eq!(run(&mut rig, &bare(&argv)).stdout, "WORKING\n");
    assert_ne!(run(&mut rig, &bare(&argv)).stdout, "READY\n");
}
