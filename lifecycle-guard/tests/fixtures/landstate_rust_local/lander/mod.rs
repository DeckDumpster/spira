pub trait Git {
    fn landed(&self, repo: &str, commit: &str) -> Result<bool, String>;
    fn landing_sha(&self, repo: &str) -> Result<String, String>;
}

pub struct Ports<'a> {
    pub git: &'a dyn Git,
}

pub fn decide(p: &Ports, repo: &str, head: &str) -> bool {
    match p.git.landed(repo, head) {
        Ok(true) => true,
        _ => p
            .git
            .landed(repo, &p.git.landing_sha(repo).unwrap_or_default())
            .unwrap_or(false),
    }
}

struct Fake;

impl Git for Fake {
    fn landed(&self, _repo: &str, commit: &str) -> Result<bool, String> {
        Ok(commit == "h1")
    }
    fn landing_sha(&self, _repo: &str) -> Result<String, String> {
        Self::landed(self, "r", "h1").map(|_| "h1".to_string())
    }
}
