use super::*;

const T0: u64 = 1_790_640_000; // 2026-09-29T00:00:00Z

fn ts(off: u64) -> String {
    tsd::iso_utc(T0 + off)
}

fn all() -> Window {
    Window {
        from: 0,
        to: u64::MAX,
    }
}

fn gate_row(
    off: u64,
    status: &str,
    reason: &str,
    waited: u64,
    ran: u64,
    mode: &str,
    bt: &str,
) -> String {
    format!(
        "{{\"ts\":\"{}\",\"host\":\"h\",\"family\":\"gate-run\",\"status\":\"{status}\",\"reason\":\"{reason}\",\"waited_secs\":{waited},\"ran_secs\":{ran},\"wall_secs\":{},\"gate_mode\":\"{mode}\",\"branch_type\":\"{bt}\"}}\n",
        ts(off),
        waited + ran
    )
}

#[test]
fn durations_and_iso_times_both_parse() {
    assert_eq!(parse_when("24h", T0), Some(T0 - 86_400));
    assert_eq!(parse_when("90m", T0), Some(T0 - 5_400));
    assert_eq!(parse_when("7d", T0), Some(T0 - 7 * 86_400));
    assert_eq!(parse_when("30s", T0), Some(T0 - 30));
    assert_eq!(parse_when("2026-09-29", T0 + 5), Some(T0));
    assert_eq!(parse_when("soon", T0), None);
}

#[test]
fn quantiles_interpolate_and_say_nothing_on_no_data() {
    assert_eq!(quantile(&[], 0.5), None);
    assert_eq!(quantile(&[10.0, 20.0], 0.5), Some(15.0));
    assert_eq!(quantile(&[3.0, 1.0, 2.0], 0.5), Some(2.0));
    assert_eq!(quantile(&[1.0, 9.0], 1.0), Some(9.0));
}

#[test]
fn gate_log_lines_backfill_status_type_and_mode() {
    let t = gate_log_line("2026-09-29T00:00:10Z spira concierge/sp-a waited=5s ran=100s rc=0 pass compose=unit phases=fences:30,build:40,test:30").unwrap();
    assert_eq!(t.ts, T0 + 10);
    assert_eq!((t.status.as_str(), t.reason.as_str()), ("PASS", "pass"));
    assert_eq!(
        (t.branch_type.as_str(), t.gate_mode.as_str()),
        ("rust-only", "unit")
    );
    assert_eq!(t.wall(), 105.0);
    assert!(t.from_log);
    let t = gate_log_line("2026-09-29T00:00:10Z spira b waited=0s ran=907s rc=75 timeout").unwrap();
    assert_eq!(t.status, "NO_VERDICT");
    assert_eq!(
        (t.branch_type.as_str(), t.gate_mode.as_str()),
        ("unknown", "")
    );
    let t = gate_log_line("2026-09-29T00:00:10Z spira b waited=0s ran=1s rc=76 base-red compose=suites(mode) phases=gate:1").unwrap();
    assert_eq!(
        (t.status.as_str(), t.gate_mode.as_str()),
        ("BASE_FAIL", "suites")
    );
    let t = gate_log_line("2026-09-29T00:00:10Z spira b waited=0s ran=1s rc=1 branch-red compose=suites(script) phases=gate:1").unwrap();
    assert_eq!(
        (t.status.as_str(), t.branch_type.as_str()),
        ("FAIL", "bash-touching")
    );
    assert_eq!(
        t.gate_mode, "unit",
        "only unit mode composes suites for a reason other than the mode"
    );
    let t = gate_log_line(
        "2026-09-29T00:00:10Z spira b waited=0s ran=1s rc=0 compose=fences phases=fences:1",
    )
    .unwrap();
    assert_eq!(
        (t.reason.as_str(), t.branch_type.as_str()),
        ("", "nothing-buildable")
    );
    assert!(gate_log_line("garbage").is_none());
    assert!(gate_log_line("").is_none());
}

#[test]
fn gate_log_is_read_only_before_the_first_gate_run_row() {
    let log = "2026-09-29T00:00:10Z spira a waited=0s ran=10s rc=0 pass\n\
               2026-09-29T00:01:40Z spira b waited=0s ran=10s rc=75 timeout\n";
    let rows = gate_row(60, "PASS", "pass", 0, 20, "unit", "rust-only");
    let t = trials(&rows, Some(log), all());
    assert_eq!(
        t.len(),
        2,
        "the log line after the first row is not double-counted"
    );
    assert!(t[0].from_log && !t[1].from_log);
    assert_eq!(trials(&rows, None, all()).len(), 1);
    let w = Window {
        from: T0 + 30,
        to: T0 + 90,
    };
    assert_eq!(
        trials(&rows, Some(log), w).len(),
        1,
        "the window applies to both"
    );
}

