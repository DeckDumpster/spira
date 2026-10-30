//! For one fixture world, design-asks.sh and the design-asks handler leave the same rows in the
//! Concierge inbox. The script is the oracle: it runs unmodified against a stubbed `bd` and
//! `spira-lc`; the handler runs on a virtual clock against a fake spira-lc socket serving the same
//! files. The one thing the handler cannot do is the script's `git log` fallback for a page's
//! writer: no door to git exists, so a page naming no writer is told as written by unknown.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use runtime::host::EffectRunner;
use runtime::ports::VirtualClock;
use runtime::{Engine, Executor, MemJournal, Outcome};
use spira_runtime::watchers::design_asks::DesignAsks;
use spira_runtime::world::World;
use spira_runtime::Effects;

const LABEL: &str = "ask-parity";
const REPEAT: u64 = 7200;
const DESIGNS: &str = "wiki/projects/zed/designs";

struct Fixture {
    dir: testkit::TempDir,
}

impl Fixture {
    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn build(name: &str) -> Fixture {
        let f = Fixture { dir: testkit::TempDir::new(&format!("design-asks-{name}")) };
        for d in ["bin", "run/watchd", "mail/concierge/new", "mail/concierge/cur", "brain/wiki/projects/zed/designs"] {
            fs::create_dir_all(f.p(d)).unwrap();
        }
        fs::write(f.p("asks.json"), "[]").unwrap();
        fs::write(f.p("held.txt"), "").unwrap();
        let root = f.dir.path().display();
        testkit::write_exe(f.p("bin/bd"), &format!("#!/usr/bin/env bash\n[ -e {root}/bdfail ] && exit 1\ncat {root}/asks.json\n"));
        testkit::write_exe(f.p("bin/spira-lc"), &format!("#!/usr/bin/env bash\ncase \"$1\" in\n  list-held) cat {root}/held.txt ;;\nesac\nexit 0\n"));
        fs::write(
            f.p("layer.toml"),
            format!(
                "[spira]\nrun = \"{}\"\nask_label = \"{LABEL}\"\nmail = \"{}\"\nbd = \"{}\"\nconcierge_inbox = \"{}\"\nwiki = \"{}\"\ndesign_asks_repeat = {REPEAT}\n",
                f.p("run").display(),
                f.p("mail").display(),
                f.p("bin/bd").display(),
                f.p("old-inbox").display(),
                f.p("brain").display()
            ),
        )
        .unwrap();
        f
    }

    fn design(&self, name: &str, frontmatter: &str, body: &str) {
        fs::write(self.p(&format!("brain/{DESIGNS}/{name}.md")), format!("---\n{frontmatter}---\n{body}\n")).unwrap();
    }

