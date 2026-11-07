//! The run ceiling: a testenv run is admitted only while the host has CPU for it, measured as
//! the host's cores over the cargo jobs one run uses (`SPIRA_TESTENV_MAX_RUNS` overrides). A run
//! past the ceiling waits for a slot and is never refused for load.

use spira_config::admission::{self as adm, Guard, Pool, Procs, Request, Seams};
use std::path::Path;
use std::time::Duration;

/// The slot this run holds until it ends.
pub struct Slot(pub Guard);

/// The wait outran the run's `--deadline` before a slot came free.
#[derive(Debug, PartialEq, Eq)]
pub struct DeadlineWaiting {
    pub waited: u64,
}

impl DeadlineWaiting {
    pub fn line(&self) -> String {
        format!("testenv: no CPU slot after {}s — the deadline passed while waiting for the run slot; nothing was tested", self.waited)
    }
}

/// Blocks until a slot of `size` is free, saying the wait on `say`, or until `out_of_time`.
pub fn take(
    run: &Path,
    holder_pid: u32,
    who: &str,
    size: u64,
    procs: &dyn Procs,
    sleep: &dyn Fn(Duration),
    out_of_time: &dyn Fn() -> bool,
    say: &mut dyn FnMut(&str),
) -> Result<Slot, DeadlineWaiting> {
    let q = Request { run, pool: Pool::Run, holder_pid, who, inherit: None, weight: 1 };
    let size_of = || size;
    let started = adm::now_epoch();
    adm::acquire_unless(&q, &Seams { size_of: &size_of, procs, sleep }, say, out_of_time)
        .map(Slot)
        .ok_or_else(|| DeadlineWaiting { waited: adm::now_epoch().saturating_sub(started) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    struct Procs1;
    impl Procs for Procs1 {
        fn start_of(&self, pid: u32) -> Option<u64> {
            Some(pid as u64)
        }
        fn ppid_of(&self, _pid: u32) -> Option<u32> {
            None
        }
    }

    fn dir() -> testkit::TempDir {
        testkit::TempDir::new("testenv-runslot")
    }

    #[test]
    fn with_the_ceiling_at_one_a_second_run_reports_waiting_and_proceeds_when_the_first_releases() {
        let d = dir();
        let first = RefCell::new(Some(take(&d, 101, "sp-a", 1, &Procs1, &|_| {}, &|| false, &mut |_| {}).ok().expect("free slot")));
        let said = RefCell::new(Vec::new());
        let sleeps = Cell::new(0);
        let sleep = |_: Duration| {
            sleeps.set(sleeps.get() + 1);
            first.borrow_mut().take();
        };
        let second = take(&d, 202, "sp-b", 1, &Procs1, &sleep, &|| false, &mut |l: &str| said.borrow_mut().push(l.to_string()));
        assert!(second.is_ok());
        assert_eq!(sleeps.get(), 1, "it waited exactly until the first released");
        assert_eq!(said.borrow().first().map(String::as_str), Some("testenv: waiting for a CPU slot (1 running)"));
    }

    #[test]
    fn a_wait_past_the_deadline_is_a_fault_naming_the_slot() {
        let d = dir();
        let _held = take(&d, 101, "sp-a", 1, &Procs1, &|_| {}, &|| false, &mut |_| {}).ok().expect("free slot");
        let out_of_time = Cell::new(false);
        let sleep = |_: Duration| out_of_time.set(true);
        let r = take(&d, 202, "sp-b", 1, &Procs1, &sleep, &|| out_of_time.get(), &mut |_| {});
        let e = r.err().expect("the wait outran the deadline");
        assert!(e.line().contains("CPU slot") && e.line().contains("deadline"), "{}", e.line());
    }
}
