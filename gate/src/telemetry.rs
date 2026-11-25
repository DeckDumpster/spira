//! The `gate-run` TSD row (DESIGN.md "Telemetry", sp-cln99): one row per trial, written by
//! the finish beside the gate.log line, so the epic's Intent measures — certification wall by
//! branch type and the no-verdict share — are read from typed fields rather than parsed back
//! out of a free-text meter. gate.log is unchanged; other programs still read it.

use crate::compose::{self, Composition, Forces};
use crate::ports::{Ctx, World};
use serde_json::Value;
use spira_config::GateMode;
use std::path::{Path, PathBuf};

/// The run/tsd family this crate writes.
pub const FAMILY: &str = "gate-run";

/// What the branch touches, independent of what the gate then ran: the Intent's three
/// branch types, plus `unknown` when the trial ended before it could tell (a cached verdict,
/// a conflict, no metadata).
pub fn branch_type(shape: &Composition) -> &'static str {
    match shape {
        Composition::Unit { .. } => "rust-only",
        Composition::Fences => "nothing-buildable",
        Composition::Suites { why } if why == "script" => "bash-touching",
        Composition::Suites { .. } => "unknown",
    }
}

/// The branch's shape as unit mode would compose it, whatever mode is in force and whatever
/// forces apply: under `gate_mode = suites` every composition is `suites(mode)`, which says
/// nothing about the branch. When the composition the trial chose is already unit mode's,
/// it is reused and nothing is recomputed.
#[allow(clippy::too_many_arguments)]
pub fn shape<W: World>(
    w: &W,
    mode: GateMode,
    chosen: &Composition,
    ctx: &Ctx,
    repo: &Path,
    base: &str,
    rev: &str,
    tree: &Path,
) -> Composition {
    let unforced =
        !matches!(chosen, Composition::Suites { why } if why == "ejected" || why == "gate-all");
    if mode == GateMode::Unit && unforced {
        return chosen.clone();
    }
    let Ok(changed) = w.diff_raw(repo, base, rev) else {
        return Composition::Suites {
            why: "no-diff".into(),
        };
    };
    // The launcher's PATH from SPIRA_RELEASE, plus the box's own tool tail (sp-c7b85,
    // amending sp-31gtu, which omitted it): cargo for `cargo metadata` lives there, not in
    // the release. A bad tail (inside a release or a checkout) falls back to no members
    // rather than a panic — `compose::compose` already handles that as "unknown".
    let members = match spira_config::release_path_with_tail(ctx.var(spira_config::RELEASE_ENV), ctx.var("SPIRA_PATH")) {
        Ok(path) => w.cargo_metadata(tree, &path, ctx.var("HOME")).and_then(|j| compose::parse_metadata(&j)),
        Err(e) => Err(e),
    };
    compose::compose(
        GateMode::Unit,
        &Forces::default(),
        &changed,
        members.as_deref().map_err(String::as_str),
    )
}

/// One trial, as the row records it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GateRun {
    pub repo: String,
    pub branch: String,
    pub bead: String,
    pub caller: String,
    /// PASS, FAIL, NO_VERDICT or BASE_FAIL.
    pub status: String,
    pub rc: i32,
    pub reason: String,
    pub waited_secs: u64,
    pub ran_secs: u64,
    /// Seconds of `ran_secs` spent waiting for a testenv slot.
    pub queue_secs: u64,
    /// `unit` / `suites`; empty when the trial ended before the mode was read.
    pub gate_mode: String,
    /// The composition label gate.log carries; empty before one was chosen.
    pub compose: String,
    pub branch_type: String,
    /// `name:secs,…` as gate.log's `phases=`.
    pub phases: String,
    pub compiled: u64,
    pub fresh: u64,
    pub slowest: String,
    /// `warm from <commit>` or `cold: <why>`; empty when no build directory was prepared.
    pub build_dir: String,
    /// The unit gate's build and test phase walls, its nextest test count, and the ten slowest
    /// tests as `<binary>/<test>:secs,…`; zero and empty when no test phase ran.
    pub unit_build_secs: u64,
    pub unit_run_secs: u64,
    pub unit_tests: u64,
    pub unit_slowest: String,
    /// On a branch-red: the unit that failed (`-` when none) and the line that says why; empty otherwise.
    pub red_unit: String,
    pub red_line: String,
    /// `<at>/<digest>` of the deny list the trial ran with; empty when it ended before resolving one.
    pub deny_list: String,
    /// `<step>:<secs>`, the longest step a gate command declared in its steps file; empty when none.
    pub slowest_step: String,
}

