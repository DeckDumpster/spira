//! The unit and integration tiers' cargo invocations, defined once: the round runs them with
//! `--workspace` under its `unittest` profile, the gate with `-p <crate>…` under `dev`.
//!
//! Each tier is a literal-producing macro (the round's remote script is a `concat!`) and a
//! `fn` giving the same words for a caller whose profile is optional; a test holds the two equal.

/// The unit tier's filter: the targets whose tests share nothing with another binary.
pub const LIB_BINS: &str = "--lib --bins";

/// `test --no-run`: compile every test harness the tiers run, once.
#[macro_export]
macro_rules! unit_tier_build {
    ($profile:literal, $sel:literal) => {
        concat!(
            "test --no-run --profile ",
            $profile,
            " --features integration --config profile.",
            $profile,
            ".incremental=false ",
            $sel
        )
    };
}

/// One process per test, each under the deadline in `$config`.
#[macro_export]
macro_rules! unit_tier_unit {
    ($config:literal, $profile:literal, $sel:literal) => {
        concat!(
            "nextest run --config-file ",
            $config,
            " --no-fail-fast --cargo-profile ",
            $profile,
            " --features integration --config profile.",
            $profile,
            ".incremental=false ",
            $sel,
            " --lib --bins"
        )
    };
}

/// The `tests/` targets, under plain `cargo test`.
#[macro_export]
macro_rules! unit_tier_integration {
    ($profile:literal, $sel:literal) => {
        concat!(
            "test --no-fail-fast --profile ",
            $profile,
            " --features integration --config profile.",
            $profile,
            ".incremental=false ",
            $sel,
            " --test '*'"
        )
    };
}

/// The nextest config as a `printf` format taking the period's seconds and the periods.
#[macro_export]
macro_rules! unit_tier_config_printf {
    () => {
        "[profile.default]\\nslow-timeout = { period = \"%ss\", terminate-after = %s }\\nstatus-level = \"skip\"\\n"
    };
}

fn profile_words(profile: Option<&str>, flag: &str) -> String {
    match profile {
        Some(p) => {
            format!("{flag} {p} --features integration --config profile.{p}.incremental=false")
        }
        None => format!("--features integration {}", crate::build::one_shot_words("dev")),
    }
}

/// [`unit_tier_build!`], with `profile` of `None` for the default profile.
pub fn build_words(profile: Option<&str>, selection: &str) -> String {
    format!(
        "test --no-run {} {selection}",
        profile_words(profile, "--profile")
    )
}

/// [`unit_tier_unit!`], its filter `filter` (a crate with no lib target takes `--bins` alone).
pub fn unit_words(config: &str, profile: Option<&str>, selection: &str, filter: &str) -> String {
    format!(
        "nextest run --config-file {config} --no-fail-fast {} {selection} {filter}",
        profile_words(profile, "--cargo-profile")
    )
}

/// [`unit_tier_integration!`].
pub fn integration_words(profile: Option<&str>, selection: &str) -> String {
    format!(
        "test --no-fail-fast {} {selection} --test '*'",
        profile_words(profile, "--profile")
    )
}

/// The words that rerun one cargo-named failing target (`rerun` is cargo's own `-p <package> <target>`).
pub fn rerun_words(profile: Option<&str>, rerun: &str) -> String {
    format!("test --no-fail-fast {} {rerun}", profile_words(profile, "--profile"))
}

/// The environment variable naming the file of a run's deny list, read by testenv.
pub const DENY_LIST_ENV: &str = "SPIRA_TEST_DENY_LIST";

/// The tests a caller is handed to leave out of the unit and integration tiers, as
/// `test-registry deny-list` prints them: `<state>\t<test>\t<bead>\t<until>` per row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DenyList {
    rows: Vec<(String, String, String)>,
}