#[test]
fn the_report_prints_every_intent_measure_against_its_target() {
    let mut rows = String::new();
    rows += &gate_row(1, "PASS", "pass", 10, 110, "unit", "rust-only"); // 120
    rows += &gate_row(2, "PASS", "pass", 0, 200, "unit", "rust-only"); // 200
    rows += &gate_row(3, "PASS", "pass", 0, 140, "unit", "rust-only"); // 140
    rows += &gate_row(4, "PASS", "cached", 0, 0, "unit", "rust-only");
    rows += &gate_row(
        5,
        "NO_VERDICT",
        "timeout",
        0,
        900,
        "suites",
        "bash-touching",
    );
    rows += &gate_row(6, "FAIL", "branch-red", 0, 400, "suites", "bash-touching");
    let ra = format!(
        "{{\"ts\":\"{a}\",\"family\":\"round-attribution\",\"outcome\":\"owner\",\"owner\":\"sp-1\",\"attribution_secs\":\"600\"}}\n\
         {{\"ts\":\"{a}\",\"family\":\"round-attribution\",\"outcome\":\"owner\",\"owner\":\"sp-2\",\"attribution_secs\":\"1000\"}}\n\
         {{\"ts\":\"{a}\",\"family\":\"round-attribution\",\"outcome\":\"unsettled\",\"owner\":\"\",\"attribution_secs\":\"\"}}\n\
         {{\"ts\":\"{a}\",\"family\":\"round-attribution\",\"outcome\":\"flaky\",\"owner\":\"\",\"attribution_secs\":\"30\"}}\n",
        a = ts(7)
    );
    let st = format!(
        "{{\"ts\":\"{a}\",\"family\":\"suite-timing\",\"suite\":\"test-a.sh\",\"wall_secs\":100,\"bd_calls\":4,\"bd_ms\":20000}}\n\
         {{\"ts\":\"{a}\",\"family\":\"suite-timing\",\"suite\":\"test-b.sh\",\"wall_secs\":100,\"bd_calls\":0,\"bd_ms\":0}}\n\
         {{\"ts\":\"{a}\",\"family\":\"suite-timing\",\"suite\":\"__batch__\",\"wall_secs\":999,\"bd_calls\":0,\"bd_ms\":0}}\n",
        a = ts(8)
    );
    let le = format!(
        "{{\"ts\":\"{}\",\"family\":\"landing-event\",\"bead\":\"sp-1\",\"state\":\"CERTIFIED\"}}\n\
         {{\"ts\":\"{}\",\"family\":\"landing-event\",\"bead\":\"sp-1\",\"state\":\"LANDED\"}}\n\
         {{\"ts\":\"{}\",\"family\":\"landing-event\",\"bead\":\"sp-1\",\"state\":\"LANDED\"}}\n",
        ts(10),
        ts(70),
        ts(72)
    );
    let out = render(
        &Inputs {
            gate_run: &rows,
            gate_log: None,
            round_attribution: &ra,
            suite_timing: &st,
            landing_event: &le,
        },
        all(),
    );
    assert!(
        out.contains("gate trials: 6 (6 from gate-run rows, 0 backfilled"),
        "{out}"
    );
    assert!(
        out.contains(
            "TARGET median certification, rust-only: 140 s over 3 passes vs <= 180 s -> MET"
        ),
        "the cached pass is not a certification: {out}"
    );
    assert!(
        out.contains("cached verdicts (no work, excluded above): 1"),
        "{out}"
    );
    assert!(
        out.contains("2. No-verdict share: 1 / 6 = 16.7% vs < 5% -> NOT MET"),
        "{out}"
    );
    assert!(out.contains("      1  timeout"), "{out}");
    assert!(out.contains("2 owned reds; median 800 s"), "{out}");
    assert!(out.contains("unsettled 1"), "{out}");
    assert!(
        out.contains("TARGET <= 900 s per member defect (max): NOT MET"),
        "{out}"
    );
    assert!(
        out.contains("2 suite runs of 2 suites; metered 1 (50.0%)"),
        "{out}"
    );
    assert!(
        out.contains("bd wall 20 s of 200 s suite wall (10.0%)"),
        "{out}"
    );
    assert!(out.contains("test-a.sh"), "{out}");
    assert!(
        out.contains("5. Certified -> landed: 1 landings; median 60 s"),
        "{out}"
    );
}

#[test]
fn an_empty_window_says_no_data_never_zero_or_met() {
    let out = render(
        &Inputs {
            gate_run: "",
            gate_log: None,
            round_attribution: "",
            suite_timing: "",
            landing_event: "",
        },
        all(),
    );
    assert!(
        out.contains("rust-only: ? s over 0 passes vs <= 180 s -> NO DATA"),
        "{out}"
    );
    assert!(
        out.contains("No-verdict share: 0 / 0 = ? vs < 5% -> NO DATA"),
        "{out}"
    );
    assert!(out.contains("(max): NO DATA"), "{out}");
    assert!(out.contains("bd wait: ? (no metered suite run"), "{out}");
    assert!(!out.contains("-> MET"), "{out}");
}

#[test]
fn unmetered_suite_rows_are_not_reported_as_zero_wait() {
    let st = format!(
        "{{\"ts\":\"{}\",\"family\":\"suite-timing\",\"suite\":\"test-a.sh\",\"wall_secs\":5,\"bd_calls\":0,\"bd_ms\":0}}\n",
        ts(1)
    );
    let bd = bd_wait(&st, all());
    assert_eq!(bd["test-a.sh"].metered, 0);
    let out = render(
        &Inputs {
            gate_run: "",
            gate_log: None,
            round_attribution: "",
            suite_timing: &st,
            landing_event: "",
        },
        all(),
    );
    assert!(
        out.contains("metered 0 (0.0%)") && out.contains("bd wait: ?"),
        "{out}"
    );
}
