//! The lib.sh seam (DESIGN.md §6): `bash` with no arguments, reading a FIXED script from
//! stdin followed by the operation's values, each NUL-terminated. argv is only `bash`; the
//! environment is the caller's own. The script reads every value first, detaches stdin,
//! sources lib.sh, defines `progress` and `act` — which
//! lib.sh functions call and which only the landing pass itself used to define — and calls
//! exactly one function, whose name is part of the fixed text, never data
//! (law-payloads-go-on-stdin).
//!
//! Why this works: bash reading a non-seekable stdin reads it one byte at a time, so the
//! compound command `{ … }` is parsed to its closing brace and executed before any byte of
//! the data after it is consumed; the `read -d ''` loop inside consumes the data, and the
//! `exit` inside the braces stops bash before it could read further.

/// One seam operation. The first value of every call is `$SPIRA_HOME` (lib.sh's directory).
///
/// `ConflictNote`, `OtherBeads`, `PrMerged`, `LandSubject` and `CloseOnLand` are retired
/// (sp-81t4d, "wave 4.17": family R, landed verification) — `conflict_reopen_note`,
/// `other_beads_on_conflicts`, `pr_merged`, `land_subject` and the close-on-land are all
/// native now (`land_verify.rs`), reached through `RealLib` directly, never this seam.
/// `Closeout` and `GhUnlandedScan` are retired too (sp-j3fim, "wave 4.31": family AB,
/// GitHub closeout) — `gh_issue_closeout`/`_gh_unlanded_scan` moved into gh-intake;
/// `RealLib` shells to the compiled `gh-intake closeout`/`unlanded-scan` binary instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Context,
    Reopen,
    Event,
    Incident,
    Rebase,
    Recut,
    BumpRequeue,
    RequeuesOf,
    Note,
    Push,
    DeliverDelivered,
    DeliverRequeued,
    DeliverReturned,
    PruneWorktrees,
    /// `spira-lc deliver pr-merged <repo> <id> <br> <merge-sha>` (lc-delivery.sh until sp-arpjt).
    DeliverPrMerged,
    /// `spira-lc deliver pr-closed <id>`.
    DeliverPrClosed,
    /// `spira_git_push <repo> -q --force-with-lease -u <remote> <branch>` — `land_pr`'s push,
    /// distinct from `Push`'s plain `-q <remote> <refspec>`.
    ForcePush,
}

pub const ALL: &[Op] = &[
    Op::Context,
    Op::Reopen,
    Op::Event,
    Op::Incident,
    Op::Rebase,
    Op::Recut,
    Op::BumpRequeue,
    Op::RequeuesOf,
    Op::Note,
    Op::Push,
    Op::DeliverDelivered,
    Op::DeliverRequeued,
    Op::DeliverReturned,
    Op::PruneWorktrees,
    Op::DeliverPrMerged,
    Op::DeliverPrClosed,
    Op::ForcePush,
];

/// Precedes a seam's machine-readable answer on stdout.
pub const MARK: char = '\u{1e}';
/// Starts a line that is a movement (`progress`), not a log line.
pub const PROGRESS: char = '\u{1f}';
/// Separates the fields of one repository record in the context answer.
pub const FIELD: char = '\u{1d}';

const PRELUDE: &str = r#"{
set -uo pipefail
__q=()
while IFS= read -r -d '' __v; do __q+=("$__v"); done
exec </dev/null
HERE="${__q[0]}"
set -- "${__q[@]:1}"
unset __q __v
. "$HERE/lib.sh" || exit 96
progress() { printf '\037%s\n' "$*"; }
act() { log "$*"; }
"#;

