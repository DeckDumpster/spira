//! The container setup in ONE `podman exec` (sp-t26yx, DESIGN.md §11.4).
//!
//! Every `podman exec` takes podman's global locks — the libpod sqlite database (a rollback
//! journal, fdatasync on every commit) and the storage `layers.lock` — so under a loaded host
//! its fixed cost is set by other podman users and by disk flush latency, not by the command
//! it runs: 24–73 s for `podman exec <c> true` was measured while four gate trials ran
//! (2026-09-30). The setup used to pay that toll once per step — stage, configure, three
//! suspends, install, one requirement check per token, the testdb template. A plan pays it
//! once: the host sends the steps, [`run`] executes them in order inside the container, and
//! [`parse`] reads back each step's rc, wall and output.
//!
//! The runner is the host's own `testenv` executable, linked into the worktree the container
//! mounts ([`stage_runner`]), so the host and the runner always speak the same protocol —
//! never the candidate's binary, which may predate it.

use std::process::Command;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// The runner inside the worktree (bind-mounted at `/workspace`), under cargo's `target/`.
pub const RUNNER_REL: &str = "target/.testenv-runner/testenv";
/// `testenv plan <json>`.
pub const PLAN_ARG: &str = "plan";

/// One setup command, exactly what one `podman exec` used to carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// Unique within the plan (`stage`, `configure`, `suspend:<unit>`, `install`,
    /// `requires:<token>`, `template`).
    pub name: String,
    /// The setup phase it belongs to: `install`, `requirements` or `testdb` (DESIGN.md D11).
    pub phase: String,
    pub argv: Vec<String>,
    /// Added to the runner's environment (what `podman exec -e` added to the container's).
    pub env: Vec<(String, String)>,
    /// A failing `must` step ends the plan: nothing after it runs.
    pub must: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// Prefixes every frame line, so no step's output can forge one.
    pub nonce: String,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepResult {
    pub name: String,
    pub phase: String,
    pub rc: i32,
    pub ms: u64,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Parsed {
    pub results: Vec<StepResult>,
    /// The step that began and never ended (the exec was killed while it ran).
    pub unfinished: Option<String>,
}

pub fn nonce() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("@@testenv-plan-{}-{t:x}", std::process::id())
}

// ---- the frame format ----------------------------------------------------------------
//
//   <nonce> begin <name>\n
//   <nonce> end <name> rc=<rc> ms=<ms> len=<bytes>\n<output: exactly len bytes>\n
//
// `output` is the step's stdout and stderr on one descriptor (as `podman exec` gave them),
// made valid UTF-8 before it is measured, so the host's lossy read cannot shift a length.

pub fn frame_begin(nonce: &str, name: &str) -> String {
    format!("{nonce} begin {name}\n")
}

pub fn frame_end(nonce: &str, name: &str, rc: i32, ms: u64, output: &str) -> String {
    format!(
        "{nonce} end {name} rc={rc} ms={ms} len={}\n{output}\n",
        output.len()
    )
}

/// Read a runner's output back into per-step results. Text outside frames (podman's own
/// complaints, a runner usage error) is ignored here; the caller keeps the raw output.
pub fn parse(plan: &Plan, out: &str) -> Parsed {
    let begin = format!("{} begin ", plan.nonce);
    let end = format!("{} end ", plan.nonce);
    let mut p = Parsed::default();
    let mut rest = out;
    loop {
        let (bi, ei) = (rest.find(&begin), rest.find(&end));
        let take_begin = match (bi, ei) {
            (None, None) => break,
            (Some(b), Some(e)) => b < e,
            (Some(_), None) => true,
            (None, Some(_)) => false,
        };
        if take_begin {
            let b = bi.unwrap_or(0) + begin.len();
            let line_end = rest[b..].find('\n').map(|i| b + i).unwrap_or(rest.len());
            p.unfinished = Some(rest[b..line_end].to_string());
            rest = &rest[(line_end + 1).min(rest.len())..];
            continue;
        }
        let e = ei.unwrap_or(0) + end.len();
        let Some(nl) = rest[e..].find('\n').map(|i| e + i) else {
            break;
        };
        let header: Vec<&str> = rest[e..nl].split(' ').collect();
        let field = |k: &str| {
            header
                .iter()
                .find_map(|f| f.strip_prefix(k))
                .and_then(|v| v.parse::<i64>().ok())
        };
        let (Some(name), Some(rc), Some(ms), Some(len)) = (
            header.first().map(|s| s.to_string()),
            field("rc="),
            field("ms="),
            field("len="),
        ) else {
            break;
        };
        let start = nl + 1;
        let stop = start + len.max(0) as usize;
        if stop > rest.len() || !rest.is_char_boundary(stop) {
            break; // cut mid-output: the step ended, its output did not arrive whole
        }
        let phase = plan
            .steps
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.phase.clone())
            .unwrap_or_default();
        p.results.push(StepResult {
            name: name.clone(),
            phase,
            rc: rc as i32,
            ms: ms.max(0) as u64,
            output: rest[start..stop].to_string(),
        });
        if p.unfinished.as_deref() == Some(name.as_str()) {
            p.unfinished = None;
        }
        rest = &rest[(stop + 1).min(rest.len())..];
    }
    p
}