/// Render the row through tsd's own builder. `wall_secs` is waited + ran: the certification
/// wall the branch experienced.
pub fn render(ts: &str, host: &str, r: &GateRun) -> Result<String, String> {
    let s = |v: &str| Value::String(v.to_string());
    let bt = if r.branch_type.is_empty() {
        "unknown"
    } else {
        &r.branch_type
    };
    let fields: Vec<(String, Value)> = vec![
        ("repo".into(), s(&r.repo)),
        ("branch".into(), s(&r.branch)),
        ("bead".into(), s(&r.bead)),
        ("caller".into(), s(&r.caller)),
        ("status".into(), s(&r.status)),
        ("rc".into(), Value::from(r.rc)),
        ("reason".into(), s(&r.reason)),
        ("waited_secs".into(), Value::from(r.waited_secs)),
        ("ran_secs".into(), Value::from(r.ran_secs)),
        ("queue_secs".into(), Value::from(r.queue_secs)),
        ("wall_secs".into(), Value::from(r.waited_secs + r.ran_secs)),
        ("gate_mode".into(), s(&r.gate_mode)),
        ("compose".into(), s(&r.compose)),
        ("branch_type".into(), s(bt)),
        ("phases".into(), s(&r.phases)),
        ("compiled".into(), Value::from(r.compiled)),
        ("fresh".into(), Value::from(r.fresh)),
        ("slowest".into(), s(&r.slowest)),
        ("build_dir".into(), s(&r.build_dir)),
        ("unit_build_secs".into(), Value::from(r.unit_build_secs)),
        ("unit_run_secs".into(), Value::from(r.unit_run_secs)),
        ("unit_tests".into(), Value::from(r.unit_tests)),
        ("unit_slowest".into(), s(&r.unit_slowest)),
        ("red_unit".into(), s(&r.red_unit)),
        ("red_line".into(), s(&r.red_line)),
        ("deny_list".into(), s(&r.deny_list)),
        ("slowest_step".into(), s(&r.slowest_step)),
    ];
    tsd::build_row(ts, host, FAMILY, &fields)
}

/// The kind of a build-directory field: `builder`, `warm` or `cold`.
pub fn build_dir_kind(field: &str) -> &str {
    field.split([' ', ':']).next().unwrap_or("")
}

/// The row's build-directory field from the lines `target_on_tmpfs` printed: the way the
/// directory was filled, or why it was not.
pub fn build_dir_field(line: &str) -> String {
    use crate::warm::{BUILDER_IN, COLD_OPEN, WARM_FROM};
    for l in line.lines() {
        if l.starts_with(BUILDER_IN) {
            return "builder".into();
        }
        if let Some(rest) = l.strip_prefix(WARM_FROM) {
            return format!("warm from {}", rest.split(' ').next().unwrap_or(""));
        }
        if let Some((_, rest)) = l.split_once(COLD_OPEN) {
            return format!("cold: {}", rest.split_once(')').map_or(rest, |(why, _)| why));
        }
        if l.contains("builds in the tree") {
            return "cold: no tmpfs, built in the tree".into();
        }
    }
    "cold: no warm directory to adopt".into()
}

/// This machine's id for the envelope (law-producers-stamp-their-own-clock).
pub fn host() -> String {
    std::fs::read_to_string(format!("{}/kernel/hostname", system_paths::PROC_SYS))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "unknown-host".into())
}

/// Append the row to `<run>/tsd/gate-run.jsonl`. Best-effort, as the gate.log line is: a
/// meter never changes a verdict.
pub fn append<W: World>(w: &W, run: &str, ts: &str, r: &GateRun) {
    if run.is_empty() {
        return;
    }
    if let Ok(line) = render(ts, &host(), r) {
        let path = tsd::family_path(Path::new(run), FAMILY);
        if let Some(dir) = path.parent() {
            w.mkdir_p(dir);
        }
        w.append(&path, &format!("{line}\n"));
    }
}

