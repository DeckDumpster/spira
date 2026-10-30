//! For one fixture world, asks.sh and the asks handler leave the same rows in the Concierge
//! inbox. The script is the oracle: it runs unmodified against a stubbed `bd` and `spira-lc`;
//! the handler runs on a virtual clock against a fake spira-lc socket serving the same files.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use runtime::host::EffectRunner;
use runtime::ports::VirtualClock;
use runtime::{Engine, Executor, MemJournal, Outcome};
use spira_runtime::watchers::asks::Asks;
use spira_runtime::world::World;
use spira_runtime::Effects;

const LABEL: &str = "ask-parity";
const QUOTED_TITLE_JSON: &str = r#"say \"hi\" in C:\\dir"#;
const HISTORY: &str = r#"[{"seq":"7","event":"Hold","applied":"1","evidence":"{\"Hold\":{\"kind\":\"manual\",\"detail\":\"no build spec:   which repo?\"}}"}]"#;

struct Fixture {
    dir: testkit::TempDir,
}

impl Fixture {
    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn build(name: &str) -> Fixture {
        let f = Fixture { dir: testkit::TempDir::new(&format!("asks-{name}")) };
        for d in ["bin", "run/watchd", "mail/concierge/new", "mail/concierge/cur"] {
            fs::create_dir_all(f.p(d)).unwrap();
        }
        f.set_asks(&[("sp-old", "already open")]);
        fs::write(f.p("held.txt"), "").unwrap();
        fs::write(f.p("history.json"), HISTORY).unwrap();
        fs::write(f.p("mail/concierge/new/m9"), "Subject: Which branch?\nX-Spira-Default: take main\nX-Spira-Work-Bead: sp-held\nMessage-ID: <m9@spira>\n\nbody\n").unwrap();
        let root = f.dir.path().display();
        testkit::write_exe(f.p("bin/bd"), &format!("#!/usr/bin/env bash\n[ -e {root}/bdfail ] && exit 1\ncat {root}/asks.json\n"));
        testkit::write_exe(f.p("bin/spira-lc"), &format!("#!/usr/bin/env bash\ncase \"$1\" in\n  list-held) cat {root}/held.txt ;;\n  history) cat {root}/history.json ;;\nesac\nexit 0\n"));
        fs::write(
            f.p("layer.toml"),
            format!("[spira]\nrun = \"{}\"\nask_label = \"{LABEL}\"\nmail = \"{}\"\nbd = \"{}\"\nconcierge_inbox = \"{}\"\n", f.p("run").display(), f.p("mail").display(), f.p("bin/bd").display(), f.p("old-inbox").display()),
        )
        .unwrap();
        f
    }