    fn asks(&self, text: &str) {
        fs::write(self.p("asks.json"), format!(r#"[{{"id": "sp-a", "title": "t", "description": "{text}", "notes": ""}}]"#)).unwrap();
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
                let once = root.join("bdfail-once");
                let down = (root.join("bdfail").exists() || fs::remove_file(&once).is_ok()) && argv[0] == "content";
                let file = match (argv[0].as_str(), argv.get(3).map(String::as_str)) {
                    ("content", Some(l)) if argv[1] == "list" && l == LABEL && argv[4..6] == ["--status", "open"] => "asks.json",
                    ("list-held", _) => "held.txt",
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

fn old_rows(f: &Fixture, ticks: usize) -> Vec<String> {
    assert!(debug_dir().join("spira-config").exists(), "build spira-config first: design-asks.sh resolves its settings through it");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/design-asks.sh");
    let path = format!("{}:{}:/usr/bin:/bin", f.p("bin").display(), debug_dir().display());
    let status = Command::new("bash")
        .arg(&script)
        .args(["watch", "--interval", "1", "--ticks", &ticks.to_string()])
        .env_clear()
        .env("PATH", path)
        .env("HOME", f.dir.path())
        .env("SPIRA_TOML", format!("{}:{}", Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira-config/tests/fixtures/complete.toml").display(), f.p("layer.toml").display()))
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "design-asks.sh failed");
    rows(&f.p("old-inbox"))
}

fn new_rows(f: &Fixture, ticks: usize) -> Vec<String> {
    let world = World { run_dir: f.p("run"), inbox: f.p("new-inbox"), lc_socket: f.serve_lc(), mail: f.p("mail"), ask_label: LABEL.into(), wiki: f.p("brain"), ..World::default() };
    let fx = Effects { world, ..Effects::default() };
    let clock = VirtualClock::new(1_800_000_000_000);
    let mut e = Engine::open(MemJournal::new(), Arc::new(clock.clone()), vec![DesignAsks { interval_ms: 1000, repeat_secs: REPEAT }.registration()], 0).unwrap();
    let (id, stream, kind) = DesignAsks::boot();
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
    rows(&f.p("new-inbox"))
}

/// The script names the store by its path, which the runtime has none to name; fixture roots differ per run.
/// The handler also says why the store failed, which the script never did.
fn unnamed_store(f: &Fixture, rows: Vec<String>) -> Vec<String> {
    let root = f.dir.path().display().to_string();
    rows.into_iter().map(|r| r.replace(&root, "ROOT")).map(|r| match (r.find("could not be read: "), r.find("; designs")) { (Some(a), Some(b)) if a < b => format!("{}{}", &r[..a + "could not be read".len()], &r[b..]), _ => r }).map(|r| match (r.find("the ask store ("), r.find(") could not")) { (Some(a), Some(b)) => format!("{}the ask store{}", &r[..a], &r[b + 1..]), _ => r }).collect()
}

fn same_rows(name: &str, prep: impl Fn(&Fixture)) -> Vec<String> {
    let f = Fixture::build(name);
    prep(&f);
    let old = unnamed_store(&f, old_rows(&f, 3));
    let g = Fixture::build(name);
    prep(&g);
    let new = unnamed_store(&g, new_rows(&g, 3));
    assert_eq!(new, old, "scenario {name}: the handler and design-asks.sh disagree");
    old
}

#[test]
fn an_unasked_draft_is_told_once_with_its_writer_and_an_approved_one_never() {
    let r = same_rows("told", |f| {
        f.design("unasked", "status: draft\nbead: sp-writer1\n", "# x");
        f.design("done", "status: approved\nbead: sp-writer3\n", "# x");
    });
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].contains(&format!("[watch:design-asks] DESIGN WITHOUT ASK: {DESIGNS}/unasked.md is draft and has had no open ask for 0h (written by sp-writer1)")), "{r:?}");
}

#[test]
fn the_writer_falls_through_bead_session_author_and_then_unknown() {
    let r = same_rows("writer", |f| {
        f.design("a", "status: in-review\nsession: \"sess-9\"\nauthor: someone\n", "");
        f.design("b", "status: draft\nauthor: 'someone'\n", "");
        f.design("c", "status: draft\n", "");
    });
    assert_eq!(r.len(), 3, "{r:?}");
    for want in ["written by sess-9", "written by someone", "written by unknown"] {
        assert!(r.iter().any(|l| l.contains(want)), "{want} in {r:?}");
    }
}

#[test]
fn an_ask_naming_the_path_slug_or_artifact_url_covers_the_page() {
    let r = same_rows("covered", |f| {
        f.design("bypath", "status: draft\n", "");
        f.design("byslug", "status: draft\n", "");
        f.design("byurl", "status: draft\n", "see https://claude.ai/artifact/AAAA1111 here");
        f.design("other", "status: draft\n", "https://claude.ai/artifact/BBBB2222");
        f.asks(&format!("approve {DESIGNS}/bypath.md and byslug and https://claude.ai/artifact/AAAA1111"));
    });
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].contains("other.md"), "{r:?}");
}

#[test]
fn the_mail_of_a_held_ask_covers_a_page_too() {
    let r = same_rows("held", |f| {
        f.design("heldpage", "status: draft\n", "");
        f.design("bare", "status: draft\n", "");
        fs::write(f.p("held.txt"), "sp-held\n").unwrap();
        fs::write(f.p("mail/concierge/new/m1"), format!("Subject: s\nX-Spira-Work-Bead: sp-held\n\nplease approve {DESIGNS}/heldpage.md\n")).unwrap();
    });
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].contains("bare.md"), "{r:?}");
}

