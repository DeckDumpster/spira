//! Every test binary runs under cargo's target runner, `tools/test-sandbox/runner`, which
//! `.cargo/config.toml` names: a bubblewrap sandbox in which the caller's installation is out of
//! reach (sp-bpenvh). What a binary can reach inside it is spira/test-test-sandbox.sh's to prove
//! with a probe binary; these tests read files and spawn nothing.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

#[test]
fn cargo_runs_every_test_binary_through_the_sandbox_runner() {
    let ws = workspace();
    let text = fs::read_to_string(ws.join(".cargo/config.toml")).expect("the workspace's .cargo/config.toml");
    let conf: toml::Table = text.parse().expect(".cargo/config.toml parses");
    let target = conf.get("target").and_then(toml::Value::as_table).expect("a [target] table");
    for triple in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let runner = target.get(triple).and_then(|t| t.get("runner")).and_then(toml::Value::as_str);
        assert_eq!(runner, Some("tools/test-sandbox/runner"), "[target.{triple}] names the sandbox runner");
    }
    assert!(target.keys().all(|k| !k.starts_with("cfg(")), "a cfg(...) key loses to a triple key in ~/.cargo/config.toml: {target:?}");
    let mode = fs::metadata(ws.join("tools/test-sandbox/runner")).expect("the runner exists").permissions().mode();
    assert_ne!(mode & 0o111, 0, "the runner is executable");
}

#[test]
fn a_test_finds_no_installation_under_its_home() {
    let home = PathBuf::from(std::env::var_os("HOME").expect("the runner sets HOME"));
    let held: Vec<_> = match fs::read_dir(home.join("spira")) {
        Ok(d) => d.map(|e| e.unwrap().file_name()).collect(),
        Err(e) => {
            assert_eq!(e.kind(), std::io::ErrorKind::NotFound, "{e}");
            Vec::new()
        }
    };
    assert!(held.is_empty(), "$HOME/spira holds {held:?}; the sandbox shows it empty or not at all");
    for p in [".config/spira", ".local/bin/work"] {
        assert!(!home.join(p).exists(), "$HOME/{p} is visible inside the sandbox");
    }
}
