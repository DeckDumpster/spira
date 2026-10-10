//! `spira-config set`/`unset` through the real binary — the writer the harness's own shell
//! callers invoke. Covers the three properties this crate's writers must have: a value with
//! shell-hostile characters round-trips exactly, an invalid value is refused leaving the file
//! byte-for-byte unchanged, and the write is atomic with a timestamped backup of whatever the
//! file held before.

use std::fs;
use std::path::Path;
use std::process::Command;

fn scratch_dir(tag: &str) -> testkit::TempDir {
    testkit::TempDir::new(&format!("spira-config-test-set-{tag}"))
}

fn spira_config(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_spira-config"))
        .args(args)
        .output()
        .expect("spira-config runs")
}

fn write_tmp(dir: &Path, name: &str, contents: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    fs::write(&p, contents).unwrap();
    p
}

fn backups_in(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".bak."))
        .collect()
}

// T1: a gate string containing quotes, `$()` and newlines round-trips exactly.
#[test]
fn set_gate_string_with_quotes_dollar_and_newlines_round_trips() {
    let dir = scratch_dir("gate-roundtrip");
    let toml = write_tmp(
        &dir,
        "spira.toml",
        "[repo.home]\npath = \"/srv/home\"\nmode = \"push\"\n",
    );
    let gate = "echo \"hi\" && $(rm -rf /)\nsecond line\n\t'single quoted'";

    let result = spira_config(&["set", "repo.home.base", gate, toml.to_str().unwrap()]);
    assert!(
        result.status.success(),
        "set failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let got = spira_config(&["get", "repo.home.base", toml.to_str().unwrap()]);
    assert!(got.status.success());
    let got_val = String::from_utf8_lossy(&got.stdout);
    assert_eq!(got_val.trim_end_matches('\n'), gate);

}

// T2: an invalid value is refused and the file is unchanged.
#[test]
fn set_refuses_an_invalid_value_and_leaves_the_file_unchanged() {
    let dir = scratch_dir("invalid-refused");
    let before = "[spira]\nmax_aeons = 4\n";
    let toml = write_tmp(&dir, "spira.toml", before);

    // max_aeons is a number; a non-numeric string can never coerce to one.
    let result = spira_config(&["set", "spira.max_aeons", "not-a-number", toml.to_str().unwrap()]);
    assert!(!result.status.success(), "expected set to refuse an invalid value");

    let after = fs::read_to_string(&toml).unwrap();
    assert_eq!(after, before, "the file must be byte-for-byte unchanged on refusal");
    assert!(backups_in(&dir).is_empty(), "a refused write must not leave a backup either");

}

// T2 positive control: the same field, a value that DOES coerce, must succeed — otherwise
// the refusal above could be failing for an unrelated reason.
#[test]
fn set_accepts_a_valid_value_for_the_same_field() {
    let dir = scratch_dir("invalid-refused-control");
    let toml = write_tmp(&dir, "spira.toml", "[spira]\nmax_aeons = 4\n");

    let result = spira_config(&["set", "spira.max_aeons", "5", toml.to_str().unwrap()]);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let got = spira_config(&["get", "spira.max_aeons", toml.to_str().unwrap()]);
    assert_eq!(String::from_utf8_lossy(&got.stdout).trim(), "5");

}

// T3 (via a subprocess, not just the library's own two-phase unit test): unset an unknown
// path — refused the same way as an invalid set — must leave the file untouched too.
#[test]
fn unset_of_an_unknown_field_is_refused_and_leaves_the_file_unchanged() {
    let dir = scratch_dir("unset-invalid");
    let before = "[spira]\nmax_aeons = 4\n";
    let toml = write_tmp(&dir, "spira.toml", before);

    let result = spira_config(&["unset", "spira.not_a_real_field", toml.to_str().unwrap()]);
    assert!(!result.status.success());
    assert_eq!(fs::read_to_string(&toml).unwrap(), before);

}

#[test]
fn set_over_an_existing_file_leaves_a_timestamped_backup_of_the_old_contents() {
    let dir = scratch_dir("backup");
    let before = "[spira]\nmax_aeons = 4\n";
    let toml = write_tmp(&dir, "spira.toml", before);

    let result = spira_config(&["set", "spira.max_aeons", "9", toml.to_str().unwrap()]);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));

    let backups = backups_in(&dir);
    assert_eq!(backups.len(), 1, "expected exactly one backup, found {backups:?}");
    let backup_contents = fs::read_to_string(dir.join(&backups[0])).unwrap();
    assert_eq!(backup_contents, before, "the backup must hold the PRE-write contents");

}