// ---- the runner (inside the container) -------------------------------------------------

fn run_step(step: &Step) -> (i32, String) {
    let Some((prog, args)) = step.argv.split_first() else {
        return (127, "plan: empty argv".into());
    };
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = std::env::temp_dir().join(format!(
        "testenv-plan-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    let file = match fs::File::create(&tmp) {
        Ok(f) => f,
        Err(e) => return (127, format!("plan: {}: {e}", tmp.display())),
    };
    let err = match file.try_clone() {
        Ok(f) => f,
        Err(e) => return (127, format!("plan: {e}")),
    };
    // batch-job: this runs whatever its caller names, as long as that takes
    let st = Command::new(prog)
        .args(args)
        .envs(step.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(file)
        .stderr(err)
        .status();
    let mut buf = Vec::new();
    if let Ok(mut f) = fs::File::open(&tmp) {
        let _ = f.read_to_end(&mut buf);
    }
    let _ = fs::remove_file(&tmp);
    let mut output = String::from_utf8_lossy(&buf).into_owned();
    let rc = match st {
        Ok(s) => {
            use std::os::unix::process::ExitStatusExt;
            s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0))
        }
        Err(e) => {
            output.push_str(&format!("plan: spawn {prog}: {e}"));
            127
        }
    };
    (rc, output)
}

/// Execute `plan`, framing each step on `out`. Returns the process rc: 0, or the rc of the
/// `must` step that ended the plan.
pub fn run(plan: &Plan, out: &mut dyn Write) -> i32 {
    for step in &plan.steps {
        let _ = out.write_all(frame_begin(&plan.nonce, &step.name).as_bytes());
        let _ = out.flush();
        let t0 = Instant::now();
        let (rc, output) = run_step(step);
        let ms = t0.elapsed().as_millis() as u64;
        let _ = out.write_all(frame_end(&plan.nonce, &step.name, rc, ms, &output).as_bytes());
        let _ = out.flush();
        if step.must && rc != 0 {
            return rc.clamp(1, 255);
        }
    }
    0
}

/// `testenv plan <json>`.
pub fn main(args: &[String]) -> i32 {
    let [json] = args else {
        eprintln!("usage: testenv plan <plan-json>");
        return 2;
    };
    let plan: Plan = match serde_json::from_str(json) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("testenv plan: unreadable plan: {e}");
            return 2;
        }
    };
    run(&plan, &mut io::stdout().lock())
}

// ---- the host side ---------------------------------------------------------------------

