//! `testenv preflight`: every dependency of the suites about to run, verified before anything
//! is built or run. A suite declares what the host must supply on `# host-requires:`, as
//! commands or `py:<module>`; a gap is a real failure (exit 1), never the retry code.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::suite::SuiteHeaders;

pub const USAGE: &str = "usage: testenv preflight --suites <a.sh,b.sh,...> [--dir <suite-dir>]";
const RC_MISSING: i32 = 1;
const RC_USAGE: i32 = 2;

/// Token -> the suites that need it, for every host requirement of the named suites.
pub fn wanted(suites: &[(String, SuiteHeaders)]) -> BTreeMap<String, Vec<String>> {
    let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, h) in suites {
        for t in &h.host_requires {
            m.entry(t.clone()).or_default().push(name.clone());
        }
    }
    m
}

/// Every unmet token, each with the suites that need it; `have` answers one token.
pub fn missing(
    suites: &[(String, SuiteHeaders)],
    have: &dyn Fn(&str) -> bool,
) -> Vec<(String, Vec<String>)> {
    wanted(suites).into_iter().filter(|(t, _)| !have(t)).collect()
}

pub fn host_has(token: &str) -> bool {
    let ok = |prog: &str, args: &[&str]| {
        Command::new(prog)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    match token.strip_prefix("py:") {
        Some(module) => {
            !module.is_empty()
                && module.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
                && ok("python3", &["-I", "-c", &format!("import {module}")])
        }
        None => ok("bash", &["-c", "command -v \"$1\" >/dev/null 2>&1", "_", token]),
    }
}

pub fn report(gaps: &[(String, Vec<String>)]) -> String {
    gaps.iter()
        .map(|(t, by)| format!("preflight: MISSING {t} (needed by {})", by.join(", ")))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn main(args: &[String]) -> i32 {
    let (mut list, mut dir) = (None, String::from("spira"));
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match (a.as_str(), it.next()) {
            ("--suites", Some(v)) => list = Some(v.clone()),
            ("--dir", Some(v)) => dir = v.clone(),
            _ => {
                eprintln!("{USAGE}");
                return RC_USAGE;
            }
        }
    }
    let Some(list) = list else {
        eprintln!("{USAGE}");
        return RC_USAGE;
    };
    let mut suites = Vec::new();
    let mut unreadable = false;
    for n in list.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        match std::fs::read_to_string(Path::new(&dir).join(n)) {
            Ok(t) => suites.push((n.to_string(), SuiteHeaders::parse(&t))),
            Err(e) => {
                eprintln!("preflight: MISSING suite {n}: {e}");
                unreadable = true;
            }
        }
    }
    let gaps = missing(&suites, &host_has);
    if !gaps.is_empty() {
        eprintln!("{}", report(&gaps));
    }
    if gaps.is_empty() && !unreadable {
        eprintln!("preflight: ok ({} suite(s), {} host requirement(s))", suites.len(), wanted(&suites).len());
        0
    } else {
        eprintln!("preflight: the selected suites cannot run on this machine; the gaps above are host configuration, not a fault in the branch");
        RC_MISSING
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(name: &str, text: &str) -> (String, SuiteHeaders) {
        (name.into(), SuiteHeaders::parse(text))
    }

    #[test]
    fn a_planted_gap_is_named_with_its_suites_and_a_present_one_is_not() {
        let suites = [
            s("test-a.sh", "# host-requires: jq py:nopkgx\nset -u\n"),
            s("test-b.sh", "# host-requires: py:nopkgx\nset -u\n"),
        ];
        let gaps = missing(&suites, &|t| t == "jq");
        assert_eq!(gaps, vec![("py:nopkgx".to_string(), vec!["test-a.sh".to_string(), "test-b.sh".to_string()])]);
        assert!(report(&gaps).contains("MISSING py:nopkgx (needed by test-a.sh, test-b.sh)"));
        assert!(missing(&suites, &|_| true).is_empty());
    }

    #[test]
    fn the_host_probe_sees_a_real_command_and_a_real_absence() {
        assert!(host_has("sh"));
        assert!(!host_has("no-such-command-sp5ieoqt"));
        assert!(!host_has("py:no_such_module_sp5ieoqt"));
        assert!(!host_has("py:os; import sys"));
    }

    #[test]
    fn main_exits_1_naming_the_package() {
        let t = testkit::TempDir::new("preflight");
        let d = t.path().to_path_buf();
        std::fs::write(d.join("test-x.sh"), "# host-requires: py:no_such_module_sp5ieoqt\nset -u\n").unwrap();
        let a = |l: &str| vec!["--suites".into(), l.into(), "--dir".into(), d.to_string_lossy().into_owned()];
        assert_eq!(main(&a("test-x.sh")), 1);
        std::fs::write(d.join("test-y.sh"), "# host-requires: sh\nset -u\n").unwrap();
        assert_eq!(main(&a("test-y.sh")), 0);
        assert_eq!(main(&a("test-gone.sh")), 1);
    }
}
