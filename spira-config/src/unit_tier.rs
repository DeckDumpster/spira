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

/// The doc-tests, under plain `cargo test`.
#[macro_export]
macro_rules! unit_tier_doc {
    ($profile:literal, $sel:literal) => {
        concat!(
            "test --no-fail-fast --profile ",
            $profile,
            " --features integration --config profile.",
            $profile,
            ".incremental=false ",
            $sel,
            " --doc"
        )
    };
}

/// The nextest config as a `printf` format taking the period's seconds and the periods.
#[macro_export]
macro_rules! unit_tier_config_printf {
    () => {
        "[profile.default]\\nslow-timeout = { period = \"%ss\", terminate-after = %s }\\n"
    };
}

fn profile_words(profile: Option<&str>, flag: &str) -> String {
    match profile {
        Some(p) => {
            format!("{flag} {p} --features integration --config profile.{p}.incremental=false")
        }
        None => "--features integration".into(),
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

/// [`unit_tier_doc!`].
pub fn doc_words(profile: Option<&str>, selection: &str) -> String {
    format!(
        "test --no-fail-fast {} {selection} --doc",
        profile_words(profile, "--profile")
    )
}

/// The nextest config of one deadline: `secs` per period, terminated after `periods`.
pub fn nextest_config(secs: u64, periods: u64) -> String {
    format!("[profile.default]\nslow-timeout = {{ period = \"{secs}s\", terminate-after = {periods} }}\n")
}

/// What one unit tier's nextest log says of its timings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UnitTimings {
    /// The `Summary [` line's test count; the per-test lines seen when the run left no summary.
    pub tests: u64,
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

/// The one reader of nextest's output for timings: lines up to the first `Summary [` count, so
/// the reds the end of the log repeats are not counted twice.
pub fn timings(log: &str) -> UnitTimings {
    let mut seen = Vec::new();
    let mut tests = None;
    for line in log.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("Summary [") {
            tests = rest.split_once(']').and_then(|(_, r)| r.split_whitespace().next()?.parse().ok());
            break;
        }
        seen.extend(per_test(line));
    }
    let tests = tests.unwrap_or(seen.len() as u64);
    seen.sort_by(|a, b| b.1.total_cmp(&a.1));
    seen.truncate(SLOWEST);
    UnitTimings { tests, slowest: seen }
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

/// The unit tier's file form: the VM's seconds, then the log lines [`unit_tier_timing_lines!`] matches.
#[derive(Clone, Debug, PartialEq)]
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
        assert_eq!(
            unit_tier_doc!("u", "--workspace"),
            doc_words(Some("u"), "--workspace")
        );
    }

    #[test]
    fn the_printf_format_prints_the_config_text() {
        let fmt = unit_tier_config_printf!()
            .replace("\\n", "\n")
            .replacen("%s", "7", 1)
            .replacen("%s", "3", 1);
        assert_eq!(fmt, nextest_config(7, 3));
    }

    #[test]
    fn the_default_profile_names_no_profile_and_no_override() {
        for w in [
            build_words(None, "-p a"),
            unit_words("C", None, "-p a", LIB_BINS),
            integration_words(None, "-p a"),
            doc_words(None, "-p a"),
        ] {
            assert!(!w.contains("profile") && !w.contains("incremental"), "{w}");
            assert!(w.contains("--features integration -p a"), "{w}");
        }
    }

    #[test]
    fn no_word_names_a_thread_count() {
        for w in [
            build_words(None, "-p a"),
            unit_words("C", None, "-p a", LIB_BINS),
            integration_words(None, "-p a"),
            doc_words(None, "-p a"),
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

    #[test]
    fn timings_read_the_count_from_the_summary_and_keep_the_ten_slowest_once() {
        let t = timings(&nextest_log());
        assert_eq!(t.tests, 12);
        let names: Vec<&str> = t.slowest.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["b t5", "b t4", "b t11", "b t10", "b t9", "a t2", "b t8", "b t7", "a t3", "b t6"]);
        assert_eq!(t.slowest[0].1, 60.0);
    }

    #[test]
    fn timings_of_a_log_with_no_tests_are_empty() {
        assert_eq!(timings("error: could not compile\n"), UnitTimings::default());
    }

    #[test]
    fn the_file_form_round_trips_and_leaves_the_parse_to_timings() {
        let lines: String = nextest_log().lines().filter(|l| l.contains("] (") || l.contains("Summary [")).map(|l| format!("{l}\n")).collect();
        let text = render(12, 61, &lines);
        let f = parse(&text).unwrap();
        assert_eq!((f.build_secs, f.run_secs), (12, 61));
        assert_eq!(f.timings, timings(&nextest_log()));
        assert_eq!(parse("garbage"), None);
    }

    #[test]
    fn the_slowest_field_is_name_colon_secs_joined_by_commas() {
        let t = UnitTimings { tests: 2, slowest: vec![("a t1".into(), 1.5), ("b t2".into(), 0.25)] };
        assert_eq!(t.slowest_field(), "a/t1:1.5,b/t2:0.25");
        assert_eq!(UnitTimings::default().slowest_field(), "-");
    }
}
