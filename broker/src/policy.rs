/// Read verbs: synchronous, no rune restriction.
#[derive(Debug, Clone, PartialEq)]
pub enum ReadVerb {
    RunView,
    RunJobs,
    ArtifactDownload,
}

impl ReadVerb {
    pub fn parse(s: &str) -> Option<ReadVerb> {
        match s {
            "run-view"          => Some(ReadVerb::RunView),
            "run-jobs"          => Some(ReadVerb::RunJobs),
            "artifact-download" => Some(ReadVerb::ArtifactDownload),
            _                   => None,
        }
    }

}

/// Rune x verb for `identity-login-code`: the pilgrim and nobody else; an unknown or empty
/// rune is refused.
pub fn allows_login_code(rune: &str) -> bool {
    rune == "pilgrim"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_verb_parse_round_trips() {
        assert_eq!(ReadVerb::parse("run-view"), Some(ReadVerb::RunView));
        assert_eq!(ReadVerb::parse("run-jobs"), Some(ReadVerb::RunJobs));
        assert_eq!(ReadVerb::parse("artifact-download"), Some(ReadVerb::ArtifactDownload));
        assert_eq!(ReadVerb::parse("run-rerun"), None);
    }

    #[test]
    fn login_code_is_allowed_to_pilgrim_only() {
        assert!(allows_login_code("pilgrim"));
        for f in ["builder", "groomer", "concierge", "czar", "ops", "mender", "batcher", "warden", "spike", "mimir", "", "Pilgrim"] {
            assert!(!allows_login_code(f), "{f}");
        }
    }
}