/// Settings only — the repository list itself (family U: `spira_home_repo`/`spira_repos`/
/// `repo_root`/`repo_land`) and the base-ref columns (family W: `spira_landref`/
/// `ref_remote`/`ref_branch`/`qualify_base_ref`/`spira_publish_forge`) are both resolved
/// in-process now by `real.rs`'s `load_context`/`parse_context`, through
/// `spira_config::repos` (family W since sp-o88bx "wave 4.12"; family U since sp-k6lku
/// "wave 4.13") — not by a loop in this script shelling into each of those, every one of
/// them an extra spira-config subprocess, once per function per repository, inside this
/// one already-running bash seam call.
const CONTEXT: &str = r#"__kv() { printf '%s=%s\0' "$1" "$2"; }
printf '\036'
__kv home "${SPIRA_HOME:-$HERE}"
__kv run "${SPIRA_RUN:-}"
__kv repo "${SPIRA_REPO:-}"
__kv db "${SPIRA_DB:-}"
__kv bd "${SPIRA_BD:-bd}"
__kv bd_timeout "${BD_TIMEOUT:-180}"
__kv id_prefix "${SPIRA_ID_PREFIX:-sp}"
__kv land_maxsec "${SPIRA_LAND_MAXSEC:-3600}"
__kv gate_reserve "${SPIRA_LAND_GATE_RESERVE:-1200}"
__kv gate_lock_wait "${SPIRA_GATE_LOCK_WAIT:-}"
__kv certify_par "${SPIRA_CERTIFY_PAR:-}"
__kv gate_worker "${SPIRA_GATE_WORKER:-1}"
__kv verdict_ttl "${SPIRA_VERDICT_TTL:-0}"
__kv verdicts "${SPIRA_VERDICTS:-${SPIRA_RUN:-}/verdicts}"
__kv deferral_at "${SPIRA_DEFERRAL_ESCALATE_AT:-5}"
__kv express_label "${SPIRA_EXPRESS_LABEL:-express}"
__kv cutover_label "${SPIRA_CUTOVER_ROUND_LABEL:-cutover-round}"
__kv submitted_label "${SPIRA_SUBMITTED_LABEL:-spira-submitted}"
__kv rebase_escalate_at "${SPIRA_REBASE_ESCALATE_AT:-3}"
__kv git_name "${SPIRA_GIT_NAME:-spira}"
__kv git_email "${SPIRA_GIT_EMAIL:-spira@spira.invalid}"
__kv incident "${SPIRA_INCIDENT:-incident.sh}"
__kv scope_label "${SPIRA_SCOPE_LABEL:-}"
__kv queue_dir "${SPIRA_QUEUE_DIR:-${SPIRA_RUN:-}/queue}"
__kv halt_grace "${SPIRA_HALT_GRACE:-30}"
__kv path "${PATH:-}"
__kv bdjson_fixture "${SPIRA_BDJSON_FIXTURE:-}"
__kv toml "${SPIRA_TOML_FILE:-}"
__kv pr_refresh_max "${SPIRA_PR_REFRESH_MAX:-5}"
__kv ask_label "${SPIRA_ASK_LABEL:-}"
__kv noverdict_max "${SPIRA_NOVERDICT_MAX:-3}"
__kv noverdict_class_window "${SPIRA_NOVERDICT_CLASS_WINDOW:-86400}"
__kv rebase_decompose_files "${SPIRA_REBASE_DECOMPOSE_FILES:-4}"
__kv rebase_generated_files "${SPIRA_REBASE_GENERATED_FILES:-}"
__kv repo_map_ok "$([ -r "${SPIRA_REPO_MAP:-}" ] && printf 1 || printf 0)"
exit 0
"#;

