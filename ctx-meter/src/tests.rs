//! Expected strings were captured from `spira/ctx-meter.sh` on the same fixtures before it was
//! deleted; `NOW` is the clock every case runs at.

use super::*;
use std::fs;
use std::path::Path;
use testkit::TempDir;

const NOW: i64 = 1_800_000_000;
const TP: &str = "projects/p/s1.jsonl";
const INFLIGHT_BAD: &str = " \x1b[31m⚙ ?inflight\x1b[0m";

struct Rig {
    dir: TempDir,
}

impl Rig {
    fn new() -> Rig {
        Rig { dir: TempDir::new("ctx-meter") }
    }
    fn cfg(&self) -> Config {
        Config { warn: 200_000, high: 400_000, limit: 1_000_000, projects: self.dir.join("projects"), run: self.dir.join("run"), idle: 1800, hhmm: |t| format!("{:02}:{:02}", t % 86400 / 3600, t % 3600 / 60) }
    }
    fn put(&self, rel: &str, text: &str) -> &Rig {
        let p = self.dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, text).unwrap();
        self
    }
    fn touch(&self, rel: &str, at: i64) -> &Rig {
        let f = fs::File::options().write(true).open(self.dir.join(rel)).unwrap();
        f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(at as u64)).unwrap();
        self
    }
    fn append(&self, rel: &str, text: &str) {
        use std::io::Write;
        fs::File::options().append(true).open(self.dir.join(rel)).unwrap().write_all(text.as_bytes()).unwrap();
    }
    fn tp(&self) -> String {
        self.dir.join(TP).display().to_string()
    }
    fn hook(&self, context: Option<i64>, rate_limits: &str) -> String {
        let cw = context.map(|c| format!(r#","context_window":{{"current_usage":{{}},"total_input_tokens":{c}}}"#)).unwrap_or_default();
        format!(r#"{{"session_id":"s1","transcript_path":"{}"{cw}{rate_limits}}}"#, self.tp())
    }
    fn line(&self, stdin: &str) -> String {
        line(&self.cfg(), NOW, stdin, &self.dir)
    }
    fn env(&self, pick: Option<&str>) -> String {
        env(&self.cfg(), NOW, pick.map(Path::new)).join("\n")
    }
    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.dir.join(rel)).unwrap_or_default()
    }
}

