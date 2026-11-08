//! The unit manager this process reads, chosen once from the environment.

use std::sync::Arc;

use runtime::ports::WallClock;
use runtime::units::{for_host, UnitFacts, UnitHandle, UnitSpec, Units, UNITS_KEY};
use runtime::{Result, RuntimeError};

/// A backend that could not be opened: every read says why, so a probe renders "unknown"
/// rather than an all-clear.
struct Unreachable(String);

impl Unreachable {
    fn err<T>(&self) -> Result<T> {
        Err(RuntimeError::new(self.0.clone()))
    }
}

impl Units for Unreachable {
    fn start(&self, _: &UnitSpec) -> Result<UnitHandle> {
        self.err()
    }
    fn stop(&self, _: &UnitHandle) -> Result<()> {
        self.err()
    }
    fn restart(&self, _: &UnitHandle) -> Result<()> {
        self.err()
    }
    fn facts(&self, _: &UnitHandle) -> Result<UnitFacts> {
        self.err()
    }
}

/// A manager that answers every read with `why`.
pub fn host_units_unreachable(why: &str) -> Arc<dyn Units> {
    Arc::new(Unreachable(why.into()))
}

/// `SPIRA_UNITS` picks the backend; a `SPIRA_SYSTEMCTL` override outranks the default.
pub fn host_units() -> Arc<dyn Units> {
    let var = |k: &str| std::env::var(k).ok();
    match for_host(Arc::new(WallClock), var(UNITS_KEY).as_deref(), var("SPIRA_SYSTEMCTL").as_deref()) {
        Ok(backend) => backend.units(),
        Err(e) => Arc::new(Unreachable(format!("unit manager unreachable: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unopened_backend_answers_every_read_with_an_error() {
        let u = Unreachable("no bus".into());
        assert!(u.detail(&UnitHandle::named("a.service")).is_err());
        assert!(u.list("spira-*").is_err());
    }
}
