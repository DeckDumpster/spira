//! The bead-state verbs against the dolt path's goldens (`tools/lc-parity/goldens`): the seed's
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

use super::{Call, VERBS};
use crate::callers::{self, Machine};
use crate::control::store::Store;
use crate::control::verbs;

/// The capture's pinned clock (`capture.sh`'s `NOW_EPOCH`); no golden moves it.
const NOW: i64 = 1_767_225_600;
const FLOOR_MS: u64 = 1_000;

/// Seed steps whose verbs the control store does not serve yet, beside the batch machine's
/// events. They are not replayed, so the state they would have built is absent: sp-blocked's
/// edge, the asks, both batches, the deliveries, and the fact.
const UNSERVED_SEED: &[&str] = &["dep-add", "create-ask", "cut", "land", "fact"];

/// Cases on served verbs whose goldens this store does not reproduce, and why.
const DIVERGES: &[(&str, &str)] = &[
    ("show-blocked", "blocked_by comes from readiness/<id>, and no served verb publishes the seed's dep-add edge into it"),
    ("show-delivering", "sp-delivering reached IN_DELIVERY through cut, which is not served; its delivery likewise"),
    ("show-landed", "sp-landed reached LANDED through land, which is not served; its delivery likewise"),
    ("list", "title and priority are planning-plane fields the control plane does not keep; and the unserved dep-add, cut and land"),
    ("list-live", "as list"),
    ("list-state", "title and priority are planning fields; sp-blocked's blocked_by is the unserved dep-add"),
    ("list-ids", "title and priority are planning fields"),
    ("list-all", "sp-delivering and sp-landed reached their states through the unserved cut and land"),
    ("claim-terminal", "sp-landed is LANDED only through the unserved land, so its refusal is not Terminal"),
    ("create-bead", "title and priority are planning fields; list --ids shows them"),
    ("deliver-push", "sp-delivering is IN_DELIVERY only through the unserved cut"),
    ("returned", "sp-delivering is IN_DELIVERY only through the unserved cut"),
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
    /// No served verb reads stdin, so a step that gives it some is not served.
    stdin: bool,
    want: Answer,
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
            stdin: base.with_extension("stdin").exists(),
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
    "close-on-land", "unclaim", "reconcile-closed", "migrate-asks", "reconcile-epics",
    "reconcile-content", "drop-orphans", "close", "drop-bead", "drop-row", "reopen",
    "bd-supersede", "bd-note", "content", "close-epic", "unpublished", "content-landed", "drop",
];

fn primitive(verb: &str) -> bool {
    VERBS.contains(&verb) || verbs::VERBS.contains(&verb)
}

/// The verbs under test: this module's and the caller verbs built on them. The feed verbs
/// answer a caller verb that reads them, and their own goldens are another module's to prove.
fn served_verb(verb: &str) -> bool {
    VERBS.contains(&verb) || (callers::is_verb(verb) && !BEFORE_CALLERS.contains(&verb))
}

/// Whether the control store answers this step: a served verb, on the bead machine when it
/// is an event, given no stdin.
fn served(step: &Step) -> bool {
    let argv = &step.argv;
    let machine = argv.get(1).map(String::as_str);
    !step.stdin && served_verb(&argv[0]) && (argv[0] != "event" || machine == Some("bead"))
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
            Some(v) if VERBS.contains(&v) => super::run(
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
/// verb's streams as `emit` writes them.
fn run<B: Backend>(rig: &mut Rig<'_, B>, argv: &[String]) -> Answer {
    let verb = argv[0].as_str();
    if primitive(verb) {
        let (rc, out) = rig.call(argv);
        let stdout = if out.is_empty() { out } else { format!("{out}\n") };
        return Answer {
            rc,
            stdout,
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
        stdout,
        stderr: a.stderr,
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
        let verb = step.argv[0].as_str();
        let batch_event = verb == "event" && step.argv.get(1).map(String::as_str) == Some("batch");
        if UNSERVED_SEED.contains(&verb) || batch_event {
            continue;
        }
        assert!(served(step), "seed step {} {:?} is neither served nor declared unserved", i + 1, step.argv);
        assert_eq!(run(&mut rig, &step.argv), step.want, "seed step {}: {:?}", i + 1, step.argv);
    }
}

/// Every case whose steps are all served, each on a fresh seeded store from `open`: the ones
/// whose answers differ from their goldens, with the first difference.
fn diverging<B: Backend>(open: impl Fn() -> Store<B>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(goldens())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().is_some_and(|n| n != "_seed") && p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let case = dir.file_name().unwrap().to_string_lossy().into_owned();
        let steps = steps(&dir);
        if !steps.iter().all(served) {
            continue;
        }
        let s = open();
        seeded(&s);
        let mut rig = Rig { store: &s, n: 1_000 };
        for (i, step) in steps.iter().enumerate() {
            let got = run(&mut rig, &step.argv);
            if got != step.want {
                out.insert(
                    case.clone(),
                    format!("step {} {:?}: got {got:?}, want {:?}", i + 1, step.argv, step.want),
                );
                break;
            }
        }
    }
    out
}

fn assert_parity(got: BTreeMap<String, String>) {
    let declared: BTreeSet<&str> = DIVERGES.iter().map(|(c, _)| *c).collect();
    let unexplained: Vec<String> = got
        .iter()
        .filter(|(c, _)| !declared.contains(c.as_str()))
        .map(|(c, why)| format!("{c}: {why}"))
        .collect();
    let stale: Vec<&&str> = declared.iter().filter(|c| !got.contains_key(**c)).collect();
    assert!(
        unexplained.is_empty() && stale.is_empty(),
        "differs from its golden:\n{}\ndeclared divergent but matches: {stale:?}",
        unexplained.join("\n")
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
fn the_parity_rig_sees_a_wrong_answer() {
    let s = store(MemBackend::new(), &at_now());
    seeded(&s);
    let mut rig = Rig { store: &s, n: 1_000 };
    let argv: Vec<String> = ["state", "sp-working"].map(String::from).into();
    assert_eq!(run(&mut rig, &argv).stdout, "WORKING\n");
    assert_ne!(run(&mut rig, &argv).stdout, "READY\n");
}