    fn set_asks(&self, asks: &[(&str, &str)]) {
        let rows: Vec<String> = asks.iter().map(|(i, t)| format!(r#"{{"id": "{i}", "title": "{t}"}}"#)).collect();
        fs::write(self.p("asks.json"), format!("[{}]", rows.join(","))).unwrap();
    }

    fn change_world(&self) {
        let long = "x".repeat(200);
        self.set_asks(&[("sp-old", "already open"), ("sp-new", "decide X"), ("sp-long", &long), ("sp-quote", QUOTED_TITLE_JSON)]);
        fs::write(self.p("held.txt"), "sp-held\nsp-nomail\n").unwrap();
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
                let down = root.join("bdfail").exists() && argv[0] == "content";
                let file = match (argv[0].as_str(), argv.get(3).map(String::as_str)) {
                    ("content", Some(l)) if argv[1] == "list" && l == LABEL => "asks.json",
                    ("list-held", _) => "held.txt",
                    ("history", _) => "history.json",
                    _ => "",
                };
                let body = if down { "boom".into() } else { fs::read_to_string(root.join(file)).unwrap_or_default() };
                let mut w = &stream;
                writeln!(w, "{{\"exit_code\": {}, \"len\": {}}}", i32::from(down), body.len()).unwrap();
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

fn rows(inbox: &Path) -> Vec<String> {
    fs::read_to_string(inbox).unwrap_or_default().lines().map(|l| l.split_once(' ').unwrap().1.to_string()).collect()
}

fn old_rows(f: &Fixture, ticks: usize, change: bool) -> Vec<String> {
    assert!(debug_dir().join("spira-config").exists(), "build spira-config first: asks.sh resolves its settings through it");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/asks.sh");
    let path = format!("{}:{}:/usr/bin:/bin", f.p("bin").display(), debug_dir().display());
    let mut child = Command::new("bash")
        .arg(&script)
        .args(["watch", "--interval", "1", "--ticks", &ticks.to_string()])
        .env_clear()
        .env("PATH", path)
        .env("HOME", f.dir.path())
        .env("SPIRA_TOML", format!("{}:{}", Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira-config/tests/fixtures/complete.toml").display(), f.p("layer.toml").display()))
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    if change {
        let first_tick = f.p("run/watchd/asks.health");
        let mut waited = 0;
        while !first_tick.exists() {
            assert!(waited < 2000, "asks.sh never finished its first tick");
            std::thread::sleep(std::time::Duration::from_millis(5)); // clock-safe: caps the wait for the script's first-tick health file
            waited += 1;
        }
        f.change_world();
    }
    assert!(child.wait().unwrap().success(), "asks.sh failed");
    rows(&f.p("old-inbox"))
}

fn new_rows(f: &Fixture, ticks: usize, change: bool) -> Vec<String> {
    let world = World { run_dir: f.p("run"), inbox: f.p("new-inbox"), lc_socket: f.serve_lc(), mail: f.p("mail"), ask_label: LABEL.into(), ..World::default() };
    let fx = Effects { world, ..Effects::default() };
    let clock = VirtualClock::new(1_800_000_000_000);
    let mut e = Engine::open(MemJournal::new(), Arc::new(clock.clone()), vec![Asks { interval_ms: 1000 }.registration()], 0).unwrap();
    let (id, stream, kind) = Asks::boot();
    e.submit(id, stream, kind).unwrap();
    for tick in 0..ticks {
        if tick == 1 && change {
            f.change_world();
        }
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
    rows(&f.p("new-inbox"))
}

fn same_rows(name: &str, ticks: usize, change: bool) -> Vec<String> {
    same_rows_with(name, ticks, change, |_| {})
}

fn same_rows_with(name: &str, ticks: usize, change: bool, prep: impl Fn(&Fixture)) -> Vec<String> {
    let f = Fixture::build(name);
    prep(&f);
    let old = old_rows(&f, ticks, change);
    let g = Fixture::build(name);
    prep(&g);
    let new = new_rows(&g, ticks, change);
    assert_eq!(new, old, "scenario {name}: the handler and asks.sh disagree");
    old
}

#[test]
fn a_baseline_ask_is_quiet_and_a_newly_opened_one_is_announced_once() {
    let r = same_rows("new", 3, true);
    assert_eq!(r.iter().filter(|l| l.contains("[watch:asks] NEW ASK sp-new: decide X")).count(), 1, "{r:?}");
    assert!(r.iter().all(|l| !l.contains("sp-old")), "{r:?}");
}

#[test]
fn a_held_ask_is_announced_with_its_mail_or_with_how_to_answer_without_one() {
    let r = same_rows("hold", 3, true);
    assert!(r.iter().any(|l| l.contains("NEW ASK HOLD sp-held: Which branch? | default: take main | message-id: <m9@spira>")), "{r:?}");
    assert!(r.iter().any(|l| l.contains("NEW ASK HOLD sp-nomail: no build spec: which repo? | NO MAIL for the question; answer with spira-lc reply sp-nomail seq:7")), "{r:?}");
}

#[test]
fn a_world_that_never_changes_says_nothing() {
    assert!(same_rows("quiet", 2, false).is_empty());
}

#[test]
fn a_title_is_cut_with_its_id_to_170_characters() {
    let r = same_rows("long", 3, true);
    let line = r.iter().find(|l| l.contains("NEW ASK sp-long")).expect("the long ask is announced");
    assert!(line.ends_with(&"x".repeat(169 - "sp-long".len())), "{line}");
}

#[test]
fn a_store_that_never_answers_announces_nothing() {
    let r = same_rows_with("down", 3, false, |f| fs::write(f.p("bdfail"), "").unwrap());
    assert!(r.is_empty(), "{r:?}");
}

#[test]
fn a_title_with_a_quote_or_backslash_is_announced_as_its_text_not_its_json() {
    let r = same_rows("quoted", 3, true);
    assert!(r.iter().any(|l| l.ends_with(r#"NEW ASK sp-quote: say "hi" in C:\dir"#)), "{r:?}");
}

#[test]
fn the_last_mail_naming_a_bead_wins_with_every_cur_after_every_new() {
    let r = same_rows_with("mailorder", 3, true, |f| {
        fs::create_dir_all(f.p("mail/other/new")).unwrap();
        let mail = |s: &str| format!("Subject: {s}\nX-Spira-Default: d\nX-Spira-Work-Bead: sp-held\nMessage-ID: <{s}@spira>\n\nbody\n");
        fs::write(f.p("mail/concierge/cur/a0"), mail("cur-first")).unwrap();
        fs::write(f.p("mail/other/new/z9"), mail("other-new-last")).unwrap();
    });
    assert!(r.iter().any(|l| l.contains("NEW ASK HOLD sp-held: cur-first |")), "{r:?}");
}
