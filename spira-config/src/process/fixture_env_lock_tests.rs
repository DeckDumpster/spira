use super::{fixture_home, fixture_toml};
use std::sync::mpsc;
use std::time::Duration;

#[test_id(tr9l7gj)]
#[test]
fn fixture_toml_reads_the_home_under_the_environment_lock_not_a_neighbours_swap() {
    let dir = testkit::TempDir::new("fixture-env-lock");
    let missing = dir.path().join("no-such-home").display().to_string();
    let (held_tx, held_rx) = mpsc::channel();
    let (b_done_tx, b_done_rx) = mpsc::channel::<()>();
    let a = std::thread::spawn(move || {
        let _g = testkit::env(&[("SPIRA_HOME", Some(missing.as_str()))]);
        held_tx.send(()).unwrap();
        // B ends (read or panic) only while A holds if the read ignores the lock; a locked B
        // waits, so this times out and A releases.
        let _ = b_done_rx.recv_timeout(Duration::from_millis(300));
    });
    held_rx.recv().unwrap();
    let out = dir.path().to_path_buf();
    let b = std::thread::spawn(move || {
        let _done = b_done_tx;
        fixture_toml(&out, &[])
    });
    a.join().unwrap();
    let p = b.join().expect("fixture_toml waited for the swap to end and read the real home");
    assert!(p.exists());
}

#[test_id(t7mo53n)]
#[test]
fn fixture_home_waits_out_a_neighbours_swap_and_names_the_real_home() {
    let dir = testkit::TempDir::new("fixture-home-lock");
    let missing = dir.path().join("no-such-home");
    let swapped = missing.display().to_string();
    let (held_tx, held_rx) = mpsc::channel();
    let (b_done_tx, b_done_rx) = mpsc::channel::<()>();
    let a = std::thread::spawn(move || {
        let _g = testkit::env(&[("SPIRA_HOME", Some(swapped.as_str()))]);
        held_tx.send(()).unwrap();
        let _ = b_done_rx.recv_timeout(Duration::from_millis(300));
    });
    held_rx.recv().unwrap();
    let b = std::thread::spawn(move || {
        let _done = b_done_tx;
        fixture_home()
    });
    a.join().unwrap();
    let home = b.join().unwrap();
    assert_ne!(home, missing);
    assert!(home.join("conf.d").is_dir(), "{}", home.display());
}