#[test]
fn a_store_that_never_answers_is_a_failure_said_once_and_not_an_empty_brain() {
    let r = same_rows("down", |f| {
        f.design("unasked", "status: draft\nbead: sp-w\n", "");
        fs::write(f.p("bdfail"), "").unwrap();
    });
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].contains("DESIGN-ASKS FAILED: the ask store"), "{r:?}");
}

#[test]
fn a_failed_store_call_names_the_command_and_what_the_store_said() {
    let g = Fixture::build("cause");
    g.design("unasked", "status: draft\nbead: sp-w\n", "");
    fs::write(g.p("bdfail"), "").unwrap();
    let r = new_rows(&g, 1);
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].contains("spira-lc content list --label ask-parity"), "{r:?}");
    assert!(r[0].contains("boom"), "{r:?}");
}

#[test]
fn a_store_that_fails_once_and_then_answers_raises_nothing() {
    let g = Fixture::build("restart");
    g.design("covered", "status: draft\n", "");
    g.asks(&format!("approve {DESIGNS}/covered.md"));
    fs::write(g.p("bdfail-once"), "").unwrap();
    let r = new_rows(&g, 1);
    assert!(r.is_empty(), "{r:?}");
    assert!(!g.p("bdfail-once").exists(), "the store was never asked");
}

#[test]
fn a_notice_before_the_ask_list_is_not_a_failure() {
    let g = Fixture::build("notice");
    g.design("unasked", "status: draft\nbead: sp-w\n", "");
    fs::write(g.p("asks.json"), "warning: slow store\n[]").unwrap();
    let r = new_rows(&g, 1);
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].contains("DESIGN WITHOUT ASK"), "{r:?}");
}

#[test]
fn a_missing_brain_is_a_failure_said_once() {
    let r = same_rows("nobrain", |f| fs::remove_dir_all(f.p("brain/wiki/projects")).unwrap());
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].contains("is not a directory"), "{r:?}");
}

#[test]
fn a_world_with_no_designs_says_nothing() {
    assert!(same_rows("quiet", |_| {}).is_empty());
}

#[test]
fn only_the_git_log_fallback_for_a_writer_diverges_from_the_script() {
    let prep = |f: &Fixture| {
        f.design("nowriter", "status: draft\n", "");
        let git = |args: &[&str]| assert!(Command::new("git").args(["-C", f.p("brain").to_str().unwrap()]).args(args).env_clear().env("GIT_AUTHOR_NAME", "a").env("GIT_AUTHOR_EMAIL", "a@a").env("GIT_COMMITTER_NAME", "a").env("GIT_COMMITTER_EMAIL", "a@a").env("PATH", "/usr/bin:/bin").status().unwrap().success());
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "draft the nowriter design"]);
    };
    let f = Fixture::build("gitwriter");
    prep(&f);
    let old = old_rows(&f, 1);
    let g = Fixture::build("gitwriter");
    prep(&g);
    let new = new_rows(&g, 1);
    assert!(old[0].contains("(written by draft the nowriter design)"), "{old:?}");
    assert!(new[0].contains("(written by unknown)"), "no door to git: {new:?}");
}

#[test]
fn a_hidden_project_directory_is_not_read_for_designs() {
    let r = same_rows("hidden", |f| {
        let dir = f.p("brain/wiki/projects/.x/designs");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("p.md"), "---\nstatus: draft\nbead: sp-w\n---\n# x\n").unwrap();
    });
    assert!(r.is_empty(), "{r:?}");
}