/// Cargo's timing report, kept in the directory of the run record, named by the run's timestamp.
pub fn keep_report<W: World>(w: &W, run: &str, ts: &str, html: &str) {
    if run.is_empty() {
        return;
    }
    if let Some(dir) = tsd::family_path(Path::new(run), FAMILY).parent() {
        w.mkdir_p(dir);
        w.write_atomic(dir, &format!("cargo-timing-{ts}.html"), html);
    }
}

/// How many kept gate outputs stay beside the run record.
pub const KEEP_GATE_OUTS: usize = 50;

/// A kept output is cut to its last bytes: the end of a killed run is where it stopped.
const GATE_OUT_MAX_BYTES: usize = 1 << 20;

const GATE_OUT_PREFIX: &str = "gate-out-";

/// The gate phase's whole output, kept beside the run record under the run's timestamp; older
/// ones past `KEEP_GATE_OUTS` are removed. Returns the path written, None when nothing was.
pub fn keep_gate_out<W: World>(w: &W, run: &str, ts: &str, out: &str) -> Option<PathBuf> {
    if run.is_empty() || out.is_empty() {
        return None;
    }
    let dir = tsd::family_path(Path::new(run), FAMILY).parent()?.to_path_buf();
    w.mkdir_p(&dir);
    let name = format!("{GATE_OUT_PREFIX}{ts}.log");
    w.write_atomic(&dir, &name, &crate::parse::tail_bytes(out, GATE_OUT_MAX_BYTES));
    let mut names = w.list_dir(&dir);
    names.push(name.clone());
    for old in stale_outs(names, KEEP_GATE_OUTS) {
        w.remove(&dir.join(old));
    }
    Some(dir.join(name))
}

/// The kept gate outputs beyond the newest `keep`, oldest first (names sort by timestamp).
pub fn stale_outs(mut names: Vec<String>, keep: usize) -> Vec<String> {
    names.retain(|n| n.starts_with(GATE_OUT_PREFIX) && n.ends_with(".log"));
    names.sort();
    names.dedup();
    let excess = names.len().saturating_sub(keep);
    names.truncate(excess);
    names
}

#[cfg(test)]
mod keep_tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test_id(twl7jzb)]
    #[test]
    fn the_oldest_by_timestamp_goes_and_other_files_stay() {
        let all = names(&["gate-out-2026-02-01T00:00:00Z.log", "cargo-timing-2020-01-01T00:00:00Z.html", "gate-out-2026-01-01T00:00:00Z.log", "gate-out-2026-03-01T00:00:00Z.log"]);
        assert_eq!(stale_outs(all.clone(), 2), names(&["gate-out-2026-01-01T00:00:00Z.log"]));
        assert!(stale_outs(all, 3).is_empty());
    }
}

/// Pushes the trial's run, queue and wait seconds and a run count to the metrics store; the
/// TSD row stays the record. Labels are bounded: no branch or bead.
pub fn push_metrics(sink: &spira_otel::metrics::MetricsSink, r: &GateRun) {
    use spira_otel::metrics::Point;
    let mode = if r.gate_mode.is_empty() { "unknown" } else { &r.gate_mode };
    let bt = if r.branch_type.is_empty() { "unknown" } else { &r.branch_type };
    let secs = |name, v: u64| Point::Seconds { name, value: v as f64 };
    let parts: Vec<(&str, u64)> = r.phases.split(',').filter_map(|p| p.split_once(':')).filter_map(|(n, v)| Some((n, v.parse().ok()?))).collect();
    let recompiled = crate::parse::recompiled_secs(parts.iter().copied());
    sink.push(
        &[
            secs("spira_gate_run_seconds", r.ran_secs),
            secs("spira_gate_queue_seconds", r.queue_secs),
            secs("spira_gate_wait_seconds", r.waited_secs),
            Point::Count { name: "spira_gate_runs", value: 1 },
        ]
        .into_iter()
        .collect::<Vec<_>>(),
        &[("repo", &r.repo), ("status", &r.status), ("gate_mode", mode), ("branch_type", bt)],
    );
    if parts.iter().any(|(n, _)| *n == "test-build") {
        for (part, v) in parts.iter().filter(|(n, _)| crate::parse::TEST_PARTS.contains(n)) {
            sink.push(
                &[secs("spira_gate_unit_test_part_seconds", *v)],
                &[("repo", &r.repo), ("status", &r.status), ("gate_mode", mode), ("branch_type", bt), ("part", part)],
            );
        }
    }
    if let Some(s) = recompiled {
        sink.push(
            &[Point::Count { name: "spira_gate_unit_test_recompiled_total", value: 1 }, secs("spira_gate_unit_test_recompile_seconds", s)],
            &[("repo", &r.repo), ("status", &r.status), ("gate_mode", mode), ("branch_type", bt)],
        );
    }
}

