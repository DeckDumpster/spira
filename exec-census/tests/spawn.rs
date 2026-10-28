//! Real /proc: N known processes launched under this test's own unit are counted for it.
use exec_census::{ProcFs, Sampler, Window};
use std::process::{Command, Stdio};

#[test]
fn n_spawned_processes_show_at_least_n_for_their_unit() {
    const N: u64 = 25;
    let fs = ProcFs::new("/proc");
    let own = exec_census::unit_of_cgroup(&std::fs::read_to_string("/proc/self/cgroup").unwrap());
    let mut sampler = Sampler::new();
    sampler.tick(&fs);
    let mut kids: Vec<_> = (0..N).map(|_| Command::new("sleep").arg("30").stdin(Stdio::null()).spawn().unwrap()).collect();
    let mut w = Window::default();
    w.add(sampler.tick(&fs));
    for k in &mut kids {
        let _ = k.kill();
        let _ = k.wait();
    }
    assert!(w.by_unit.get(&own).copied().unwrap_or(0) >= N, "unit {own}: {:?}", w.by_unit);
    assert!(w.forks >= N, "the exact fork count covers the spawns: {}", w.forks);
    assert!(w.coverage() > 0.0);
}
