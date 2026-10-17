//! The `gate-run` TSD row (DESIGN.md "Telemetry", sp-cln99): one row per trial, written by
//! the finish beside the gate.log line, so the epic's Intent measures — certification wall by
//! branch type and the no-verdict share — are read from typed fields rather than parsed back
//! out of a free-text meter. gate.log is unchanged; other programs still read it.

use crate::compose::{self, Composition, Forces};
use crate::ports::{Ctx, World};
use serde_json::Value;
use spira_config::GateMode;
use std::path::Path;

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
    ];
    tsd::build_row(ts, host, FAMILY, &fields)
}

/// This machine's id for the envelope (law-producers-stamp-their-own-clock).
pub fn host() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
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

/// Pushes the trial's run, queue and wait seconds and a run count to the metrics store; the
/// TSD row stays the record. Labels are bounded: no branch or bead.
pub fn push_metrics(sink: &spira_otel::metrics::MetricsSink, r: &GateRun) {
    use spira_otel::metrics::Point;
    let mode = if r.gate_mode.is_empty() { "unknown" } else { &r.gate_mode };
    let bt = if r.branch_type.is_empty() { "unknown" } else { &r.branch_type };
    let secs = |name, v: u64| Point::Seconds { name, value: v as f64 };
    sink.push(
        &[
            secs("spira_gate_run_seconds", r.ran_secs),
            secs("spira_gate_queue_seconds", r.queue_secs),
            secs("spira_gate_wait_seconds", r.waited_secs),
            Point::Count { name: "spira_gate_runs", value: 1 },
        ],
        &[("repo", &r.repo), ("status", &r.status), ("gate_mode", mode), ("branch_type", bt)],
    );
}

/// The sink for `SPIRA_METRICS_ENDPOINT`; disabled when the config does not resolve.
pub fn metrics_sink() -> spira_otel::metrics::MetricsSink {
    let endpoint = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok();
    spira_otel::metrics::MetricsSink::new(endpoint.as_deref(), "spira-gate", std::time::Duration::from_secs(2))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn branch_types_follow_the_unit_composition() {
        let unit = Composition::Unit {
            touched: vec!["gate".into()],
            crates: vec!["gate".into()],
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
    }

    #[test]
    fn an_unclassified_trial_says_unknown_not_empty() {
        let line = render("t", "h", &GateRun::default()).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["branch_type"], "unknown");
    }
}
