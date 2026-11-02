//! `batcher sifts` and `round-controller sifts` on one recorded pool reach the same verdicts,
//! line for line: the same store writes and notes in the same order, the same Concierge inbox,
//! the same screen records and the same verdict lines on stdout.

#[path = "../../round-controller/tests/sifts_world/mod.rs"]
mod sifts_world;

use std::process::Command;

use sifts_world::World;

/// A stdout line with the program's own name in front of it replaced by the controller's.
fn as_controller(line: &str) -> String {
    match line.strip_prefix("batcher") {
        Some(rest) => format!("round-controller{rest}"),
        None => line.to_string(),
    }
}

#[test]
fn batcher_sifts_and_round_controller_sifts_reach_identical_verdicts_on_one_pool() {
    let dir = testkit::TempDir::new("sifts-parity");
    let w = World::build(&dir);
    let set = w.process_env();
    let unset: Vec<String> = std::env::vars().map(|(k, _)| k).filter(|k| k.starts_with("SPIRA_") && !set.iter().any(|(s, _)| s == k)).collect();
    let mut edits: Vec<(&str, Option<&str>)> = set.iter().map(|(k, v)| (k.as_str(), Some(v.as_str()))).collect();
    edits.extend(unset.iter().map(|k| (k.as_str(), None)));
    let _env = testkit::env(&edits);

    let b = Command::new(env!("CARGO_BIN_EXE_batcher")).arg("sifts").output().unwrap();
    let b_err = String::from_utf8_lossy(&b.stderr).into_owned();
    assert!(!b.status.success(), "the broken repo fails the batcher's pass: {b_err}");
    assert!(b_err.contains("batcher: sift failed for: broken"), "{b_err}");
    let b_out: Vec<String> = String::from_utf8_lossy(&b.stdout).lines().map(as_controller).collect();
    let b_verdicts = w.verdicts();

    w.reset();
    let mut out = Vec::new();
    let r = round_controller::sifts::for_process(&mut out);
    assert_eq!(r, Err("sift failed for: broken".to_string()));
    let r_out: Vec<String> = String::from_utf8(out).unwrap().lines().map(str::to_string).collect();
    let r_verdicts = w.verdicts();

    assert!(b_verdicts.iter().any(|l| l.contains("--kind {\"Sifted\"")), "the pool reached a pass: {b_verdicts:#?}\n{b_err}");
    assert!(b_verdicts.iter().any(|l| l.contains("--kind {\"GateRed\"")), "the pool reached a send-back: {b_verdicts:#?}");
    assert!(b_verdicts.iter().any(|l| l.contains("--superseded-by")), "the pool reached a supersede: {b_verdicts:#?}");
    assert!(b_verdicts.iter().any(|l| l.contains("SIFT REPEAT")), "the pool reached the cap: {b_verdicts:#?}");
    assert_eq!(r_verdicts, b_verdicts);
    assert_eq!(r_out, b_out);
}
