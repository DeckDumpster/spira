/// Read verbs: synchronous, no fayth restriction.
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
}
