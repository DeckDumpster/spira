//! The lib.sh seams this crate still needs, in the one shape every Rust crate in this
//! workspace already uses for it (sentinel/src/seams.rs, DESIGN.md §6): a FIXED script run
//! as `bash -c '<script>' <name>`, parameters arriving only through the environment
//! (never interpolated into the script text — law-payloads-go-on-stdin), stdout the only
//! channel back.
//!
//! lib.sh itself is explicitly out of this wave's scope (`remaining-bash-inventory.md`
//! group 4: "Ryan's standing instruction during the cutover was 'leave lib.sh alone.'");
//! its chokepoint below is called, never re-implemented:
//!
//!   - `spira_reactive_runes` + `rune_get`: a `.rune` file is SOURCED as bash to read one
//!     field (dwarves.sh's own `reactive_caps`), and a rune file may compute that field from
//!     other shell variables (groomer.rune's `RUNE_LABELS` does exactly this for a
//!     different field) — a hand-written Rust parser would silently disagree on the one
//!     rune that does, which is the exact failure `dwarves.sh` exists to prevent
//!     (law-bake-rules-into-tools: a program that refuses to be misread).

use std::path::Path;
use std::process::Stdio;

/// Run `<script>` as `bash -c '<script>' <name>`, with `SPIRA_HOME_LIB` pointing at
/// `lib.sh` (the seam's own prelude sources it) and every `(k, v)` in `envs` set besides
/// it. `stdin` is written and closed before the output is read. Returns
/// `(stdout, success)`; stderr is inherited so lib.sh's own chatter is visible, exactly as
/// sentinel's seams do.
pub fn run(lib_sh: &Path, name: &str, script: &str, envs: &[(&str, &str)], stdin: &str) -> (String, bool) {
    let (out, rc) = run_for(lib_sh, name, script, envs, stdin, spira_config::bounded::CALL_TIMEOUT_SECS);
    (out, rc == Some(0))
}

/// [`run`] with the caller's own deadline (seconds, as `timeout` takes them) and the exit
/// code: `Some(124)` is the deadline, `None` is a seam that never started or has no code.
pub fn run_for(lib_sh: &Path, name: &str, script: &str, envs: &[(&str, &str)], stdin: &str, secs: &str) -> (String, Option<i32>) {
    let full = format!("set -uo pipefail\n. \"$SPIRA_HOME_LIB\" >&2 || exit 97\n{script}\n");
    // FORK-EDGE: not a config bootstrap: a rune file is sourced as bash, and may compute a field from shell variables.
    let mut cmd = spira_config::bounded::bounded_for("bash", secs);
    cmd.args(["-c", &full, name]);
    cmd.env("SPIRA_HOME_LIB", lib_sh);
    match spira_config::release_env::child_path_env_for_process() {
        Ok(e) => cmd.envs(e),
        Err(e) => return (format!("{name}: {e}"), None),
    };
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return (String::new(), None),
    };
    {
        use std::io::Write;
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(stdin.as_bytes());
        }
    }
    match child.wait_with_output() {
        Ok(o) => (String::from_utf8_lossy(&o.stdout).into_owned(), o.status.code()),
        Err(_) => (String::new(), None),
    }
}

/// `dwarves.sh status`'s reactive report: every reactive rune's name and whether it is
/// `RUNE_SINGLE_FLIGHT`, NUL-separated `name\tsingle-flight|` records.
pub const REACTIVE_CAPS: &str = r#"
for f in $(spira_reactive_runes); do
    sf=""; [ "$(rune_get "$f" RUNE_SINGLE_FLIGHT 0)" = 1 ] && sf="single-flight"
    printf '%s\t%s\0' "$(rune_get "$f" RUNE_NAME "$f")" "$sf"
done
"#;

/// Parse [`REACTIVE_CAPS`]'s output into `(name, single_flight)` pairs.
pub fn parse_reactive_caps(out: &str) -> Vec<(String, bool)> {
    out.split('\0')
        .filter(|r| !r.is_empty())
        .filter_map(|rec| {
            let (name, sf) = rec.split_once('\t')?;
            Some((name.to_string(), sf.trim() == "single-flight"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(t1ierue)]
    #[test]
    fn parse_reactive_caps_reads_nul_separated_records() {
        let out = "groomer\tsingle-flight\0ops\t\0";
        assert_eq!(parse_reactive_caps(out), vec![("groomer".to_string(), true), ("ops".to_string(), false)]);
    }

    #[test_id(twh4z66)]
    #[test]
    fn parse_reactive_caps_reads_anything_but_single_flight_as_not() {
        let out = "x\tnotanumber\0";
        assert_eq!(parse_reactive_caps(out), vec![("x".to_string(), false)]);
    }

    #[test_id(tbppgkm)]
    #[test]
    fn parse_reactive_caps_of_empty_output_is_empty() {
        assert!(parse_reactive_caps("").is_empty());
    }
}
