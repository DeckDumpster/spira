//! For one fixture world, supersede-duty.sh and the supersede-duty handler leave the same rows in
//! the Concierge inbox and make the same writes. The script is the oracle: it runs unmodified
//! against stubbed `spira-lc`, `bdq` and `bd`; the handler runs on a virtual clock against a fake
//! spira-lc socket serving the same state. The one write the handler cannot make is `bd supersede`:
//! spira-lc's socket has no door to it, so the oracle's record of it is set aside.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use runtime::host::EffectRunner;
use runtime::ports::VirtualClock;
use runtime::{Engine, Executor, Outcome};
use spira_runtime::watchers::supersede_duty::SupersedeDuty;
use spira_runtime::world::World;
use spira_runtime::Effects;

fn hold(cause: &str, detail: &str) -> String {
    let ev = format!(r#"{{"Hold":{{"kind":"manual","cause":"{cause}","detail":"{detail}"}}}}"#)
        .replace('"', "\\\"");
    format!(r#"[{{"seq":"4","event":"Hold","applied":"1","evidence":"{ev}"}}]"#)
}

/// (bead, its state, its standing manual hold as (cause, detail))
type Bead = (&'static str, &'static str, Option<(&'static str, &'static str)>);
const WORLD: &[Bead] = &[
    ("sp-land", "WORKING", Some(("supersede-request", "sp-succ-landed"))),
    ("sp-open", "WORKING", Some(("supersede-request", "sp-succ-open"))),
    ("sp-self", "REWORK", Some(("supersede-request", "sp-self"))),
    ("sp-ghost", "WORKING", Some(("supersede-request", "sp-nowhere"))),
    ("sp-done", "LANDED", Some(("supersede-request", "sp-succ-landed"))),
    ("sp-ask", "WORKING", Some(("ask", "which repo?"))),
];

const SUCCESSORS: &[(&str, &str)] = &[("sp-succ-landed", "LANDED"), ("sp-succ-open", "WORKING")];

struct Fixture {
    dir: testkit::TempDir,
}

impl Fixture {
    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn build(name: &str) -> Fixture {
        let f = Fixture {
            dir: testkit::TempDir::new(&format!("sd-{name}")),
        };
        for d in ["bin", "run/watchd", "state", "history"] {
            fs::create_dir_all(f.p(d)).unwrap();
        }
        let mut held = vec![];
        for (id, own, hist) in WORLD {
            fs::write(f.p(&format!("state/{id}")), own).unwrap();
            if let Some((cause, detail)) = hist {
                fs::write(f.p(&format!("history/{id}")), hold(cause, detail)).unwrap();
            }
            held.push(*id);
        }
        for (id, st) in SUCCESSORS {
            fs::write(f.p(&format!("state/{id}")), st).unwrap();
        }
        fs::write(f.p("held.txt"), held.join("\n") + "\n").unwrap();
        let root = f.dir.path().display();
        testkit::write_exe(
            f.p("bin/spira-lc"),
            &format!(
                "#!/usr/bin/env bash\ncase \"$1\" in\n  list) cat {root}/held.txt | while read -r id; do echo \"{{\\\"bead_id\\\":\\\"$id\\\",\\\"version\\\":\\\"v1\\\"}}\"; done | paste -sd, | sed 's/^/[/; s/$/]/' ;;\n  history) cat {root}/history/$2 ;;\n  state) [ -s {root}/state/$2 ] && cat {root}/state/$2 ;;\n  unhold|supersede) [ -e {root}/fail-$1 ] && exit 1; echo \"$*\" >> {root}/calls; grep -vx \"$2\" {root}/held.txt > {root}/held.tmp; mv {root}/held.tmp {root}/held.txt; [ $1 = supersede ] && echo SUPERSEDED > {root}/state/$2; exit 0 ;;\n  *) exit 1 ;;\nesac\n"
            ),
        );
        testkit::write_exe(f.p("bin/bdq"), &format!("#!/usr/bin/env bash\n[ \"$1\" = note ] && echo \"note $2 $3\" >> {root}/calls\nexit 0\n"));
        testkit::write_exe(
            f.p("bin/bd"),
            &format!("#!/usr/bin/env bash\necho \"bd $*\" >> {root}/bd-calls\nexit 0\n"),
        );
        fs::write(
            f.p("layer.toml"),
            format!(
                "[spira]\nrun = \"{}\"\nbd = \"{}\"\nconcierge_inbox = \"{}\"\n",
                f.p("run").display(),
                f.p("bin/bd").display(),
                f.p("old-inbox").display()
            ),
        )
        .unwrap();
        f
    }

    fn serve_lc(&self) -> PathBuf {
        let sock = self.p("lc.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let root = self.dir.path().to_path_buf();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                let argv: Vec<String> = serde_json::from_str(&line).unwrap();
                let read = |rel: String| fs::read_to_string(root.join(rel));
                let (code, body) = match argv[0].as_str() {
                    "list" => {
                        let ids = read("held.txt".into()).unwrap_or_default();
                        let rows: Vec<String> = ids
                            .lines()
                            .map(|id| format!(r#"{{"bead_id":"{id}","version":"v1"}}"#))
                            .collect();
                        (0, format!("[{}]", rows.join(",")))
                    }
                    "history" => {
                        read(format!("history/{}", argv[1])).map_or((1, String::new()), |b| (0, b))
                    }
                    "state" => read(format!("state/{}", argv[1]))
                        .ok()
                        .filter(|s| !s.is_empty())
                        .map_or((3, String::new()), |b| (0, b)),
                    "unhold" | "supersede" if root.join(format!("fail-{}", argv[0])).exists() => {
                        (1, "refused".into())
                    }
                    "unhold" | "supersede" => {
                        let mut calls = fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(root.join("calls"))
                            .unwrap();
                        writeln!(calls, "{}", argv.join(" ")).unwrap();
                        let ids = read("held.txt".into()).unwrap_or_default();
                        fs::write(
                            root.join("held.txt"),
                            ids.lines()
                                .filter(|l| *l != argv[1])
                                .map(|l| format!("{l}\n"))
                                .collect::<String>(),
                        )
                        .unwrap();
                        if argv[0] == "supersede" {
                            fs::write(root.join(format!("state/{}", argv[1])), "SUPERSEDED")
                                .unwrap();
                        }
                        (0, String::new())
                    }
                    "content" if argv[1..3] == ["comments", "add"] => {
                        let mut calls = fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(root.join("calls"))
                            .unwrap();
                        writeln!(calls, "note {} {}", argv[3], argv[4]).unwrap();
                        (0, String::new())
                    }
                    _ => (2, "no such door".into()),
                };
                let mut w = &stream;
                writeln!(w, "{{\"exit_code\": {code}, \"len\": {}}}", body.len()).unwrap();
                w.write_all(body.as_bytes()).unwrap();
            }
        });
        sock
    }
}

fn debug_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent().unwrap().parent().unwrap().to_path_buf()
}

fn lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// The rows by bead, in the order each bead's own rows came: the handler's writes settle
/// concurrently, so rows of different beads interleave differently from the script's.
fn rows(inbox: &Path) -> Vec<String> {
    let mut rows: Vec<String> = lines(inbox)
        .iter()
        .map(|l| l.split_once(' ').unwrap().1.to_string())
        .collect();
    rows.sort_by_key(|r| r.split_whitespace().nth(3).unwrap_or("").to_string());
    rows
}

/// Inbox rows and the sorted writes made.
type Outcome2 = (Vec<String>, Vec<String>);

fn old_run(f: &Fixture, ticks: usize) -> Outcome2 {
    assert!(
        debug_dir().join("spira-config").exists(),
        "build spira-config first: supersede-duty.sh resolves its settings through it"
    );
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/supersede-duty.sh");
    let path = format!(
        "{}:{}:/usr/bin:/bin",
        f.p("bin").display(),
        debug_dir().display()
    );
    let status = Command::new("bash")
        .arg(&script)
        .args(["watch", "--interval", "1", "--ticks", &ticks.to_string()])
        .env_clear()
        .env("PATH", path)
        .env("HOME", f.dir.path())
        .env(
            "SPIRA_TOML",
            format!(
                "{}:{}",
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../spira-config/tests/fixtures/complete.toml")
                    .display(),
                f.p("layer.toml").display()
            ),
        )
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "supersede-duty.sh failed");
    (rows(&f.p("old-inbox")), calls(f))
}