/// Link (else copy) the running executable to `<worktree>/RUNNER_REL`, where the container
/// sees it at `/workspace/RUNNER_REL`. A hard link costs no IO; the release and the
/// worktrees normally share a filesystem.
pub fn stage_runner(worktree: &Path, exe: &Path) -> io::Result<PathBuf> {
    let dst = worktree.join(RUNNER_REL);
    if let Some(d) = dst.parent() {
        fs::create_dir_all(d)?;
    }
    let tmp = dst.with_extension(format!("tmp{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    if fs::hard_link(exe, &tmp).is_err() {
        fs::copy(exe, &tmp)?;
    }
    fs::rename(&tmp, &dst)?;
    Ok(dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(name: &str, phase: &str, argv: &[&str], must: bool) -> Step {
        Step {
            name: name.into(),
            phase: phase.into(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            env: vec![("PLAN_T".into(), "v".into())],
            must,
        }
    }

    fn plan(steps: Vec<Step>) -> Plan {
        Plan {
            nonce: "@@N".into(),
            steps,
        }
    }

    #[test]
    fn runs_steps_in_order_with_their_env_and_frames_each() {
        let p = plan(vec![
            step(
                "a",
                "install",
                &["sh", "-c", "echo out-$PLAN_T; echo err >&2"],
                true,
            ),
            step("b", "requirements", &["sh", "-c", "exit 3"], false),
            step("c", "testdb", &["sh", "-c", "printf 'x\\ny'"], false),
        ]);
        let mut buf = Vec::new();
        assert_eq!(
            run(&p, &mut buf),
            0,
            "a non-must failure does not end the plan"
        );
        let out = String::from_utf8(buf).unwrap();
        let r = parse(&p, &out);
        assert_eq!(r.unfinished, None);
        let got: Vec<(&str, &str, i32, &str)> = r
            .results
            .iter()
            .map(|s| (s.name.as_str(), s.phase.as_str(), s.rc, s.output.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("a", "install", 0, "out-v\nerr\n"),
                ("b", "requirements", 3, ""),
                ("c", "testdb", 0, "x\ny"),
            ]
        );
    }

    #[test]
    fn a_failing_must_step_ends_the_plan_with_its_rc() {
        let p = plan(vec![
            step("a", "install", &["sh", "-c", "echo boom; exit 4"], true),
            step("b", "install", &["sh", "-c", "echo never"], true),
        ]);
        let mut buf = Vec::new();
        assert_eq!(run(&p, &mut buf), 4);
        let r = parse(&p, &String::from_utf8(buf).unwrap());
        assert_eq!(r.results.len(), 1);
        assert_eq!(
            (r.results[0].rc, r.results[0].output.as_str()),
            (4, "boom\n")
        );
    }

    #[test]
    fn a_missing_program_is_127_not_a_runner_crash() {
        let p = plan(vec![step("a", "install", &["/nonexistent/prog"], true)]);
        let mut buf = Vec::new();
        assert_eq!(run(&p, &mut buf), 127);
        let r = parse(&p, &String::from_utf8(buf).unwrap());
        assert_eq!(r.results[0].rc, 127);
        assert!(r.results[0].output.contains("spawn /nonexistent/prog"));
    }

    #[test]
    fn a_step_cut_mid_run_is_the_unfinished_one() {
        let p = plan(vec![
            step("a", "install", &["true"], true),
            step("b", "testdb", &["true"], false),
        ]);
        let out = format!(
            "{}{}{}",
            frame_begin("@@N", "a"),
            frame_end("@@N", "a", 0, 5, "ok"),
            frame_begin("@@N", "b")
        );
        let r = parse(&p, &out);
        assert_eq!(r.results.len(), 1);
        assert_eq!(r.unfinished.as_deref(), Some("b"));
    }

    #[test]
    fn output_that_looks_like_a_frame_cannot_forge_one() {
        let p = plan(vec![step("a", "install", &["true"], true)]);
        let evil = "@@other end a rc=0 ms=1 len=0\n";
        let out = format!(
            "{}{}",
            frame_begin("@@N", "a"),
            frame_end("@@N", "a", 1, 2, evil)
        );
        let r = parse(&p, &out);
        assert_eq!(r.results.len(), 1);
        assert_eq!((r.results[0].rc, r.results[0].output.as_str()), (1, evil));
    }

    #[test]
    fn a_truncated_output_is_not_a_result() {
        let p = plan(vec![step("a", "install", &["true"], true)]);
        let full = format!(
            "{}{}",
            frame_begin("@@N", "a"),
            frame_end("@@N", "a", 0, 2, "abcdef")
        );
        let r = parse(&p, &full[..full.len() - 4]);
        assert!(r.results.is_empty());
        assert_eq!(r.unfinished.as_deref(), Some("a"));
    }

    #[test]
    fn noise_outside_frames_is_ignored() {
        let p = plan(vec![step("a", "install", &["true"], true)]);
        let out = format!(
            "podman: warning\n{}{}trailing\n",
            frame_begin("@@N", "a"),
            frame_end("@@N", "a", 0, 1, "")
        );
        assert_eq!(parse(&p, &out).results.len(), 1);
    }

    #[test]
    fn the_plan_round_trips_through_json() {
        let p = plan(vec![step("a", "install", &["x", "y z"], true)]);
        let j = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Plan>(&j).unwrap(), p);
    }

    #[test]
    fn stage_runner_links_the_exe_under_target() {
        let d = testkit::TempDir::new("plan-stage");
        let exe = d.join("exe");
        fs::write(&exe, b"bin").unwrap();
        let wt = d.join("wt");
        let got = stage_runner(&wt, &exe).unwrap();
        assert_eq!(got, wt.join(RUNNER_REL));
        assert_eq!(fs::read(&got).unwrap(), b"bin");
        // again: replaced, not an error
        stage_runner(&wt, &exe).unwrap();
    }
}
