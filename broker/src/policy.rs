/// Forge-write policy: fayth × verb table. Hard-coded in source per the design.

/// Read verbs: synchronous, bypass the inbox queue, no fayth restriction.
#[derive(Debug, Clone, PartialEq)]
pub enum ReadVerb {
    RunView,
    ArtifactDownload,
}

impl ReadVerb {
    pub fn parse(s: &str) -> Option<ReadVerb> {
        match s {
            "run-view"          => Some(ReadVerb::RunView),
            "artifact-download" => Some(ReadVerb::ArtifactDownload),
            _                   => None,
        }
    }

}

#[derive(Debug, Clone, PartialEq)]
pub enum Verb {
    RunRerun,
    RunCancel,
    PrClose,
    PrComment,
    IssueComment,
    IssueClose,
}

impl Verb {
    pub fn parse(s: &str) -> Option<Verb> {
        match s {
            "run-rerun"     => Some(Verb::RunRerun),
            "run-cancel"    => Some(Verb::RunCancel),
            "pr-close"      => Some(Verb::PrClose),
            "pr-comment"    => Some(Verb::PrComment),
            "issue-comment" => Some(Verb::IssueComment),
            "issue-close"   => Some(Verb::IssueClose),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Verb::RunRerun     => "run-rerun",
            Verb::RunCancel    => "run-cancel",
            Verb::PrClose      => "pr-close",
            Verb::PrComment    => "pr-comment",
            Verb::IssueComment => "issue-comment",
            Verb::IssueClose   => "issue-close",
        }
    }

    /// Only the czar fayth may submit these.
    pub fn czar_only(&self) -> bool {
        matches!(self, Verb::RunRerun | Verb::RunCancel | Verb::PrClose)
    }

    /// Czar-only verbs also pass czar-fence.sh (shadow → CZAR-WOULD).
    pub fn needs_czar_fence(&self) -> bool {
        self.czar_only()
    }

    /// gh pr close only applies to batch PRs (branch spira/queue/*).
    pub fn requires_batch_pr(&self) -> bool {
        matches!(self, Verb::PrClose)
    }
}

/// Policy check result: either allowed (with gh args) or a refusal reason.
pub enum PolicyResult {
    Allowed,
    Refused(String),
}

pub fn check_fayth(verb: &Verb, fayth: &str) -> PolicyResult {
    if verb.czar_only() && fayth != "czar" {
        PolicyResult::Refused(format!(
            "fayth {} may not submit verb {} (czar only)",
            fayth,
            verb.as_str()
        ))
    } else {
        PolicyResult::Allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_VERBS: [Verb; 6] = [
        Verb::RunRerun, Verb::RunCancel, Verb::PrClose,
        Verb::PrComment, Verb::IssueComment, Verb::IssueClose,
    ];

    #[test]
    fn verb_parse_round_trips_through_as_str() {
        for v in ALL_VERBS {
            assert_eq!(Verb::parse(v.as_str()), Some(v));
        }
    }

    #[test]
    fn verb_parse_rejects_unknown_strings() {
        assert_eq!(Verb::parse("run-explode"), None);
        assert_eq!(Verb::parse(""), None);
        assert_eq!(Verb::parse("Run-Rerun"), None); // case-sensitive: no silent normalization
    }

    #[test]
    fn read_verb_parse_round_trips() {
        assert_eq!(ReadVerb::parse("run-view"), Some(ReadVerb::RunView));
        assert_eq!(ReadVerb::parse("artifact-download"), Some(ReadVerb::ArtifactDownload));
        assert_eq!(ReadVerb::parse("run-rerun"), None); // a write verb is not a read verb
    }

    #[test]
    fn only_the_named_three_verbs_are_czar_only() {
        for v in &ALL_VERBS {
            let expect_czar_only = matches!(v, Verb::RunRerun | Verb::RunCancel | Verb::PrClose);
            assert_eq!(v.czar_only(), expect_czar_only, "{:?}", v);
        }
    }

    #[test]
    fn czar_fence_applies_exactly_where_czar_only_does() {
        // needs_czar_fence is a separate method from czar_only so the two CAN diverge, but
        // today's policy has them agree; this pins that until a verb deliberately splits them.
        for v in &ALL_VERBS {
            assert_eq!(v.needs_czar_fence(), v.czar_only(), "{:?}", v);
        }
    }

    #[test]
    fn only_pr_close_requires_a_batch_pr() {
        for v in &ALL_VERBS {
            assert_eq!(v.requires_batch_pr(), matches!(v, Verb::PrClose), "{:?}", v);
        }
    }

    #[test]
    fn czar_only_verb_refuses_a_non_czar_fayth() {
        match check_fayth(&Verb::RunRerun, "builder") {
            PolicyResult::Refused(msg) => {
                assert!(msg.contains("builder"));
                assert!(msg.contains("run-rerun"));
                assert!(msg.contains("czar only"));
            }
            PolicyResult::Allowed => panic!("builder should be refused for a czar-only verb"),
        }
    }

    #[test]
    fn czar_only_verb_allows_the_czar_fayth() {
        assert!(matches!(check_fayth(&Verb::RunRerun, "czar"), PolicyResult::Allowed));
    }

    #[test]
    fn non_czar_only_verb_allows_any_fayth() {
        assert!(matches!(check_fayth(&Verb::PrComment, "builder"), PolicyResult::Allowed));
        assert!(matches!(check_fayth(&Verb::PrComment, "czar"), PolicyResult::Allowed));
        assert!(matches!(check_fayth(&Verb::PrComment, ""), PolicyResult::Allowed));
    }
}