fn calls(f: &Fixture) -> Vec<String> {
    lines(&f.p("calls"))
}

fn new_run(f: &Fixture, ticks: usize) -> Outcome2 {
    let world = World {
        run_dir: f.p("run"),
        inbox: f.p("new-inbox"),
        lc_socket: f.serve_lc(),
        ..World::default()
    };
    let fx = Effects {
        world,
        ..Effects::default()
    };
    let clock = VirtualClock::new(1_800_000_000_000);
    let mut e = Engine::open(
        runtime::MemJournal::new(),
        Arc::new(clock.clone()),
        vec![SupersedeDuty { interval_ms: 1000 }.registration()],
        0,
    )
    .unwrap();
    let (id, stream, kind) = SupersedeDuty::boot();
    e.submit(id, stream, kind).unwrap();
    for _ in 0..ticks {
        e.run_until_idle().unwrap();
        loop {
            let pending = e.pending_effects();
            if pending.is_empty() {
                break;
            }
            for p in pending {
                let out = match fx.run(&p.key, &p.spec) {
                    Ok(d) => Outcome::Succeeded(d),
                    Err(d) => Outcome::Failed(d),
                };
                e.complete(&p.key, out).unwrap();
            }
            e.run_until_idle().unwrap();
        }
        clock.advance(1000);
    }
    (rows(&f.p("new-inbox")), calls(f))
}

