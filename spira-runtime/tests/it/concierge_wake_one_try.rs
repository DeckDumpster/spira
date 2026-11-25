//! `concierge.sh wake` is one try: it delivers into an empty, still input line and otherwise refuses at once, never holding.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use spira_runtime::wake::concierge_wake;

struct Fixture {
    dir: testkit::TempDir,
}

impl Fixture {
    fn new(pane: &str, cursors: &[&str]) -> Fixture {
        let dir = testkit::TempDir::new("wake-one-try");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("run")).unwrap();
        let release = dir.join("release");
        std::fs::create_dir_all(release.join("bin")).unwrap();
        std::os::unix::fs::symlink(testkit::bin_exe("spira-config"), release.join("bin/spira-config")).unwrap();
        let toml = spira_config::process::fixture_toml(dir.path(), &[("SPIRA_RUN", &dir.join("run").to_string_lossy())]);
        std::fs::write(dir.join("cursors"), cursors.join("\n")).unwrap();
        std::fs::write(dir.join("pane"), pane).unwrap();
        testkit::write_exe(
            dir.join("bin/tmux"),
            &format!(
                "#!/bin/sh\necho \"$@\" >> {d}/log\ncase \"$*\" in\n*has-session*) exit 0;;\n*list-panes*) echo 0;;\n*capture-pane*) cat {d}/pane; n=$(cat {d}/n 2>/dev/null || echo 0); echo $((n+1)) > {d}/n; sed -n \"$((n+1))p\" {d}/cursors;;\nesac\n",
                d = dir.path().display()
            ),
        );
        testkit::write_exe(dir.join("bin/claude"), "#!/bin/sh\n");
        let real = repo_root().join("concierge.sh");
        testkit::write_exe(
            dir.join("concierge.sh"),
            &format!(
                "#!/bin/sh\nPATH={d}/bin:{release}/bin:/usr/bin:/bin SPIRA_RELEASE={release} SPIRA_TOML={toml} CONCIERGE_WAKE_SETTLE_SECS=0 exec bash {real} \"$@\"\n",
                d = dir.path().display(),
                toml = toml.display(),
                release = release.display(),
                real = real.display()
            ),
        );
        Fixture { dir }
    }

    fn script(&self) -> PathBuf {
        self.dir.join("concierge.sh")
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("log")).unwrap_or_default().lines().map(String::from).collect()
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

#[test_id(tgrd70v)]
#[test]
fn a_busy_input_line_is_refused_at_once_with_nothing_typed() {
    let f = Fixture::new("x\n❯ half a sentence\nfoot\n", &["3.4", "3.4", "3.4", "3.4"]);
    let t = Instant::now();
    let err = concierge_wake(&f.script(), "line", Duration::from_secs(20)).unwrap_err();
    assert!(err.contains("wake not delivered"), "{err}");
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    let calls = f.calls();
    assert!(!calls.iter().any(|c| c.contains("send-keys")), "{calls:?}");
    let after_checks = calls.iter().filter(|c| !c.contains("has-session") && !c.contains("list-panes")).count();
    assert!(after_checks <= 3, "{calls:?}");
}

#[test_id(ttez5m1)]
#[test]
fn an_empty_input_line_and_a_still_cursor_deliver_once() {
    let f = Fixture::new("x\n❯ \nfoot\n", &["3.4", "3.4", "3.4", "3.4"]);
    assert_eq!(concierge_wake(&f.script(), "line", Duration::from_secs(20)), Ok("woke".to_string()));
    let calls = f.calls();
    assert_eq!(calls.iter().filter(|c| c.contains("send-keys") && c.contains(" -l ")).count(), 1, "{calls:?}");
    assert_eq!(calls.iter().filter(|c| c.contains("send-keys") && c.ends_with("Enter")).count(), 1, "{calls:?}");
}

#[test_id(t0h1ef0)]
#[test]
fn a_cursor_that_moved_between_the_looks_is_refused() {
    let f = Fixture::new("x\n❯ \nfoot\n", &["3.4", "4.4", "4.4"]);
    let err = concierge_wake(&f.script(), "line", Duration::from_secs(20)).unwrap_err();
    assert!(err.contains("wake not delivered"), "{err}");
    assert!(!f.calls().iter().any(|c| c.contains("send-keys")));
}
