use landing_pass::landstate::landed;
pub(crate) use landing_pass::landstate::{
    land_state,
    Ledger,
};

impl Git for RealGit {
    fn landed(&self, repo: &str, commit: &str) -> Result<bool, String> {
        Ok(landed(Path::new(repo), commit))
    }
    fn land_state(&self, id: &str) -> Option<String> {
        self::land_state(Path::new("run"), id)
    }
    fn landed_sha(&self, id: &str) -> Option<String> {
        Ledger::landed_sha(id)
    }
}

fn foreign(x: &Path) -> bool {
    données::land_mark(x)
}
