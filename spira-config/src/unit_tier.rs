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
}