// First write to a file that does not exist yet has nothing to back up.
#[test]
fn set_on_a_fresh_file_leaves_no_backup() {
    let dir = scratch_dir("no-backup-fresh");
    let toml = dir.join("spira.toml");

    let result = spira_config(&["set", "spira.max_aeons", "3", toml.to_str().unwrap()]);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert!(backups_in(&dir).is_empty());

}

// The `repo.<name>.land` alias `spira-config set`'s callers use for the schema's `mode`
// field, end to end through the binary (the library-level alias has its own unit test).
#[test]
fn set_repo_land_alias_round_trips_through_the_binary() {
    let dir = scratch_dir("land-alias");
    let toml = write_tmp(
        &dir,
        "spira.toml",
        "[repo.home]\npath = \"/srv/home\"\nmode = \"push\"\n",
    );

    let result = spira_config(&["set", "repo.home.land", "queue.local", toml.to_str().unwrap()]);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));

    let via_land = spira_config(&["get", "repo.home.land", toml.to_str().unwrap()]);
    let via_mode = spira_config(&["get", "repo.home.mode", toml.to_str().unwrap()]);
    assert_eq!(String::from_utf8_lossy(&via_land.stdout).trim(), "queue.local");
    assert_eq!(String::from_utf8_lossy(&via_mode.stdout).trim(), "queue.local");

}

#[test]
fn set_refuses_a_checked_in_fixture_and_leaves_no_backup() {
    let dir = scratch_dir("fixture-refused");
    let fixtures = dir.join("tests/fixtures");
    fs::create_dir_all(&fixtures).unwrap();
    let toml = write_tmp(&fixtures, "complete.toml", "[repo.home]\npath = \"/srv/home\"\nmode = \"push\"\n");
    let before = fs::read(&toml).unwrap();

    let result = spira_config(&["set", "repo.home.base", "main", toml.to_str().unwrap()]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("checked-in test fixture"));
    assert_eq!(fs::read(&toml).unwrap(), before);
    assert!(backups_in(&fixtures).is_empty());
}

// Setting one key changes only that key's line: an earlier key set to "", comments, key order
// and quoting style of every other line all survive byte-identical.
#[test]
fn set_leaves_every_other_line_byte_identical() {
    let dir = scratch_dir("only-named-key");
    let original = "# operator's note\n[spira]\nzeta_label = 'x'   # trailing\nid_prefix = \"sp\"\n\n[repo.r]\npath = \"/x\"\nmode = \"queue.local\"\n";
    let toml = write_tmp(&dir, "spira.toml", original);
    let f = toml.to_str().unwrap();

    let a = spira_config(&["set", "spira.scope_label", "", f]);
    assert!(a.status.success(), "{}", String::from_utf8_lossy(&a.stderr));
    let after_a = fs::read_to_string(&toml).unwrap();
    assert!(after_a.contains("scope_label = \"\""), "{after_a}");
    for line in original.lines() {
        assert!(after_a.lines().any(|l| l == line), "line {line:?} changed:\n{after_a}");
    }

    let b = spira_config(&["set", "spira.batcher_enable", "1", f]);
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    let after_b = fs::read_to_string(&toml).unwrap();
    assert!(after_b.contains("scope_label = \"\""), "{after_b}");
    for line in after_a.lines() {
        assert!(after_b.lines().any(|l| l == line), "line {line:?} changed:\n{after_b}");
    }
    assert!(after_b.contains("batcher_enable = \"1\""), "{after_b}");

    let c = spira_config(&["set", "spira.batcher_enable", "0", f]);
    assert!(c.status.success());
    let after_c = fs::read_to_string(&toml).unwrap();
    assert_eq!(after_c.replace("batcher_enable = \"0\"", "batcher_enable = \"1\""), after_b);
}
