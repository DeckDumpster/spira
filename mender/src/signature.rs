//! Environment signatures: output lines that say the box failed, not the members' code. The
//! pack reports them as evidence; the mender may disagree.

pub struct Signature {
    pub name: &'static str,
    pub needles: &'static [&'static str],
}

pub const SIGNATURES: &[Signature] = &[
    Signature {
        name: "EAGAIN",
        needles: &["Resource temporarily unavailable", "EAGAIN", "fork: retry", "Cannot fork", "cannot allocate memory"],
    },
    Signature {
        name: "fixture-server-never-started",
        needles: &["server did not start", "server never started", "fixture server", "dolt sql-server: not ready", "timed out waiting for the server"],
    },
    Signature {
        name: "no-space",
        needles: &["No space left on device"],
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub name: &'static str,
    pub line: usize,
    pub text: String,
}

pub fn scan(output: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    for (i, text) in output.lines().enumerate() {
        let lower = text.to_ascii_lowercase();
        for s in SIGNATURES {
            if s.needles.iter().any(|n| lower.contains(&n.to_ascii_lowercase())) {
                hits.push(Hit { name: s.name, line: i + 1, text: text.trim().chars().take(200).collect() });
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_signature_is_found_by_its_own_needles() {
        for s in SIGNATURES {
            for n in s.needles {
                let hits = scan(&format!("ok\nsomething: {n} (os error)\nok"));
                assert_eq!(hits.iter().map(|h| (h.name, h.line)).collect::<Vec<_>>(), vec![(s.name, 2)], "{n}");
            }
        }
    }

    #[test]
    fn a_clean_output_has_no_hit() {
        assert!(scan("ok 1 - a\nnot ok 2 - expected 3, got 4\n").is_empty());
    }

    #[test]
    fn matching_ignores_case_and_a_long_line_is_cut() {
        let long = format!("resource TEMPORARILY unavailable {}", "x".repeat(500));
        let hits = scan(&long);
        assert_eq!(hits[0].name, "EAGAIN");
        assert_eq!(hits[0].text.chars().count(), 200);
    }
}
