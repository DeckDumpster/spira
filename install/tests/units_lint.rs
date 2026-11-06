// covers: systemd/*.service systemd/*.timer install/src/values.rs round-controller/Cargo.toml round-controller/src/main.rs
use std::path::{Path, PathBuf};
fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../systemd")
}

fn raw(n: &str) -> String {
    std::fs::read_to_string(dir().join(n)).unwrap()
}

fn lone_pct(text: &str) -> usize {
    text.lines().filter(|l| l.starts_with("Exec")).filter(|l| l.replace("%%", "").contains('%') && l.replace("%%", "").split('%').skip(1).any(|r| r.starts_with(|c: char| c.is_ascii_alphabetic()))).count()
}

#[test]
fn exec_lines_carry_no_unescaped_percent_specifier() {
    assert!(lone_pct("ExecCondition=/bin/bash -c 'printf \"%s\"'") > 0, "positive control");
    // Every Exec line's `%` is an intended specifier — the runtime dir (`%t`) or a template's
    // instance (`%i`) — or escaped. A `printf "%s"` in a shell snippet is how one hid: systemd
    // expands it first. The spira-ops unit that carried that snippet is now a scheduler task.
    let intended = |l: &str| l.replace("%t", "").replace("%i", "");
    assert!(lone_pct(&intended("ExecStart=/x serve %t/sock %i")) == 0, "positive control: the intended specifiers pass");
    let mut units = 0;
    for e in std::fs::read_dir(dir()).unwrap().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.ends_with(".service") || n.ends_with(".timer") {
            units += 1;
            let text: String = raw(&n).lines().map(|l| format!("{}\n", intended(l))).collect();
            assert_eq!(lone_pct(&text), 0, "{n}");
        }
    }
    assert!(units > 0, "the templates were read");
}