fn row(n: usize, total: i64) -> String {
    format!(r#"{{"type":"assistant","message":{{"id":"m{n}","usage":{{"input_tokens":{},"cache_creation_input_tokens":{},"cache_read_input_tokens":{}}}}}}}{}"#, total / 3, total / 3, total - 2 * (total / 3), "\n")
}

fn transcript(totals: &[i64]) -> String {
    totals.iter().enumerate().map(|(n, t)| row(n, *t)).collect()
}

fn climbing() -> Vec<i64> {
    (0..25).map(|i| 100_000 + i * 5000).collect()
}

fn rate_limits(five: Option<(f64, i64)>, seven: Option<(f64, i64)>) -> String {
    let w = |name: &str, x: Option<(f64, i64)>| x.map(|(p, r)| format!(r#""{name}":{{"used_percentage":{p},"resets_at":{r}}}"#));
    format!(r#","rate_limits":{{{}}}"#, [w("five_hour", five), w("seven_day", seven)].into_iter().flatten().collect::<Vec<_>>().join(","))
}

fn samples(rows: &[(i64, &str, &str)]) -> String {
    rows.iter().map(|(t, a, b)| format!("{t} {a} {b}\n")).collect()
}

fn lined(rig: &Rig, context: Option<i64>) -> String {
    rig.line(&rig.hook(context, ""))
}

// ---- the line: headline, rounding, growth -------------------------------------------------

#[test_id(ttnu0bj)]
#[test]
fn a_session_the_hook_named_that_has_not_spoken_is_fresh_not_unreadable() {
    let rig = Rig::new();
    assert_eq!(rig.line(r#"{"session_id":"s1"}"#), format!("\x1b[2m· ctx fresh\x1b[0m{INFLIGHT_BAD}"));
}

#[test_id(tzn4va5)]
#[test]
fn a_hook_that_names_no_session_and_finds_no_transcript_prints_ctx_question_mark() {
    let rig = Rig::new();
    assert_eq!(rig.line(""), format!("ctx ?{INFLIGHT_BAD}"));
    assert_eq!(rig.line("not json"), format!("ctx ?{INFLIGHT_BAD}"));
    assert_eq!(rig.line("[1,2]"), format!("ctx ?{INFLIGHT_BAD}"));
}

#[test_id(t5m3sd1)]
#[test]
fn a_null_current_usage_is_the_fresh_signal_even_with_a_total() {
    let rig = Rig::new();
    let hook = format!(r#"{{"session_id":"s1","context_window":{{"current_usage":null,"total_input_tokens":5000}}}}"#);
    assert_eq!(rig.line(&hook), format!("\x1b[2m· ctx fresh\x1b[0m{INFLIGHT_BAD}"));
}

#[test_id(tceni82)]
#[test]
fn the_headline_rounds_thousands_half_to_even_like_printf() {
    let rig = Rig::new();
    rig.put(TP, &transcript(&[100, 200]));
    for (ctx, shown) in [(0, "0k"), (2499, "2k"), (2500, "2k"), (3500, "4k"), (12345, "12k")] {
        assert_eq!(lined(&rig, Some(ctx)), format!("\x1b[32m▁ ctx {shown}\x1b[0m \x1b[2m/2t\x1b[0m{INFLIGHT_BAD}"), "ctx {ctx}");
    }
}

#[test_id(tkq4ksh)]
#[test]
fn the_colour_changes_at_each_threshold_and_the_bar_fills_to_the_limit() {
    let rig = Rig::new();
    rig.put(TP, &transcript(&[100, 200]));
    let head = |ctx| lined(&rig, Some(ctx)).split("\x1b[0m").next().unwrap().to_string();
    assert_eq!(head(199_999), "\x1b[32m▂ ctx 200k");
    assert_eq!(head(200_000), "\x1b[33m▂ ctx 200k");
    assert_eq!(head(400_000), "\x1b[31m▄ ctx 400k");
    assert_eq!(head(999_999), "\x1b[31m█ ctx 1000k");
    assert_eq!(head(2_500_000), "\x1b[31m█ ctx 2500k");
}

#[test_id(t146oin)]
#[test]
fn growth_per_turn_needs_twenty_one_turns_and_shows_in_tenths_of_a_thousand() {
    let rig = Rig::new();
    rig.put(TP, &transcript(&climbing()));
    assert_eq!(lined(&rig, None), format!("\x1b[33m▂ ctx 220k\x1b[0m \x1b[2m/25t +5.0k/turn\x1b[0m \x1b[2m· not archived\x1b[0m{INFLIGHT_BAD}"));
    let rig = Rig::new();
    rig.put(TP, &transcript(&(0..21).map(|i| 1000 + i * 5).collect::<Vec<_>>()));
    assert_eq!(lined(&rig, None), format!("\x1b[32m▁ ctx 1k\x1b[0m \x1b[2m/21t +0.0k/turn\x1b[0m{INFLIGHT_BAD}"));
    let rig = Rig::new();
    rig.put(TP, &transcript(&[5000; 25]));
    assert_eq!(lined(&rig, None), format!("\x1b[32m▁ ctx 5k\x1b[0m \x1b[2m/25t\x1b[0m{INFLIGHT_BAD}"));
}

#[test_id(t9ne81r)]
#[test]
fn rows_of_one_assistant_message_count_as_one_turn() {
    let rig = Rig::new();
    let doubled: String = climbing().iter().enumerate().map(|(n, t)| row(n, *t).repeat(2)).collect();
    rig.put(TP, &doubled);
    assert_eq!(lined(&rig, None), format!("\x1b[33m▂ ctx 220k\x1b[0m \x1b[2m/25t +5.0k/turn\x1b[0m \x1b[2m· not archived\x1b[0m{INFLIGHT_BAD}"));
}

// ---- the transcript cursor ---------------------------------------------------------------

#[test_id(tl14g18)]
#[test]
fn a_partial_last_line_is_not_consumed_until_the_rest_lands() {
    let rig = Rig::new();
    let whole = row(99, 3000);
    rig.put(TP, &format!("{}{}", transcript(&[1000, 2000, 3000]), &whole[..whole.len() - 20]));
    assert_eq!(lined(&rig, None), format!("\x1b[32m▁ ctx 3k\x1b[0m \x1b[2m/3t\x1b[0m{INFLIGHT_BAD}"));
    rig.append(TP, &whole[whole.len() - 20..]);
    assert_eq!(lined(&rig, None), format!("\x1b[32m▁ ctx 3k\x1b[0m \x1b[2m/4t\x1b[0m{INFLIGHT_BAD}"));
}

#[test_id(tts8ffq)]
#[test]
fn a_steady_state_run_reads_only_what_was_appended() {
    let rig = Rig::new();
    rig.put(TP, &transcript(&[10, 20, 30]));
    rig.line(&rig.hook(None, ""));
    let first = rig.env(Some(&rig.tp()));
    assert!(first.contains("SP_CTX_SCAN_BYTES=0"), "{first}");
    rig.append(TP, &row(7, 5));
    assert!(rig.env(Some(&rig.tp())).contains(&format!("SP_CTX_SCAN_BYTES={}", row(7, 5).len())));
}

#[test_id(t2hsyak)]
#[test]
fn a_transcript_rewritten_in_place_is_read_again_from_the_start() {
    let rig = Rig::new();
    rig.put(TP, &transcript(&[10, 20, 30, 40, 50]));
    assert!(lined(&rig, None).contains("/5t"));
    rig.put(TP, &transcript(&[7, 8]));
    assert!(lined(&rig, None).contains("/2t"));
}

#[test_id(tn088wp)]
#[test]
fn a_transcript_cut_short_is_read_again_from_the_start() {
    let rig = Rig::new();
    rig.put(TP, &transcript(&[10, 20, 30, 40, 50]));
    assert!(lined(&rig, None).contains("/5t"));
    fs::File::options().write(true).open(rig.dir.join(TP)).unwrap().set_len(transcript(&[10, 20]).len() as u64).unwrap();
    assert!(lined(&rig, None).contains("/2t"));
}

#[test_id(t0uy2p0)]
#[test]
fn rows_that_are_not_turns_do_not_count() {
    let rig = Rig::new();
    let junk = format!("\"usage\" junk\n[1,\"usage\"]\n{}{{\"message\":{{\"usage\":{{}},\"id\":\"z\"}}}}\n{{\"message\":\"usage\"}}\n{{\"message\":{{\"usage\":{{\"input_tokens\":null,\"cache_read_input_tokens\":3}},\"id\":\"y\"}}}}\n", row(1, 15));
    rig.put(TP, &junk);
    assert_eq!(lined(&rig, None), format!("\x1b[32m▁ ctx 0k\x1b[0m \x1b[2m/2t\x1b[0m{INFLIGHT_BAD}"));
}

// ---- the archivist's state ---------------------------------------------------------------

fn with_archivist(body: &str) -> String {
    let rig = Rig::new();
    rig.put(TP, &transcript(&climbing())).put("run/archivist/s1.state", body);
    lined(&rig, Some(300_000))
}

fn arc(segment: &str) -> String {
    format!("\x1b[33m▃ ctx 300k\x1b[0m \x1b[2m/25t +5.0k/turn\x1b[0m{segment}{INFLIGHT_BAD}")
}

#[test_id(tyxxeqo)]
#[test]
fn every_archivist_state_has_its_own_segment() {
    assert_eq!(with_archivist("state=sweeping\n"), arc(" \x1b[36m⟳ sweeping\x1b[0m"));
    assert_eq!(with_archivist("state=archiving\n"), arc(" \x1b[36m⇣ archiving\x1b[0m"));
    assert_eq!(with_archivist("state=safe\nitems_filed=4\nat_turn=25\n"), arc(" \x1b[32m✓ safe to clear\x1b[0m \x1b[2m(4 filed)\x1b[0m"));
    assert_eq!(with_archivist("state=failed\n"), arc(" \x1b[31m! archive failed for good (run: archivist now s1)\x1b[0m"));
    assert_eq!(with_archivist("state=timeout\n"), arc(" \x1b[33m⏸ archive timed out, retrying\x1b[0m"));
    assert_eq!(with_archivist("state=capacity\n"), arc(" \x1b[33m⏸ archive deferred (capacity)\x1b[0m"));
    assert_eq!(with_archivist("state=done\n"), arc(" \x1b[2mdone\x1b[0m"));
}

#[test_id(t5outft)]
#[test]
fn a_safe_verdict_older_than_two_turns_says_how_old() {
    assert_eq!(with_archivist("state=safe\nitems_filed=4\nat_turn=23\n"), arc(" \x1b[32m✓ safe to clear\x1b[0m \x1b[2m(4 filed)\x1b[0m"));
    assert_eq!(with_archivist("state=safe\nitems_filed=4\nat_turn=20\n"), arc(" \x1b[33m✓ safe as of 5t ago\x1b[0m"));
}

#[test_id(trjzmjh)]
#[test]
fn a_failed_or_deferred_archive_names_the_time_it_will_retry() {
    assert_eq!(with_archivist("state=failed\nretry_at=1800003600\n"), arc(" \x1b[33m⏸ archive failed, will retry at 09:00\x1b[0m"));
    assert_eq!(with_archivist("state=retry\nretry_at=1800003600\n"), arc(" \x1b[33m⏸ archive deferred, will retry at 09:00\x1b[0m"));
}

#[test_id(t3uzymo)]
#[test]
fn an_unreadable_archivist_state_is_a_question_mark_not_nothing() {
    assert_eq!(with_archivist("state=safe\nat_turn=abc\n"), arc(" \x1b[31m· archivist ?\x1b[0m"));
    let rig = Rig::new();
    rig.put(TP, &transcript(&climbing())).put("run/archivist/s1.state/x", "a");
    assert_eq!(lined(&rig, Some(250_000)), "\x1b[33m▃ ctx 250k\x1b[0m \x1b[2m/25t +5.0k/turn\x1b[0m \x1b[31m· archivist ?\x1b[0m".to_string() + INFLIGHT_BAD);
}

#[test_id(tx02tjj)]
#[test]
fn no_state_file_is_quiet_below_the_warn_line_and_not_archived_above_it() {
    let rig = Rig::new();
    rig.put(TP, &transcript(&climbing()));
    assert_eq!(lined(&rig, Some(1000)), format!("\x1b[32m▁ ctx 1k\x1b[0m \x1b[2m/25t +5.0k/turn\x1b[0m{INFLIGHT_BAD}"));
    assert_eq!(lined(&rig, Some(250_000)), format!("\x1b[33m▃ ctx 250k\x1b[0m \x1b[2m/25t +5.0k/turn\x1b[0m \x1b[2m· not archived\x1b[0m{INFLIGHT_BAD}"));
}

#[test_id(tw680ju)]
#[test]
fn a_sweep_skipped_for_capacity_shows_at_any_context() {
    let rig = Rig::new();
    rig.put(TP, &transcript(&climbing())).put("run/archivist/sweep.state", "sweep_state=skipped\nreason=capacity\n");
    assert_eq!(lined(&rig, Some(1000)), format!("\x1b[32m▁ ctx 1k\x1b[0m \x1b[2m/25t +5.0k/turn\x1b[0m \x1b[33m⏸ skipped (capacity)\x1b[0m{INFLIGHT_BAD}"));
}

// ---- work in flight ----------------------------------------------------------------------

fn inflight(snapshot: Option<(&str, i64)>) -> String {
    let rig = Rig::new();
    rig.put(TP, &transcript(&[100, 200]));
    if let Some((text, mtime)) = snapshot {
        rig.put("run/cockpit.env", text).touch("run/cockpit.env", mtime);
    }
    lined(&rig, Some(5000)).strip_prefix("\x1b[32m▁ ctx 5k\x1b[0m \x1b[2m/2t\x1b[0m").unwrap().to_string()
}

#[test_id(tbchxzx)]
#[test]
fn the_in_flight_segment_is_the_collectors_snapshot_while_it_is_fresh() {
    assert_eq!(inflight(Some(("SP_INFLIGHT_SEG='3 in flight'\n", NOW - 5))), " \x1b[2m3 in flight\x1b[0m");
    assert_eq!(inflight(Some(("SP_INFLIGHT_SEG='x'\n", NOW - 60))), " \x1b[2mx\x1b[0m");
    assert_eq!(inflight(Some(("SP_INFLIGHT_SEG=abc\n", NOW))), " \x1b[2mabc\x1b[0m");
    assert_eq!(inflight(Some(("SP_INFLIGHT_SEG='?x'\n", NOW))), " \x1b[31m?x\x1b[0m");
}

#[test_id(tifgnpm)]
#[test]
fn a_missing_stale_or_empty_in_flight_snapshot_is_a_red_question_never_idle() {
    assert_eq!(inflight(None), INFLIGHT_BAD);
    assert_eq!(inflight(Some(("SP_INFLIGHT_SEG='x'\n", NOW - 61))), INFLIGHT_BAD);
    assert_eq!(inflight(Some(("SP_INFLIGHT_SEG=''\n", NOW))), INFLIGHT_BAD);
    assert_eq!(inflight(Some(("OTHER=1\n", NOW))), INFLIGHT_BAD);
}

// ---- the operator pointer ----------------------------------------------------------------

fn pointer_to(rig: &Rig, rel: &str, ts: i64) {
    rig.put("run/ctx-operator.ptr", &format!("tp={}\nts={ts}\nsid=o\n", rig.dir.join(rel).display()));
}

// covers: UC-cockpit-observability-36
#[test_id(t95ksbv)]
#[test]
fn a_status_line_tick_writes_the_pointer_env_reads() {
    let rig = Rig::new();
    rig.put(TP, &transcript(&climbing()));
    lined(&rig, None);
    assert_eq!(rig.read("run/ctx-operator.ptr"), format!("tp={}\nts={NOW}\nsid=s1\n", rig.tp()));
    assert!(rig.env(None).starts_with("SP_CTX_NOW=220000\nSP_CTX_TURNS=25\nSP_CTX_GROWTH=5000\nSP_CTX_NEXT=high\nSP_CTX_HEADROOM=180000\nSP_CTX_TURNS_LEFT=36\nSP_CTX_AGE=0\n"));
}

// covers: UC-cockpit-observability-36
#[test_id(tan74yf)]
#[test]
fn an_idle_second_session_cannot_take_the_pointer_from_a_holder_that_spoke_more_recently() {
    let rig = Rig::new();
    rig.put(TP, &transcript(&[10, 20])).put("projects/p/other.jsonl", &transcript(&[1])).touch(TP, NOW - 500).touch("projects/p/other.jsonl", NOW - 5);
    pointer_to(&rig, "projects/p/other.jsonl", NOW - 10);
    let before = rig.read("run/ctx-operator.ptr");
    lined(&rig, None);
    assert_eq!(rig.read("run/ctx-operator.ptr"), before);
}

// covers: UC-cockpit-observability-36
#[test_id(tlud0r4)]
#[test]
fn a_stale_holder_never_keeps_the_pointer_and_an_older_holder_yields_it() {
    for (holder_at, holder_mtime) in [(NOW - 5000, NOW - 5), (NOW - 10, NOW - 500)] {
        let rig = Rig::new();
        rig.put(TP, &transcript(&[10, 20])).put("projects/p/other.jsonl", &transcript(&[1])).touch(TP, NOW - 5).touch("projects/p/other.jsonl", holder_mtime);
        pointer_to(&rig, "projects/p/other.jsonl", holder_at);
        lined(&rig, None);
        assert_eq!(rig.read("run/ctx-operator.ptr"), format!("tp={}\nts={NOW}\nsid=s1\n", rig.tp()), "holder at {holder_at}");
    }
}

// covers: UC-cockpit-observability-36
#[test_id(tzqa9tw)]
#[test]
fn a_stale_deleted_or_corrupt_pointer_means_no_session() {
    let none = |age: &str| format!("SP_CTX_NOW=-\nSP_CTX_TURNS=-\nSP_CTX_GROWTH=-\nSP_CTX_NEXT=-\nSP_CTX_HEADROOM=-\nSP_CTX_TURNS_LEFT=-\nSP_CTX_AGE={age}\nSP_CTX_ARCHIVIST=-\nSP_CTX_ARCHIVIST_BEHIND=-\nSP_CTX_ARCHIVIST_FILED=-\nSP_CTX_SCAN_BYTES=0\nSP_LIMIT_5H_PCT=-\nSP_LIMIT_5H_ETA=-\nSP_LIMIT_7D_PCT=-\nSP_LIMIT_7D_ETA=-\nSP_LIMIT_AGE=-");
    let rig = Rig::new();
    assert_eq!(rig.env(None), none("-"));
    rig.put(TP, &transcript(&climbing()));
    pointer_to(&rig, TP, NOW - 1801);
    assert_eq!(rig.env(None), none("1801"));
    pointer_to(&rig, "projects/p/gone.jsonl", NOW - 30);
    assert_eq!(rig.env(None), none("30"));
    rig.put("run/ctx-operator.ptr", "tp=x\nts=abc\n");
    assert_eq!(rig.env(None), none("-"));
    pointer_to(&rig, TP, NOW - 1800);
    assert!(rig.env(None).contains("SP_CTX_AGE=1800\n"), "a holder exactly at the idle threshold is still live");
}

#[test_id(t99fzf0)]
#[test]
fn a_named_transcript_wins_over_the_pointer() {
    let rig = Rig::new();
    rig.put(TP, &transcript(&climbing())).put("projects/p/s2.jsonl", &transcript(&[10, 20]));
    pointer_to(&rig, TP, NOW - 30);
    let named = rig.env(Some(&rig.dir.join("projects/p/s2.jsonl").display().to_string()));
    assert!(named.starts_with("SP_CTX_NOW=20\nSP_CTX_TURNS=2\nSP_CTX_GROWTH=0\nSP_CTX_NEXT=warn\nSP_CTX_HEADROOM=199980\nSP_CTX_TURNS_LEFT=-\n"), "{named}");
}

#[test_id(tzy8gmb)]
#[test]
fn a_named_transcript_that_is_missing_or_has_no_turns_prints_dashes() {
    let rig = Rig::new();
    assert!(rig.env(Some("/nonexistent/s.jsonl")).contains("SP_CTX_NOW=-\n"));
    rig.put(TP, "{\"x\":1}\n");
    let e = rig.env(Some(&rig.tp()));
    assert!(e.contains("SP_CTX_NOW=-\n") && e.contains("SP_CTX_SCAN_BYTES=8\n"), "{e}");
}

#[test_id(t912z70)]
#[test]
fn env_names_the_next_band_the_headroom_to_it_and_the_turns_left() {
    let rig = Rig::new();
    rig.put(TP, &transcript(&climbing()));
    assert!(rig.env(Some(&rig.tp())).contains("SP_CTX_NEXT=high\nSP_CTX_HEADROOM=180000\nSP_CTX_TURNS_LEFT=36\n"));
    rig.put("projects/p/big.jsonl", &transcript(&[1_500_000, 1_600_000]));
    assert!(rig.env(Some(&rig.dir.join("projects/p/big.jsonl").display().to_string())).contains("SP_CTX_NEXT=over\nSP_CTX_HEADROOM=0\n"));
}

#[test_id(trddmck)]
#[test]
fn env_publishes_the_archivist_state_of_the_session_it_was_asked_about() {
    let rig = Rig::new();
    rig.put(TP, &transcript(&climbing())).put("run/archivist/s1.state", "state=safe\nitems_filed=3\nat_turn=10\n");
    assert!(rig.env(Some(&rig.tp())).contains("SP_CTX_ARCHIVIST=safe\nSP_CTX_ARCHIVIST_BEHIND=15\nSP_CTX_ARCHIVIST_FILED=3\n"));
}

#[test_id(t6nhvm0)]
#[test]
fn env_publishes_a_skipped_sweep_as_the_archivist_state() {
    let rig = Rig::new();
    rig.put(TP, &transcript(&climbing())).put("run/archivist/sweep.state", "sweep_state=skipped\nreason=capacity\n");
    assert!(rig.env(Some(&rig.tp())).contains("SP_CTX_ARCHIVIST=skipped\n"));
}

#[test_id(t9a8d4a)]
#[test]
fn env_age_without_a_pointer_is_the_transcripts_own() {
    let rig = Rig::new();
    rig.put(TP, &transcript(&climbing())).touch(TP, NOW - 90);
    assert!(rig.env(Some(&rig.tp())).contains("SP_CTX_AGE=90\n"));
}

// ---- the two limit windows ---------------------------------------------------------------

fn limited(five: Option<(f64, i64)>, seven: Option<(f64, i64)>, prior: &[(i64, &str, &str)]) -> String {
    let rig = Rig::new();
    if !prior.is_empty() {
        rig.put("run/limits.samples", &samples(prior));
    }
    rig.line(&rig.hook(Some(5000), &rate_limits(five, seven))).strip_prefix("\x1b[32m▁ ctx 5k\x1b[0m \x1b[2m\x1b[0m").unwrap().strip_suffix(INFLIGHT_BAD).unwrap().to_string()
}

// covers: UC-cockpit-observability-37
#[test_id(t4xi5yb)]
#[test]
fn each_window_shows_when_it_clears_then_how_full_in_a_fixed_order() {
    let at = limited(Some((42.0, NOW + 9000)), Some((10.0, NOW + 500_000)), &[]);
    assert_eq!(at, " \x1b[2m·\x1b[0m \x1b[2m2h30m\x1b[0m \x1b[32m42%\x1b[0m \x1b[2m·\x1b[0m \x1b[2m5d18h\x1b[0m \x1b[32m10%\x1b[0m");
    assert_eq!(limited(Some((42.5, NOW + 9000)), Some((0.5, NOW + 500_000)), &[]), " \x1b[2m·\x1b[0m \x1b[2m2h30m\x1b[0m \x1b[32m42.5%\x1b[0m \x1b[2m·\x1b[0m \x1b[2m5d18h\x1b[0m \x1b[32m0.5%\x1b[0m");
}

// covers: UC-cockpit-observability-37
#[test_id(t2i27yq)]
#[test]
fn an_absent_or_malformed_window_renders_nothing_never_zero() {
    assert_eq!(limited(None, Some((80.0, NOW + 5000)), &[]), " \x1b[2m·\x1b[0m \x1b[2m1h23m\x1b[0m \x1b[33m80%\x1b[0m");
    assert_eq!(limited(Some((101.0, NOW + 1)), Some((-1.0, NOW + 1)), &[]), "");
    let rig = Rig::new();
    let bool_window = r#"{"session_id":"s1","rate_limits":{"five_hour":{"used_percentage":true,"resets_at":1}}}"#;
    assert_eq!(rig.line(bool_window), format!("\x1b[2m· ctx fresh\x1b[0m{INFLIGHT_BAD}"));
    assert!(rig.read("run/limits.samples").is_empty());
}

// covers: UC-cockpit-observability-37
#[test_id(twik5bn)]
#[test]
fn a_full_window_projects_nothing_and_a_passed_reset_reads_zero_minutes() {
    assert_eq!(limited(Some((100.0, NOW + 600)), Some((95.0, NOW + 90_000)), &[]), " \x1b[2m·\x1b[0m \x1b[2m10m\x1b[0m \x1b[31m100%\x1b[0m \x1b[2m·\x1b[0m \x1b[2m1d1h\x1b[0m \x1b[31m95%\x1b[0m");
    assert_eq!(limited(Some((50.0, NOW - 100)), None, &[]), " \x1b[2m·\x1b[0m \x1b[2m0m\x1b[0m \x1b[32m50%\x1b[0m");
}

// covers: UC-cockpit-observability-37
#[test_id(t7oh0jm)]
#[test]
fn burn_rate_is_measured_over_the_span_and_projects_only_if_the_window_fills_before_it_resets() {
    let reset = NOW + 4 * 3600;
    let climbing = format!("20 {reset}");
    let rows = [(NOW - 3600, climbing.as_str(), "- -"), (NOW - 1800, &format!("30 {reset}"), "- -")];
    assert_eq!(limited(Some((40.0, reset)), None, &rows), " \x1b[2m·\x1b[0m \x1b[2m4h\x1b[0m \x1b[32m40%\x1b[0m \x1b[2m↑20.0%/h\x1b[0m \x1b[2mfull in ~3h\x1b[0m");
    let soon = NOW + 600;
    assert_eq!(limited(Some((30.0, soon)), None, &[(NOW - 3600, &format!("20 {soon}"), "- -"), (NOW - 1800, &format!("25 {soon}"), "- -")]), " \x1b[2m·\x1b[0m \x1b[2m10m\x1b[0m \x1b[32m30%\x1b[0m");
}

// covers: UC-cockpit-observability-37
#[test_id(t11m0v5)]
#[test]
fn a_projection_under_an_hour_is_red_and_one_over_a_week_keeps_its_hours() {
    assert_eq!(limited(Some((85.0, NOW + 3 * 3600)), None, &[(NOW - 1200, "60 1", "- -")]), " \x1b[2m·\x1b[0m \x1b[2m3h\x1b[0m \x1b[33m85%\x1b[0m \x1b[2m↑75.0%/h\x1b[0m \x1b[31mfull in ~12m\x1b[0m");
    let got = limited(Some((30.0, NOW + 3 * 3600)), Some((60.0, NOW + 6 * 86400 + 21 * 3600)), &[(NOW - 20000, "10 1", "40 2")]);
    assert_eq!(got, " \x1b[2m·\x1b[0m \x1b[2m3h\x1b[0m \x1b[32m30%\x1b[0m \x1b[2m·\x1b[0m \x1b[2m6d21h\x1b[0m \x1b[32m60%\x1b[0m \x1b[2m↑3.6%/h\x1b[0m \x1b[2mfull in ~11h7m\x1b[0m");
}

// covers: UC-cockpit-observability-37
#[test_id(tecif9n)]
#[test]
fn flat_single_sample_short_and_one_point_histories_project_nothing() {
    let plain = " \x1b[2m·\x1b[0m \x1b[2m2h30m\x1b[0m \x1b[32m40%\x1b[0m";
    let reset = NOW + 9000;
    for prior in [
        vec![(NOW - 3000, "40 1", "- -"), (NOW - 1000, "40 1", "- -")],
        vec![(NOW - 300, "20 1", "- -")],
        vec![(NOW - 3000, "39 1", "- -")],
    ] {
        assert_eq!(limited(Some((40.0, reset)), None, &prior), plain, "{prior:?}");
    }
    assert_eq!(limited(Some((41.0, reset)), None, &[(NOW - 3000, "40 1", "- -")]), " \x1b[2m·\x1b[0m \x1b[2m2h30m\x1b[0m \x1b[32m41%\x1b[0m");
}

// covers: UC-cockpit-observability-37
#[test_id(tmpd54n)]
#[test]
fn a_reset_is_a_boundary_the_rate_is_not_measured_across() {
    assert_eq!(limited(Some((3.0, NOW + 9000)), None, &[(NOW - 3000, "90 1", "- -"), (NOW - 1000, "2 1", "- -")]), " \x1b[2m·\x1b[0m \x1b[2m2h30m\x1b[0m \x1b[32m3%\x1b[0m");
}

// covers: UC-cockpit-observability-37
#[test_id(th8c0tg)]
#[test]
fn the_sample_file_is_pruned_to_six_hours_and_a_run_of_equal_readings_keeps_its_endpoints() {
    let rig = Rig::new();
    let reset = NOW + 9000;
    let r = format!("40 {reset}");
    rig.put("run/limits.samples", &samples(&[(NOW - 30_000, "1 1", "- -"), (NOW - 21_601, "3 1", "- -"), (NOW - 21_600, "2 1", "- -"), (NOW - 100, "39 1", "- -")]));
    rig.line(&rig.hook(Some(5000), &rate_limits(Some((40.0, reset)), None)));
    assert_eq!(rig.read("run/limits.samples"), format!("{} 2 1 - -\n{} 39 1 - -\n{NOW} {r} - -\n", NOW - 21_600, NOW - 100));
    let rig = Rig::new();
    rig.put("run/limits.samples", &samples(&[(NOW - 300, &r, "- -"), (NOW - 200, &r, "- -")]));
    rig.line(&rig.hook(Some(5000), &rate_limits(Some((40.0, reset)), None)));
    assert_eq!(rig.read("run/limits.samples"), format!("{} {r} - -\n{NOW} {r} - -\n", NOW - 300));
}

// covers: UC-cockpit-observability-37
#[test_id(thzmwx6)]
#[test]
fn a_corrupt_sample_file_is_replaced_by_the_hook_in_hand() {
    let rig = Rig::new();
    rig.put("run/limits.samples", "garbage\n");
    assert_eq!(rig.env(None).lines().last(), Some("SP_LIMIT_AGE=?"));
    rig.line(&rig.hook(Some(5000), &rate_limits(Some((40.0, NOW + 9000)), None)));
    assert_eq!(rig.read("run/limits.samples"), format!("{NOW} 40 {} - -\n", NOW + 9000));
    assert_eq!(rig.env(None).lines().last(), Some("SP_LIMIT_AGE=0"));
}

fn limit_lines(rows: &[(i64, &str, &str)]) -> String {
    let rig = Rig::new();
    rig.put("run/limits.samples", &samples(rows));
    rig.env(None).lines().skip_while(|l| !l.starts_with("SP_LIMIT")).collect::<Vec<_>>().join(" ")
}

// covers: UC-cockpit-observability-37
#[test_id(tinjuk7)]
#[test]
fn env_prints_the_newest_sample_with_its_eta_and_age() {
    let r = format!("30.5 {}", NOW + 4 * 3600);
    assert_eq!(limit_lines(&[(NOW - 3600, &format!("20 {}", NOW + 4 * 3600), "5 99"), (NOW - 100, &r, "- -")]), "SP_LIMIT_5H_PCT=30.5 SP_LIMIT_5H_ETA=23166 SP_LIMIT_7D_PCT=- SP_LIMIT_7D_ETA=- SP_LIMIT_AGE=100");
    assert_eq!(limit_lines(&[(NOW - 3600, "20 5", "10 5"), (NOW - 10, "30 5", "12 5")]), "SP_LIMIT_5H_PCT=30 SP_LIMIT_5H_ETA=25130 SP_LIMIT_7D_PCT=12 SP_LIMIT_7D_ETA=157960 SP_LIMIT_AGE=10");
}

// covers: UC-cockpit-observability-37
#[test_id(tatuq7s)]
#[test]
fn env_prints_a_full_window_as_full_now_and_dashes_or_question_marks_never_zero() {
    assert_eq!(limit_lines(&[(NOW - 3600, "100 5", "100 9")]), "SP_LIMIT_5H_PCT=100 SP_LIMIT_5H_ETA=0 SP_LIMIT_7D_PCT=100 SP_LIMIT_7D_ETA=0 SP_LIMIT_AGE=3600");
    let rig = Rig::new();
    assert!(rig.env(None).ends_with("SP_LIMIT_5H_PCT=-\nSP_LIMIT_5H_ETA=-\nSP_LIMIT_7D_PCT=-\nSP_LIMIT_7D_ETA=-\nSP_LIMIT_AGE=-"));
    rig.put("run/limits.samples", "bad line\n");
    assert!(rig.env(None).contains("SP_LIMIT_5H_PCT=?\nSP_LIMIT_5H_ETA=?\nSP_LIMIT_7D_PCT=?\nSP_LIMIT_7D_ETA=?\nSP_LIMIT_AGE=?"));
}

// covers: UC-cockpit-observability-37
#[test_id(tow05z3)]
#[test]
fn the_line_with_a_projection_on_screen_stays_within_110_columns() {
    let rig = Rig::new();
    rig.put("run/cockpit.env", "SP_INFLIGHT_SEG='3 in flight'\n").touch("run/cockpit.env", NOW);
    let (reset5, reset7) = (NOW + 86_400, NOW + 600_000);
    rig.put("run/limits.samples", &samples(&[(NOW - 600, &format!("60 {reset5}"), &format!("41 {reset7}")), (NOW - 300, &format!("62 {reset5}"), &format!("41 {reset7}"))]));
    let line = rig.line(&rig.hook(Some(5000), &rate_limits(Some((64.0, reset5)), Some((41.0, reset7)))));
    assert!(line.contains("full in"), "the projection is on the line being measured: {line}");
    let mut plain = String::new();
    let mut esc = false;
    for c in line.chars() {
        match (esc, c) {
            (_, '\x1b') => esc = true,
            (true, 'm') => esc = false,
            (false, c) => plain.push(c),
            _ => {}
        }
    }
    assert!(plain.chars().count() <= 110, "{} columns: {plain}", plain.chars().count());
}

// ---- formats -----------------------------------------------------------------------------

#[test_id(tkbiq43)]
#[test]
fn percent_g_drops_trailing_zeros_and_keeps_six_significant_digits() {
    for (x, want) in [(0.0, "0"), (42.0, "42"), (42.5, "42.5"), (0.5, "0.5"), (100.0, "100"), (33.333333, "33.3333"), (0.00001, "1e-05"), (1234567.0, "1.23457e+06")] {
        assert_eq!(fmt::g(x), want);
    }
}

#[test_id(t6hzk5u)]
#[test]
fn durations_use_the_coarsest_unit_that_still_says_something() {
    for (m, want) in [(-5.0, "0m"), (47.6, "48m"), (0.5, "0m"), (1.5, "2m"), (60.0, "1h"), (80.0, "1h20m"), (1440.0, "1d"), (1500.0, "1d1h")] {
        assert_eq!(fmt::dur(m), want, "{m}");
    }
}

#[test_id(tp6swsc)]
#[test]
fn a_missing_key_is_refused_by_name() {
    let get = |k: &str| if k == "SPIRA_ARCHIVIST_IDLE" { Err(format!("{k} is not a registered config key")) } else { Ok("1".to_string()) };
    let err = Config::from_lookup(&get).err().unwrap();
    assert!(err.contains("SPIRA_ARCHIVIST_IDLE"), "{err}");
    let bad = |k: &str| Ok(if k == "SPIRA_CTX_HIGH" { "lots".to_string() } else { "1".to_string() });
    assert!(Config::from_lookup(&bad).err().unwrap().contains("SPIRA_CTX_HIGH"));
}
