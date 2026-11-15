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
//!   - `spira_lane_runes` + `rune_get`: a `.rune` file is SOURCED as bash to read one
//!     field (dwarves.sh's own `lane_caps`), and a rune file may compute that field from
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

/// `dwarves.sh status`'s lane report: every lane rune's name and `RUNE_MAX_CONCURRENT`,
/// NUL-separated `name\tmax_concurrent` records — the same two `rune_get` reads
/// `lane_caps()` made, in one process instead of two lib.sh sourcings.
pub const LANE_CAPS: &str = r#"
for f in $(spira_lane_runes); do
    printf '%s\t%s\0' "$(rune_get "$f" RUNE_NAME "$f")" "$(rune_get "$f" RUNE_MAX_CONCURRENT 1)"
done
"#;

/// Parse [`LANE_CAPS`]'s output into `(name, max_concurrent)` pairs. A field that fails to
/// parse as a number is skipped, matching bash's own arithmetic-context coercion of a bad
/// value to 0 rather than crashing the whole report.
pub fn parse_lane_caps(out: &str) -> Vec<(String, u32)> {
    out.split('\0')
        .filter(|r| !r.is_empty())
        .filter_map(|rec| {
            let (name, n) = rec.split_once('\t')?;
            Some((name.to_string(), n.trim().parse().unwrap_or(0)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lane_caps_reads_nul_separated_records() {
        let out = "groomer\t1\0ops\t3\0";
        assert_eq!(parse_lane_caps(out), vec![("groomer".to_string(), 1), ("ops".to_string(), 3)]);
    }

    #[test]
    fn parse_lane_caps_defaults_bad_number_to_zero() {
        let out = "x\tnotanumber\0";
        assert_eq!(parse_lane_caps(out), vec![("x".to_string(), 0)]);
    }

    #[test]
    fn parse_lane_caps_of_empty_output_is_empty() {
        assert!(parse_lane_caps("").is_empty());
    }
}