const INCIDENT: &str = r#"case "$1" in
    */*) [ -r "$1" ] || exit 2; __p="$1" ;;
    *) __p="$(command -v "$1")" || exit 2 ;;
esac
__id="$(SPIRA_INCIDENT_TYPE=bug SPIRA_INCIDENT_PRIORITY=1 SPIRA_INCIDENT_ACTOR=landing \
    SPIRA_INCIDENT_LABELS="$2" SPIRA_INCIDENT_REPO="$3" SPIRA_INCIDENT_REF="$4" \
    SPIRA_INCIDENT_CAUSE=base-suite-red bash "$__p" file "$5" - <<< "$6")" || exit 1
printf '\036%s' "$__id"
exit 0
"#;

fn body(op: Op) -> &'static str {
    match op {
        Op::Context => CONTEXT,
        Op::Reopen => "bead_reopen \"$1\" \"$2\" \"$3\" || true\nexit 0\n",
        Op::Event => "spira_event \"$1\" \"$2\" \"$3\" \"$4\" || true\nexit 0\n",
        Op::Incident => INCIDENT,
        Op::Rebase => "rebase_branch \"$1\" \"$2\" \"$3\" \"$4\"; __rc=$?\nprintf '\\036%s\\035%s\\035%s' \"${REBASE_FAILURE:-}\" \"${REBASE_CONFLICTS:-}\" \"${REBASE_REFUSED_REASON:-}\"\nexit $__rc\n",
        Op::Recut => "recut_onto \"$1\" \"$2\" \"$3\" \"$4\"; __rc=$?\nprintf '\\036%s\\035%s' \"${RECUT_APPLIED_COUNT:-0}\" \"${RECUT_CONFLICTS:-}\"\nexit $__rc\n",
        Op::BumpRequeue => "bump_requeue \"$1\" \"$2\" >/dev/null 2>&1\nexit 0\n",
        Op::RequeuesOf => "printf '\\036%s' \"$(requeues_of \"$1\")\"\nexit 0\n",
        Op::Note => "bdq note \"$1\" \"$2\" >/dev/null 2>&1\nexit 0\n",
        Op::Push => "__e=\"$(spira_git_push \"$1\" -q \"$2\" \"$3\" 2>&1 >/dev/null)\"; __rc=$?\nprintf '\\036%s' \"$__e\"\nexit $__rc\n",
        // spira-lc's caller verbs (sp-arpjt; lc-delivery.sh before): their log lines are
        // lib.sh `log` lines, passed through like any other.
        Op::DeliverDelivered => "spira-lc deliver push-delivered \"$1\" \"$2\"; exit $?\n",
        Op::DeliverRequeued => "spira-lc deliver push-requeued \"$1\" \"$2\"; exit $?\n",
        Op::DeliverReturned => "spira-lc deliver push-returned \"$1\" \"$2\"; exit $?\n",
        Op::PruneWorktrees => "spira_prune_worktrees \"$1\" >/dev/null 2>&1\nexit 0\n",
        Op::DeliverPrMerged => "spira-lc deliver pr-merged \"$1\" \"$2\" \"$3\" \"$4\"; exit $?\n",
        Op::DeliverPrClosed => "spira-lc deliver pr-closed \"$1\" \"$2\"; exit $?\n",
        Op::ForcePush => "__e=\"$(spira_git_push \"$1\" -q --force-with-lease -u \"$2\" \"$3\" 2>&1 >/dev/null)\"; __rc=$?\nprintf '\\036%s' \"$__e\"\nexit $__rc\n",
    }
}

/// The complete fixed script for `op`.
pub fn script(op: Op) -> String {
    format!("{PRELUDE}{}}}\n", body(op))
}

/// The bytes written to the seam's stdin: the script, then every value NUL-terminated.
/// A value may not itself contain NUL; one that does is cut at it (it could not travel).
pub fn stdin_bytes(op: Op, values: &[&str]) -> Vec<u8> {
    let mut v = script(op).into_bytes();
    for val in values {
        let cut = val.split('\0').next().unwrap_or("");
        v.extend_from_slice(cut.as_bytes());
        v.push(0);
    }
    v
}

/// What one seam call printed: the lines before the answer (lib.sh log lines and
/// `progress` movements) and the answer after the last MARK.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Printed {
    pub logs: Vec<String>,
    pub progress: Vec<String>,
    pub answer: String,
}

pub fn split(stdout: &str) -> Printed {
    let (before, answer) = match stdout.rfind(MARK) {
        Some(i) => (&stdout[..i], &stdout[i + MARK.len_utf8()..]),
        None => (stdout, ""),
    };
    let mut p = Printed { answer: answer.to_string(), ..Default::default() };
    for line in before.split('\n') {
        if line.is_empty() {
            continue;
        }
        match line.strip_prefix(PROGRESS) {
            Some(m) => p.progress.push(m.to_string()),
            None => p.logs.push(line.to_string()),
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};

    #[test]
    fn every_script_is_one_braced_command_with_no_nul() {
        for op in ALL {
            let s = script(*op);
            assert!(s.starts_with("{\n") && s.ends_with("}\n"), "{op:?}");
            assert!(!s.contains('\0'), "{op:?}");
            assert!(s.contains("exit"), "{op:?} must exit inside the braces");
        }
    }

    #[test]
    fn a_delivery_script_exits_with_the_machines_own_status() {
        for op in [Op::DeliverDelivered, Op::DeliverRequeued, Op::DeliverReturned, Op::DeliverPrMerged, Op::DeliverPrClosed] {
            let s = script(op);
            assert!(!s.contains("|| true"), "{op:?} swallows a refusal");
            assert!(s.contains("exit $?"), "{op:?} must hand the status back");
        }
    }

    fn run(dir: &std::path::Path, op: Op, vals: &[&str]) -> (i32, String) {
        let home = dir.to_str().unwrap();
        let mut all = vec![home];
        all.extend_from_slice(vals);
        let mut child = Command::new("bash").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(&stdin_bytes(op, &all)).unwrap();
        let out = child.wait_with_output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
    }

    #[test]
    fn values_travel_on_stdin_with_newlines_and_metacharacters_intact() {
        let dir = crate::testutil::tmpdir("seam");
        std::fs::write(dir.join("lib.sh"), "bead_reopen() { printf '[%s]' \"$@\"; }\nlog() { echo \"L $*\"; }\n").unwrap();
        let (rc, out) = run(&dir, Op::Reopen, &["sp-a", "gate-red", "line one\nline $(two) `x` \"q\""]);
        assert_eq!(rc, 0);
        assert_eq!(out, "[sp-a][gate-red][line one\nline $(two) `x` \"q\"]");
    }

    #[test]
    fn progress_and_answers_are_separated_from_log_lines() {
        let dir = crate::testutil::tmpdir("seam2");
        std::fs::write(
            dir.join("lib.sh"),
            "log() { echo \"T spira: $*\"; }\n\
             spira_event() { log \"counted $1\"; progress \"escalated $3 — x\"; }\n\
             rebase_branch() { REBASE_FAILURE=conflict; REBASE_CONFLICTS='a b'; return 1; }\n",
        )
        .unwrap();
        let (_, out) = run(&dir, Op::Event, &["sp-a", "spira/sp-a", "spira", "detail"]);
        let p = split(&out);
        assert_eq!(p.logs, vec!["T spira: counted sp-a"]);
        assert_eq!(p.progress, vec!["escalated spira — x"]);
        let (rc, out) = run(&dir, Op::Rebase, &["spira/sp-a", "local/main", "/r", "spira"]);
        assert_eq!(rc, 1);
        assert_eq!(split(&out).answer, "conflict\u{1d}a b\u{1d}");
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let o = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }

    #[test]
    fn the_context_script_resolves_every_repository_through_lib_sh() {
        // spira_home_repo/spira_repos/repo_root/repo_land (family U, sp-k6lku "wave 4.13")
        // and spira_landref/ref_remote/ref_branch/qualify_base_ref/spira_publish_forge
        // (family W, sp-o88bx "wave 4.12") are both dropped from the CONTEXT script —
        // parse_context resolves all of them in-process through spira_config::repos
        // against a REAL SPIRA_REPO_MAP and REAL checkouts now, not stubbed bash functions, so
        // "h" and "o" are real git repositories rather than bare `.git` markers, and
        // SPIRA_REPO_MAP/SPIRA_HOME_REPO are real env (serialised: process-global state).
        let _serial = crate::testutil::serial();
        let dir = crate::testutil::tmpdir("ctx");
        let h = dir.join("h");
        let o = dir.join("o");
        std::fs::create_dir_all(&h).unwrap();
        std::fs::create_dir_all(&o).unwrap();
        // "h": no remote at all, so landref falls back (rung 4) to its own current branch —
        // which IS "local/main", a queue.local-shaped ref with no real "local" remote.
        git(&h, &["init", "-q", "-b", "local/main"]);
        std::fs::write(h.join("f"), "x").unwrap();
        git(&h, &["add", "f"]);
        git(&h, &["commit", "-q", "-m", "x"]);
        // "o": a real "origin" remote whose default branch is "master" (rung 2, via clone's
        // own cache of refs/remotes/origin/HEAD).
        let upstream = dir.join("upstream");
        std::fs::create_dir_all(&upstream).unwrap();
        git(&upstream, &["init", "-q", "-b", "master"]);
        std::fs::write(upstream.join("f"), "x").unwrap();
        git(&upstream, &["add", "f"]);
        git(&upstream, &["commit", "-q", "-m", "x"]);
        git(&dir, &["clone", "-q", upstream.to_str().unwrap(), o.to_str().unwrap()]);

        // "spira" (the home repo) declares land=queue.local; "other" and "ghost" leave
        // land blank (repo_land's own default, "push" — repos[1].mode is not asserted
        // either way); "ghost" declares no path at all, matching the old fake's `*) return
        // 1;;` branch for an unmapped name.
        let map = dir.join("repomap-fixture");
        std::fs::write(&map, format!("spira | {} | queue.local\nother | {}\nghost |\n", h.display(), o.display())).unwrap();
        // Declared config (the one source): the registry reads the map and home repo from
        // the SPIRA_TOML it resolves, never from the environment.
        std::os::unix::fs::symlink(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/conf.d"), dir.join("conf.d")).unwrap();
        let prev_toml = std::env::var("SPIRA_TOML").ok();
        let cfgdir = testkit::TempDir::new("lp-seam-registry-cfg");
        let toml = spira_config::process::fixture_toml(cfgdir.path(), &[("SPIRA_REPO_MAP", &map.display().to_string()), ("SPIRA_HOME_REPO", "spira")]);
        std::env::set_var("SPIRA_TOML", &toml);

        let lib = r#"SPIRA_RUN=/run/x; SPIRA_TOML_FILE=/cfg/doc
log() { echo "L $*"; }
"#;
        std::fs::write(dir.join("lib.sh"), lib).unwrap();
        let (rc, out) = run(&dir, Op::Context, &[]);

        let result = (|| -> Result<_, String> {
            if rc != 0 {
                return Err(format!("seam exited {rc}: {out}"));
            }
            crate::real::parse_context(&split(&out).answer, &dir)
        })();

        match prev_toml {
            Some(v) => std::env::set_var("SPIRA_TOML", v),
            None => std::env::remove_var("SPIRA_TOML"),
        }

        let (s, repos) = result.unwrap();
        assert_eq!(s.run, std::path::PathBuf::from("/run/x"));
        assert_eq!(s.toml, Some(std::path::PathBuf::from("/cfg/doc")));
        assert_eq!(s.home_repo, "spira");
        assert_eq!(repos.len(), 3);
        assert_eq!(repos[0].mode, crate::model::LandMode::QueueLocal);
        assert_eq!(repos[0].landref.as_deref(), Some("local/main"));
        assert_eq!(repos[0].base_remote, None);
        assert_eq!(repos[0].forge_ref.as_deref(), Some("refs/remotes/origin/main"));
        assert_eq!(repos[1].base_fq.as_deref(), Some("refs/remotes/origin/master"));
        assert_eq!(repos[1].base_remote.as_deref(), Some("origin"));
        assert_eq!(repos[1].base_branch, "master");
        assert_eq!(repos[1].forge_ref.as_deref(), Some("refs/remotes/origin/master"));
        assert!(repos[2].path.as_os_str().is_empty() && repos[2].landref.is_none());
    }

    #[test]
    fn a_missing_lib_is_status_96_not_an_answer() {
        let dir = crate::testutil::tmpdir("seam3");
        let (rc, out) = run(&dir, Op::RequeuesOf, &["sp-a"]);
        assert_eq!(rc, 96);
        assert_eq!(split(&out).answer, "");
    }
}
