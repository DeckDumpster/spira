//! A standing condition filed from inside the scheduler: the store answers the scheduler's
//! reference read, so an open item silences the alarm and none raises it.
//! Real throwaway store and `spira-lc serve`; this process names itself `spira-runtime`.

#[macro_use]
extern crate spira_test_id;

use std::cell::Cell;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use watchtower::conditions::{reconcile, Cond, Ctx, Reading};
use watchtower::incident::AlarmOutcome;

fn tool(name: &str) -> Option<String> {
    let on = |dirs: &str| dirs.split(':').filter(|d| !d.is_empty()).map(|d| Path::new(d).join(name)).find(|p| p.is_file());
    let declared = spira_config::process::cfg("SPIRA_PATH").unwrap_or_default();
    let env_bin = std::env::var("HOME").map(|h| format!("{h}/spira-env/bin")).unwrap_or_default();
    on(&std::env::var("PATH").unwrap_or_default()).or_else(|| on(&declared)).or_else(|| on(&env_bin)).map(|p| p.display().to_string())
}

fn report(text: &str, key: &str) -> String {
    text.lines().find_map(|l| l.strip_prefix(&format!("{key}="))).unwrap_or_default().to_string()
}

struct Store {
    dir: testkit::TempDir,
    fixture: String,
    db: String,
    port: String,
}

impl Store {
    fn up() -> Option<Store> {
        let dir = testkit::TempDir::new("wtcs");
        let Some(dolt) = std::env::var("TESTDB_DOLT").ok().filter(|b| !b.is_empty()).or_else(|| tool("dolt")) else {
            eprintln!("SKIP: no dolt on PATH, spira.path or ~/spira-env/bin");
            return None;
        };
        let root = std::env::var("TESTDB_ROOT").ok().filter(|r| !r.is_empty()).unwrap_or_else(|| dir.join("db").display().to_string());
        let schema = testkit::manifest_dir().join("../spira-lc/beads-schema.sql");
        let out = Command::new(testkit::bin_exe("testenv")).args(["testdb", "up", "--tag", "conditions-on-store", "--dolt", &dolt, "--root", &root]).arg("--schema").arg(&schema).output().expect("testenv runs");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "testdb up: {text}{}", String::from_utf8_lossy(&out.stderr));
        Some(Store { dir, fixture: report(&text, "TESTDB_FIXTURE"), db: report(&text, "TESTDB_DIR"), port: report(&text, "TESTDB_SERVER_PORT") })
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = Command::new(testkit::bin_exe("testenv")).args(["testdb", "down", "--fixture", &self.fixture]).output();
    }
}

struct Lc {
    toml: PathBuf,
    home: PathBuf,
    dir: PathBuf,
    child: Option<Child>,
}