fn same(name: &str, ticks: usize, prep: impl Fn(&Fixture)) -> Outcome2 {
    let f = Fixture::build(name);
    prep(&f);
    let old = old_run(&f, ticks);
    let bd = lines(&f.p("bd-calls"));
    assert!(bd.iter().all(|c| c.contains(" supersede ")), "the oracle's only bd call is supersede: {bd:?}");
    assert_eq!(bd.len(), old.1.iter().filter(|w| w.starts_with("supersede ")).count(), "one bd supersede per supersede the lifecycle took: {bd:?}");
    let g = Fixture::build(name);
    prep(&g);
    let new = new_run(&g, ticks);
    assert!(lines(&g.p("bd-calls")).is_empty(), "the handler launches nothing");
    assert_eq!(
        new, old,
        "scenario {name}: the handler and supersede-duty.sh disagree"
    );
    old
}

#[test]
fn every_kind_of_request_is_adjudicated_as_the_script_does() {
    let (say, writes) = same("all", 2, |_| {});
    let once = |needle: &str| say.iter().filter(|l| l.contains(needle)).count();
    assert_eq!(
        once("SUPERSEDE REQUEST sp-land: confirmed, superseded by LANDED sp-succ-landed"),
        1,
        "{say:?}"
    );
    assert_eq!(
        once("SUPERSEDE REQUEST sp-open: refused, sp-succ-open is WORKING; unheld"),
        1,
        "{say:?}"
    );
    assert_eq!(
        once(
            "SUPERSEDE REQUEST sp-self: refused, the successor is the bead itself (REWORK); unheld"
        ),
        1,
        "{say:?}"
    );
    assert_eq!(
        once("SUPERSEDE REQUEST sp-ghost: refused, successor sp-nowhere has no usable row; unheld"),
        1,
        "{say:?}"
    );
    assert!(
        say.iter()
            .all(|l| !l.contains("sp-done") && !l.contains("sp-ask")),
        "{say:?}"
    );
    assert_eq!(
        writes.iter().filter(|w| w.starts_with("note ")).count(),
        4,
        "{writes:?}"
    );
    assert!(
        writes.contains(&"supersede sp-land sp-succ-landed supersede-duty".to_string()),
        "{writes:?}"
    );
}

#[test]
fn a_failing_write_is_reported_once_across_repeated_ticks() {
    let (say, writes) = same("failing", 3, |f| fs::write(f.p("fail-unhold"), "").unwrap());
    assert_eq!(
        say.iter()
            .filter(|l| l.ends_with("SUPERSEDE REQUEST sp-open: unhold failed — will retry"))
            .count(),
        1,
        "{say:?}"
    );
    assert_eq!(
        say.iter()
            .filter(|l| l.contains("SUPERSEDE REQUEST sp-open: successor"))
            .count(),
        1,
        "{say:?}"
    );
    assert!(
        writes.iter().all(|w| !w.starts_with("unhold")),
        "{writes:?}"
    );
}

#[test]
fn a_store_that_does_not_answer_says_nothing() {
    let (say, writes) = same("down", 2, |f| fs::remove_file(f.p("held.txt")).unwrap());
    assert!(say.is_empty() && writes.is_empty(), "{say:?} {writes:?}");
}

#[test]
fn the_watchers_manifest_no_longer_carries_the_row() {
    let manifest = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/watchers")).unwrap();
    assert!(manifest.lines().any(|l| l.starts_with("dwarf-watch|")), "the scan must be able to see a row");
    assert!(!manifest.lines().any(|l| l.starts_with("supersede-duty|")), "spira-runtime hosts supersede-duty");
}