impl DenyList {
    pub fn parse(text: &str) -> Result<DenyList, String> {
        let mut rows = Vec::new();
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let n = i + 1;
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() != 4 {
                return Err(format!("deny list line {n}: want 4 tab-separated fields, got {}", f.len()));
            }
            if f[0] != "disabled" && f[0] != "quarantined" {
                return Err(format!("deny list line {n}: state {:?} is neither disabled nor quarantined", f[0]));
            }
            let word = |w: &str| !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || "_:.-/".contains(c));
            let well_formed = match f[1].split_once(':') {
                Some(("suite", name)) => word(name),
                Some(("test", id)) => {
                    id.len() == 7 && id.starts_with('t') && id[1..].bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
                }
                _ => false,
            };
            if !well_formed {
                return Err(format!("deny list line {n}: test {:?} is not suite:<file> or test:<id>", f[1]));
            }
            rows.push((f[0].to_string(), f[1].to_string(), f[2].to_string()));
        }
        Ok(DenyList { rows })
    }

    /// The text [`Self::parse`] reads back: one row per denied test, state and test only.
    pub fn render(&self) -> String {
        self.rows.iter().map(|(s, t, _)| format!("{s}\t{t}\t-\t-\n")).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Each `test:` row's state, id and owning bead.
    pub fn test_rows(&self) -> Vec<(&str, &str, &str)> {
        self.rows.iter().filter_map(|(s, t, b)| Some((s.as_str(), t.strip_prefix("test:")?, b.as_str()))).collect()
    }

    /// Each `suite:` row's file and state.
    pub fn suites(&self) -> Vec<(&str, &str)> {
        self.rows.iter().filter_map(|(s, t, _)| Some((t.strip_prefix("suite:")?, s.as_str()))).collect()
    }

    pub fn nextest_filter(&self) -> Option<String> {
        let terms: Vec<String> = self.test_rows().into_iter().map(|(_, id, _)| format!("test(/::{id}$/)")).collect();
        (!terms.is_empty()).then(|| format!("not ({})", terms.join(" | ")))
    }

    /// ` -- --skip ::<id> …` for the `--test '*'` invocation; empty for no `test:` row.
    pub fn integration_skips(&self) -> String {
        let skips: Vec<String> = self.test_rows().into_iter().map(|(_, id, _)| format!("--skip ::{id}")).collect();
        if skips.is_empty() {
            return String::new();
        }
        format!(" -- {}", skips.join(" "))
    }

    /// The config line that applies [`Self::nextest_filter`]; empty for none.
    pub fn config_filter_line(&self) -> String {
        match self.nextest_filter() {
            Some(f) => format!("default-filter = \"{}\"\n", f.replace('\\', "\\\\").replace('"', "\\\"")),
            None => String::new(),
        }
    }
}

/// The nextest config of one deadline: `secs` per period, terminated after `periods`.
pub fn nextest_config(secs: u64, periods: u64, deny: &DenyList) -> String {
    format!(
        "[profile.default]\nslow-timeout = {{ period = \"{secs}s\", terminate-after = {periods} }}\nstatus-level = \"skip\"\n{}",
        deny.config_filter_line()
    )
}

/// What one unit tier's nextest log says of its timings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UnitTimings {
    /// The `Summary [` line's test count; the per-test lines seen when the run left no summary.
    pub tests: u64,
    /// Whether the log carried a `Summary [` line: nextest's own report that it ran.
    pub summarized: bool,
    /// The seconds in the `Summary [ <secs>s]` line; 0 when unsummarized.
    pub run_secs: f64,
    /// `<binary> <test>` and seconds, the ten largest, ties in log order.
    pub slowest: Vec<(String, f64)>,
}

/// How many tests `slowest` keeps.
pub const SLOWEST: usize = 10;

/// grep -E pattern for the lines [`timings`] reads: nextest's per-test lines and its summary.
#[macro_export]
macro_rules! unit_tier_timing_lines {
    () => {
        r"^ *([A-Z][A-Z-]+ +\[[ >]*[0-9.]+s\] +\( *[0-9]+/[0-9]+\) |Summary \[)"
    };
}

fn per_test(line: &str) -> Option<(String, f64)> {
    let t = line.trim_start();
    let (status, rest) = t.split_once(' ')?;
    if !status.chars().all(|c| c.is_ascii_uppercase() || c == '-') || status.is_empty() {
        return None;
    }
    let rest = rest.trim_start().strip_prefix('[')?;
    let (secs, rest) = rest.split_once("s]")?;
    let secs = secs.trim().parse::<f64>().ok()?;
    let rest = rest.trim_start().strip_prefix('(')?;
    let (_, name) = rest.split_once(')')?;
    let name = name.trim();
    (!name.is_empty()).then(|| (name.to_string(), secs))
}

