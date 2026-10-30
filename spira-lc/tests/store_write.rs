//! `bd-supersede` and `bd-note` reach bd, at the configured `SPIRA_BD`, with the argv the
//! retired supersede-duty.sh used, and never the lifecycle service.

use std::path::Path;
use std::process::Command;

fn run(args: &[&str], dir: &Path, bd: &Path, db: &str) -> (i32, String) {
    let toml = spira_config::process::fixture_toml(
        dir,
        &[("SPIRA_RUN", dir.to_str().unwrap()), ("SPIRA_BD", bd.to_str().unwrap()), ("SPIRA_DB", db), ("SPIRA_LC_SOCKET", "/nonexistent/lc.sock")],
    );
    let o = Command::new(env!("CARGO_BIN_EXE_spira-lc"))
        .args(args)
        .env("SPIRA_HOME", concat!(env!("CARGO_MANIFEST_DIR"), "/../spira"))
        .env("SPIRA_TOML", &toml)
        .output()
        .unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stderr).into_owned())
}

#[test]
fn supersede_and_note_call_the_configured_bd_with_the_shells_argv() {
    let dir = testkit::TempDir::new("spira-lc-store-write");
    let log = dir.join("calls.log");
    let bd = dir.join("fake-bd");
    testkit::write_exe(&bd, &format!("#!/bin/sh\nfor a in \"$@\"; do printf '[%s]' \"$a\"; done >> {}\necho >> {}\n", log.display(), log.display()));

    let (rc, err) = run(&["bd-supersede", "sp-old", "sp-new"], &dir, &bd, "/db");
    assert_eq!(rc, 0, "{err}");
    let (rc, err) = run(&["bd-note", "sp-old", "supersede-request confirmed: sp-new is LANDED"], &dir, &bd, "/db");
    assert_eq!(rc, 0, "{err}");
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "[-C][/db][supersede][sp-old][--with][sp-new]\n[-C][/db][note][sp-old][supersede-request confirmed: sp-new is LANDED]\n"
    );

    let (rc, _) = run(&["bd-supersede", "sp-old", "sp-old"], &dir, &bd, "/db");
    assert_eq!(rc, 3, "a self-supersede is refused");
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 2, "the refusal never reached bd");
}
