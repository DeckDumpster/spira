use spira_sim::control_stub::serve;
use std::path::Path;
use std::time::{Duration, Instant};

fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let by = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < by, "{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn started(world: &Path, phase: &str) -> (std::path::PathBuf, std::path::PathBuf, std::thread::JoinHandle<Result<(), String>>) {
    let (sock, phase_file) = (world.join("ctl.sock"), world.join("cutover.phase"));
    std::fs::write(&phase_file, phase).unwrap();
    let (s, p) = (sock.clone(), phase_file.clone());
    let stub = std::thread::spawn(move || serve(&s, &p));
    wait_until("the stub never bound its socket", || sock.exists());
    (sock, phase_file, stub)
}

#[test]
fn the_stub_answers_until_the_walk_is_done_with_no_wall_clock_life() {
    let world = testkit::TempDir::new("sim-control-stub");
    let (sock, phase_file, stub) = started(world.path(), "land\n");
    std::thread::sleep(Duration::from_millis(1500));
    assert!(runtime::socket::lc_call(&sock, &["halt"]).is_ok(), "a stub mid-walk must still answer after any real time has passed");
    assert!(!stub.is_finished(), "the stub left before the walk was done");
    std::fs::write(&phase_file, "done\n").unwrap();
    stub.join().unwrap().unwrap();
    assert!(!sock.exists(), "the stub leaves no socket behind");
}

#[test]
fn the_stub_ends_with_a_world_that_was_torn_down_before_the_walk_finished() {
    let world = testkit::TempDir::new("sim-control-stub");
    let dir = world.path().join("w");
    std::fs::create_dir(&dir).unwrap();
    let (_sock, phase_file, stub) = started(&dir, "land\n");
    std::fs::remove_file(&phase_file).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    wait_until("the stub outlived its world", || stub.is_finished());
    stub.join().unwrap().unwrap();
}
