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