/// The sink for `SPIRA_METRICS_ENDPOINT`; disabled when the config does not resolve.
pub fn metrics_sink() -> spira_otel::metrics::MetricsSink {
    let endpoint = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok();
    spira_otel::metrics::MetricsSink::new(endpoint.as_deref(), "spira-gate", std::time::Duration::from_secs(2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tj4lh5l)]
    #[test]
    fn a_trial_pushes_its_run_queue_and_wait_seconds_and_one_run() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let h = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut c, _) = l.accept().unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = c.read(&mut chunk).unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                let len: usize = head.split("content-length: ").nth(1).unwrap().split("\r\n").next().unwrap().parse().unwrap();
                if buf.len() >= i + 4 + len {
                    break;
                }
            }
            c.write_all(b"HTTP/1.1 204 No Content\r\n\r\n").unwrap();
            String::from_utf8_lossy(&buf).into_owned()
        });
        let sink = spira_otel::metrics::MetricsSink::new(Some(&addr), "spira-gate", std::time::Duration::from_secs(2));
        let r = GateRun { repo: "r".into(), status: "PASS".into(), ran_secs: 40, queue_secs: 3, waited_secs: 7, ..Default::default() };
        push_metrics(&sink, &r);
        let body = h.join().unwrap();
        for name in ["spira_gate_run_seconds", "spira_gate_queue_seconds", "spira_gate_wait_seconds", "spira_gate_runs", "gate_mode", "branch_type"] {
            assert!(body.contains(name), "{name} missing from the push");
        }
    }

    #[test_id(t3hl28v)]
    #[test]
    fn a_unit_trial_pushes_each_test_part_and_a_recompile() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        l.set_nonblocking(true).unwrap();
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = done.clone();
        let h = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let mut all = String::new();
            loop {
                let mut c = match l.accept() {
                    Ok((c, _)) => c,
                    Err(_) if stop.load(std::sync::atomic::Ordering::SeqCst) => break,
                    Err(_) => {
                        std::thread::yield_now();
                        continue;
                    }
                };
                c.set_nonblocking(false).unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = c.read(&mut chunk).unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                    let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                    let len: usize = head.split("content-length: ").nth(1).unwrap().split("\r\n").next().unwrap().parse().unwrap();
                    if buf.len() >= i + 4 + len {
                        break;
                    }
                }
                c.write_all(b"HTTP/1.1 204 No Content\r\n\r\n").unwrap();
                all.push_str(&String::from_utf8_lossy(&buf));
            }
            all
        });
        let sink = spira_otel::metrics::MetricsSink::new(Some(&addr), "spira-gate", std::time::Duration::from_secs(2));
        let r = GateRun { repo: "r".into(), status: "PASS".into(), phases: "build:1,test-build:207,test-list:2,test:5,test-integration:0,test-other:1".into(), ..Default::default() };
        push_metrics(&sink, &r);
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        let body = h.join().unwrap();
        assert!(body.contains("spira_gate_unit_test_part_seconds") && body.contains("part") && body.contains("test-build"), "{body}");
        assert!(body.contains("spira_gate_unit_test_recompiled_total"), "{body}");
    }

    #[test_id(tx32x6r)]
    #[test]
    fn branch_types_follow_the_unit_composition() {
        let unit = Composition::Unit {
            touched: vec!["gate".into()],
            crates: vec!["gate".into()],
            targets: Default::default(),
            test_bins: vec![],
            dropped: vec![],
        };
        assert_eq!(branch_type(&unit), "rust-only");
        assert_eq!(branch_type(&Composition::Fences), "nothing-buildable");
        assert_eq!(
            branch_type(&Composition::Suites {
                why: "script".into()
            }),
            "bash-touching"
        );
        for why in ["mode", "no-metadata", "no-diff", "ejected"] {
            assert_eq!(
                branch_type(&Composition::Suites { why: why.into() }),
                "unknown",
                "{why}"
            );
        }
    }

    #[test_id(tie7yk3)]
    #[test]
    fn the_row_carries_typed_fields_and_the_wall() {
        let r = GateRun {
            repo: "spira".into(),
            branch: "concierge/sp-x".into(),
            bead: "sp-x".into(),
            caller: "landing-pass".into(),
            status: "NO_VERDICT".into(),
            rc: 75,
            reason: "timeout".into(),
            waited_secs: 12,
            ran_secs: 150,
            queue_secs: 40,
            gate_mode: "unit".into(),
            compose: "unit".into(),
            branch_type: "rust-only".into(),
            phases: "fences:30,unit:120".into(),
            build_dir: "warm from abc".into(),
            ..Default::default()
        };
        let line = render("2026-09-29T00:00:00Z", "h1", &r).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["family"], "gate-run");
        assert_eq!(v["host"], "h1");
        assert_eq!(v["status"], "NO_VERDICT");
        assert_eq!(v["rc"], 75);
        assert_eq!(v["wall_secs"], 162);
        assert_eq!(v["ran_secs"], 150);
        assert_eq!(v["queue_secs"], 40);
        assert_eq!(v["branch_type"], "rust-only");
        assert_eq!(v["gate_mode"], "unit");
        assert_eq!(v["phases"], "fences:30,unit:120");
        assert_eq!(v["build_dir"], "warm from abc");
    }

    #[test_id(t9isqt4)]
    #[test]
    fn a_branch_red_renders_its_unit_and_line_and_a_pass_renders_both_empty() {
        let red = GateRun { status: "FAIL".into(), reason: "branch-red".into(), red_unit: "unit-tests".into(), red_line: "bwrap: execvp cargo".into(), ..Default::default() };
        let v: Value = serde_json::from_str(&render("t", "h", &red).unwrap()).unwrap();
        assert_eq!((v["red_unit"].as_str(), v["red_line"].as_str()), (Some("unit-tests"), Some("bwrap: execvp cargo")));
        let pass = GateRun { status: "PASS".into(), ..Default::default() };
        let v: Value = serde_json::from_str(&render("t", "h", &pass).unwrap()).unwrap();
        assert_eq!((v["red_unit"].as_str(), v["red_line"].as_str()), (Some(""), Some("")));
    }

    #[test_id(tf24on0)]
    #[test]
    fn the_build_directory_field_says_how_it_was_filled_or_why_cold() {
        let b = "gate: build on /t/x";
        assert_eq!(build_dir_field(&format!("{b}\n{}c0ffee (12 unchanged files aged)", crate::warm::WARM_FROM)), "warm from c0ffee");
        assert_eq!(build_dir_field(&format!("{b}\ngate: warm build directory from c1 discarded, {}mtimes: denied)", crate::warm::COLD_OPEN)), "cold: mtimes: denied");
        assert_eq!(build_dir_field("gate: no tmpfs for the build (x) — /t builds in the tree"), "cold: no tmpfs, built in the tree");
        assert_eq!(build_dir_field(b), "cold: no warm directory to adopt");
        let leased = format!("{b}\n{}/pool/dir.2 (leased from sp-a's builder at c1)", crate::warm::BUILDER_IN);
        assert_eq!(build_dir_field(&leased), "builder");
        assert_eq!(["builder", "warm from c1", "cold: no tmpfs"].map(build_dir_kind), ["builder", "warm", "cold"]);
    }

    #[test_id(t0bh4o2)]
    #[test]
    fn an_unclassified_trial_says_unknown_not_empty() {
        let line = render("t", "h", &GateRun::default()).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["branch_type"], "unknown");
    }
}