/// The test name of a nextest red status line: `FAIL|TIMEOUT|SIG*|LEAK-FAIL|EXEC-FAIL [ <secs>]
/// (<k>/<n>) <binary> <name>`.
pub fn nextest_red(l: &str) -> Option<&str> {
    let (status, rest) = l.split_once(' ')?;
    if !matches!(status, "FAIL" | "TIMEOUT" | "LEAK-FAIL" | "EXEC-FAIL") && !status.starts_with("SIG") {
        return None;
    }
    let after = rest.trim_start().strip_prefix('[')?.split_once(']')?.1.trim_start();
    let after = match after.strip_prefix('(') {
        Some(counter) => counter.split_once(')')?.1,
        None => after,
    };
    let mut words = after.split_whitespace();
    let (_binary, name) = (words.next()?, words.next()?);
    words.next().is_none().then_some(name)
}

/// Whether `l` is one of nextest's per-test status lines, of any status.
pub fn nextest_status(l: &str) -> bool {
    let Some((status, rest)) = l.trim_start().split_once(' ') else { return false };
    !status.is_empty() && status.chars().all(|c| c.is_ascii_uppercase() || c == '-') && rest.trim_start().starts_with('[') && rest.contains(']')
}

/// The one reader of nextest's output for timings: lines up to the first `Summary [` count, so
/// the reds the end of the log repeats are not counted twice.
pub fn timings(log: &str) -> UnitTimings {
    let mut seen = Vec::new();
    let mut tests = None;
    let mut summarized = false;
    let mut run_secs = 0.0;
    for line in log.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("Summary [") {
            summarized = true;
            run_secs = rest.split_once("s]").and_then(|(s, _)| s.trim().parse().ok()).unwrap_or(0.0);
            tests = rest.split_once(']').and_then(|(_, r)| r.split_whitespace().next()?.parse().ok());
            break;
        }
        seen.extend(per_test(line));
    }
    let tests = tests.unwrap_or(seen.len() as u64);
    seen.sort_by(|a, b| b.1.total_cmp(&a.1));
    seen.truncate(SLOWEST);
    UnitTimings { tests, summarized, run_secs, slowest: seen }
}

impl UnitTimings {
    /// `<binary>/<test>:<secs>,…`, slowest first, one token so a log line stays splittable; `-` when empty.
    pub fn slowest_field(&self) -> String {
        if self.slowest.is_empty() {
            return "-".into();
        }
        let f: Vec<String> = self.slowest.iter().map(|(n, s)| format!("{}:{s}", n.replace(' ', "/"))).collect();
        f.join(",")
    }
}

impl UnitTimings {
    /// The record field: [`Self::slowest_field`], or empty when no test was timed.
    pub fn slowest_record(&self) -> String {
        if self.slowest.is_empty() { String::new() } else { self.slowest_field() }
    }
}

/// The unit tier's file form: the VM's seconds, then the log lines [`unit_tier_timing_lines!`] matches.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UnitFile {
    pub build_secs: u64,
    pub run_secs: u64,
    pub timings: UnitTimings,
}

pub fn render(build_secs: u64, run_secs: u64, lines: &str) -> String {
    format!("unit_build_secs {build_secs}\nunit_secs {run_secs}\n{lines}")
}