impl Lc {
    fn serving(store: &Store) -> Lc {
        let dir = store.dir.join("lc");
        let run = dir.join("run");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::create_dir_all(dir.join("release/bin")).unwrap();
        let harness = spira_config::resolve::locate_home_for_process().expect("SPIRA_HOME names the harness tree");
        let guards = dir.join("statute-guards");
        std::fs::write(&guards, "law-fixture-forbids-the-phrase | refuse | forbidden phrase\n").unwrap();
        let (runes, guards) = (harness.join("runes").display().to_string(), guards.display().to_string());
        let (d, r, ctl, sock) = (dir.display().to_string(), run.display().to_string(), dir.join("control.db").display().to_string(), dir.join("s").display().to_string());
        let (reader, writer) = (dir.join("reader.credential").display().to_string(), dir.join("writer.credential").display().to_string());
        let toml = spira_config::process::fixture_toml(
            &dir,
            &[
                ("SPIRA_WORKSPACES", &d),
                ("SPIRA_RUN", &r),
                ("SPIRA_LC_STORE_PATH", &ctl),
                ("SPIRA_LC_SOCKET", &sock),
                ("SPIRA_VICTORIALOGS_URL", "http://127.0.0.1:9"),
                ("SPIRA_DB", &store.db),
                ("SPIRA_BEADS_SQL_HOST", "127.0.0.1"),
                ("SPIRA_BEADS_SQL_PORT", &store.port),
                ("SPIRA_BEADS_SQL_USER", "spira_lc_ro"),
                ("SPIRA_BEADS_SQL_PASSWORD_FILE", &reader),
                ("SPIRA_BEADS_SQL_WRITER_PASSWORD_FILE", &writer),
                ("SPIRA_RUNES_DIR", &runes),
                ("SPIRA_STATUTE_GUARDS", &guards),
            ],
        );
        let home = spira_config::resolve::locate_home_for_process().expect("SPIRA_HOME names the harness tree");
        let mut lc = Lc { toml, home, dir, child: None };
        for verb in ["admin-create-store", "admin-provision-reader", "admin-provision-writer"] {
            let out = lc.command().arg(verb).output().expect("spira-lc runs");
            assert!(out.status.success(), "{verb}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        }
        let mut child = lc.command().arg("serve").stderr(Stdio::piped()).spawn().expect("spira-lc serve runs");
        let mut stderr = BufReader::new(child.stderr.take().unwrap());
        let (mut line, mut said) = (String::new(), String::new());
        while !line.contains(" ready after ") {
            line.clear();
            assert!(stderr.read_line(&mut line).unwrap() > 0, "the server stopped before it said ready: {said}");
            said.push_str(&line);
        }
        std::thread::spawn(move || std::io::copy(&mut stderr, &mut std::io::sink()));
        lc.child = Some(child);
        lc
    }

    fn command(&self) -> Command {
        let mut c = Command::new(testkit::bin_exe("spira-lc"));
        c.env_clear().env("PATH", std::env::var("PATH").unwrap_or_default()).env("SPIRA_TOML", &self.toml).env("SPIRA_HOME", &self.home).env("SPIRA_RELEASE", self.dir.join("release")).env("HOME", self.dir.join("home"));
        c
    }

    fn client(&self) -> spira_store::lc_call::LcClient {
        spira_store::lc_call::LcClient::socket(self.dir.join("s"))
    }
}

impl Drop for Lc {
    fn drop(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn name_self(comm: &str) {
    std::fs::write(format!("{}/comm", system_paths::PROC_SELF), comm).expect("this process renames itself");
}

fn standing(reference: &str) -> Cond {
    Cond { key: "drift".into(), reference: reference.into(), title: "t".into(), body: "b".into(), priority: 2, sustain_secs: 0 }
}

const REF: &str = "incident:lifecycle-content-drift";

#[test_id(tpdcn1g)]
#[test]
fn a_standing_condition_is_raised_once_when_the_store_holds_no_item_for_it_and_never_when_it_holds_one() {
    let Some(store) = Store::up() else { return };
    let lc = Lc::serving(&store);
    let client = lc.client();
    let run = testkit::TempDir::new("wtcs-run");
    let calls = Cell::new(0);
    let alarm = |_: &str, _: &watchtower::incident::Finding| {
        calls.set(calls.get() + 1);
        AlarmOutcome::Noted
    };
    let inc = run.join("incident.sh");
    std::fs::write(&inc, "#!/usr/bin/env bash\n").unwrap();
    let inc = inc.display().to_string();
    let ctx = || Ctx { alarm: &alarm, ..Ctx::new(run.path(), "db", "spira", &inc, &client, "no-lc-bin", 5) };

    name_self("spira-runtime");
    assert!(!watchtower::sweep::escalate::tracked_by_open_bead(&client, "lifecycle-content-drift.service"), "no item holds the unit");
    reconcile(1, &ctx(), "content-drift", Reading::Standing(vec![standing(REF)]));
    assert_eq!(calls.get(), 1, "no open item: the alarm is raised once");
    assert!(!run.join("conditions/unread/content-drift/drift").exists(), "the store answered, so no read failed");

    name_self("conditions_on_st");
    let body = "what the item says\n";
    let made = client.call_stdin(&["content", "create", "an incident", "--type", "task", "--labels", "repo:spira", "--external-ref", REF, "--silent", "--body-file", "-"], body).expect("the item is created");
    assert_eq!(made.code, 0, "{}{}", made.out, made.err);
    name_self("spira-runtime");
    reconcile(2, &ctx(), "content-drift", Reading::Standing(vec![standing(REF)]));
    assert_eq!(calls.get(), 1, "an open item holds the condition: the alarm is not raised again");
}
