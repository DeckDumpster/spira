use std::process::Command;

fn provenance(mode: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_gh-intake")).args(["provenance", mode]).output().unwrap()
}

#[test]
fn the_retired_provenance_modes_exit_2_with_the_unknown_argument_usage() {
    for mode in ["watch", "health"] {
        let out = provenance(mode);
        assert_eq!(out.status.code(), Some(2), "{mode}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(&format!("unknown argument: {mode}")), "{mode}: {err}");
    }
}