pub fn parse(text: &str) -> Option<UnitFile> {
    let mut it = text.splitn(3, '\n');
    let build_secs = it.next()?.strip_prefix("unit_build_secs ")?.trim().parse().ok()?;
    let run_secs = it.next()?.strip_prefix("unit_secs ")?.trim().parse().ok()?;
    Some(UnitFile { build_secs, run_secs, timings: timings(it.next().unwrap_or("")) })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_ROW: &str = "disabled\ttest:t0aaaaa\tsp-x\t-\n";

    #[test_id(tq0hr1m)]
    #[test]
    fn a_test_id_row_parses_and_test_rows_returns_its_bead() {
        let d = DenyList::parse(TEST_ROW).unwrap();
        assert_eq!(d.test_rows(), vec![("disabled", "t0aaaaa", "sp-x")]);
    }

    #[test_id(ty6n2di)]
    #[test]
    fn a_malformed_test_id_row_is_refused_by_line_number() {
        let e = DenyList::parse(&format!("{TEST_ROW}quarantined\ttest:T0\tsp-x\t-\n")).unwrap_err();
        assert!(e.starts_with("deny list line 2:"), "{e}");
    }

    #[test_id(tv80b70)]
    #[test]
    fn a_test_id_row_reaches_the_filter_and_the_skip_and_a_unit_row_is_refused() {
        let d = DenyList::parse(TEST_ROW).unwrap();
        assert_eq!(d.nextest_filter().unwrap(), "not (test(/::t0aaaaa$/))");
        assert_eq!(d.integration_skips(), " -- --skip ::t0aaaaa");
        let e = DenyList::parse(&format!("{TEST_ROW}disabled\tunit:a b::c\tsp-1\t-\n")).unwrap_err();
        assert!(e.starts_with("deny list line 2:"), "{e}");
    }

    #[test_id(t2te7i8)]
    #[test]
    fn each_macro_gives_the_words_of_its_fn() {
        assert_eq!(
            unit_tier_build!("u", "--workspace"),
            build_words(Some("u"), "--workspace")
        );
        assert_eq!(
            unit_tier_unit!("C", "u", "--workspace"),
            unit_words("C", Some("u"), "--workspace", LIB_BINS)
        );
        assert_eq!(
            unit_tier_integration!("u", "--workspace"),
            integration_words(Some("u"), "--workspace")
        );
    }

    #[test_id(ts5rb7x)]
    #[test]
    fn the_printf_format_prints_the_config_text() {
        let fmt = unit_tier_config_printf!()
            .replace("\\n", "\n")
            .replacen("%s", "7", 1)
            .replacen("%s", "3", 1);
        assert_eq!(fmt, nextest_config(7, 3, &DenyList::default()));
    }

    const TWO: &str = "# test-registry deny-list at=1 rows=2\ndisabled\ttest:t0aaaaa\tsp-1\t-\nquarantined\ttest:t0bbbbb\tsp-2\t5\n";

    #[test_id(ty3yp8p)]
    #[test]
    fn an_empty_deny_list_composes_no_filter_no_skips_and_todays_config() {
        let d = DenyList::default();
        assert_eq!(d.nextest_filter(), None);
        assert_eq!(d.integration_skips(), "");
        assert_eq!(nextest_config(7, 3, &d), "[profile.default]\nslow-timeout = { period = \"7s\", terminate-after = 3 }\nstatus-level = \"skip\"\n");
        assert_eq!(DenyList::parse("# test-registry deny-list at=1 rows=0\n").unwrap(), d);
    }

    #[test_id(tzb1b4a)]
    #[test]
    fn two_test_rows_of_either_state_give_the_exact_filter_expression() {
        let d = DenyList::parse(TWO).unwrap();
        assert_eq!(d.nextest_filter().unwrap(), "not (test(/::t0aaaaa$/) | test(/::t0bbbbb$/))");
        assert!(nextest_config(7, 3, &d).ends_with("default-filter = \"not (test(/::t0aaaaa$/) | test(/::t0bbbbb$/))\"\n"));
        assert_eq!(DenyList::parse(&d.render()).unwrap().render(), d.render(), "render keeps state and test, not the bead");
    }

    #[test_id(trfvnun)]
    #[test]
    fn test_rows_become_skips_and_suite_rows_reach_neither_filter() {
        let d = DenyList::parse("disabled\ttest:t0ccccc\t-\t-\ndisabled\tsuite:test-a.sh\t-\t-\n").unwrap();
        assert_eq!(d.integration_skips(), " -- --skip ::t0ccccc");
        assert_eq!(d.suites(), [("test-a.sh", "disabled")]);
    }

    #[test_id(tiptmp9)]
    #[test]
    fn a_malformed_deny_list_line_is_refused_by_its_number() {
        for (text, line) in [
            ("disabled\tsuite:a\t-\t-\nbroken\n", "line 2"),
            ("\n\nfrozen\tsuite:a\t-\t-\n", "line 3"),
            ("disabled\tunit:nopath\t-\t-\n", "line 1"),
            ("disabled\ttest:T0\t-\t-\n", "line 1"),
            ("disabled\tfoo:a b\t-\t-\n", "line 1"),
            ("disabled\tintegration:p x;touch$IFS/tmp/y\t-\t-\n", "line 1"),
            ("disabled\ttest:t0aaaaa\t-\t-\nquarantined\tunit:a t::x)|(y\t-\t-\n", "line 2"),
            ("disabled\tsuite:a$(id)\t-\t-\n", "line 1"),
        ] {
            let e = DenyList::parse(text).unwrap_err();
            assert!(e.contains(line), "{e}");
        }
    }

    #[test_id(ttooqj0)]
    #[test]
    fn the_default_profile_names_no_profile_and_is_one_shot_under_dev() {
        for w in [
            build_words(None, "-p a"),
            unit_words("C", None, "-p a", LIB_BINS),
            integration_words(None, "-p a"),
        ] {
            assert!(!w.contains("--profile") && !w.contains("--cargo-profile"), "{w}");
            assert!(w.contains("--features integration --config profile.dev.incremental=false -p a"), "{w}");
        }
    }

    #[test_id(tafjah0)]
    #[test]
    fn no_word_names_a_thread_count() {
        for w in [
            build_words(None, "-p a"),
            unit_words("C", None, "-p a", LIB_BINS),
            integration_words(None, "-p a"),
        ] {
            assert!(!w.contains("-j") && !w.contains("test-threads"), "{w}");
        }
    }

    fn nextest_log() -> String {
        let mut l = String::from("    Starting 12 tests across 2 binaries\n");
        let rows = [
            ("PASS", 0.007, "a t1"), ("PASS", 3.5, "a t2"), ("FAIL", 1.25, "a t3"),
            ("PASS", 9.0, "b t4"), ("TIMEOUT", 60.0, "b t5"), ("PASS", 0.5, "b t6"),
            ("PASS", 2.0, "b t7"), ("PASS", 3.5, "b t8"), ("PASS", 4.0, "b t9"),
            ("PASS", 5.0, "b t10"), ("PASS", 6.0, "b t11"), ("PASS", 0.001, "b t12"),
        ];
        for (i, (st, s, n)) in rows.iter().enumerate() {
            l.push_str(&format!("{st:>12} [{s:>8.3}s] ({:>2}/12) {n}\n", i + 1));
        }
        l.push_str("     SLOW [>120.000s] ( 5/12) b t5\n------------\n");
        l.push_str("     Summary [  61.000s] 12 tests run: 10 passed, 1 failed, 1 timed out, 0 skipped\n");
        l.push_str("        FAIL [   1.250s] ( 3/12) a t3\n     TIMEOUT [  60.000s] ( 5/12) b t5\n");
        l
    }

    #[test_id(tdso2vl)]
    #[test]
    fn timings_read_the_count_from_the_summary_and_keep_the_ten_slowest_once() {
        let t = timings(&nextest_log());
        assert_eq!(t.tests, 12);
        let names: Vec<&str> = t.slowest.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["b t5", "b t4", "b t11", "b t10", "b t9", "a t2", "b t8", "b t7", "a t3", "b t6"]);
        assert_eq!(t.slowest[0].1, 60.0);
    }

    #[test_id(tzxicbs)]
    #[test]
    fn timings_say_whether_nextest_printed_its_summary() {
        assert!(!timings("").summarized);
        assert!(!timings("bwrap: execvp cargo: No such file or directory\n").summarized);
        let t = timings("     Summary [   1.000s] 3 tests run: 3 passed, 0 skipped\n");
        assert_eq!(t.run_secs, 1.0);
        assert!(t.summarized);
        assert_eq!(t.tests, 3);
    }

    #[test_id(t1lgkfk)]
    #[test]
    fn timings_of_a_log_with_no_tests_are_empty() {
        assert_eq!(timings("error: could not compile\n"), UnitTimings::default());
    }

    #[test_id(t3vj6ui)]
    #[test]
    fn the_file_form_round_trips_and_leaves_the_parse_to_timings() {
        let lines: String = nextest_log().lines().filter(|l| l.contains("] (") || l.contains("Summary [")).map(|l| format!("{l}\n")).collect();
        let text = render(12, 61, &lines);
        let f = parse(&text).unwrap();
        assert_eq!((f.build_secs, f.run_secs), (12, 61));
        assert_eq!(f.timings, timings(&nextest_log()));
        assert_eq!(parse("garbage"), None);
    }

    #[test_id(tkrzhxj)]
    #[test]
    fn the_slowest_field_is_name_colon_secs_joined_by_commas() {
        let t = UnitTimings { tests: 2, summarized: true, run_secs: 0.0, slowest: vec![("a t1".into(), 1.5), ("b t2".into(), 0.25)] };
        assert_eq!(t.slowest_field(), "a/t1:1.5,b/t2:0.25");
        assert_eq!(UnitTimings::default().slowest_field(), "-");
    }
}
