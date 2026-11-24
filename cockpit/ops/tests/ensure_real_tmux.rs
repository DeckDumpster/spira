//! `layout ensure` against a real tmux server: the snapshot format must expand the way the
//! parser expects, or a healthy layout would be rewritten every minute.

#[macro_use]
extern crate spira_test_id;
use cockpit_ops::layout::{Conf, Layout};
use cockpit_ops::tmux::Tmux;
use std::fs;

#[test_id(tr3a8kq)]
#[test]
fn a_second_ensure_over_a_real_tmux_makes_one_read_and_no_writes() {
    let dir = testkit::TempDir::new("ensure-real-tmux");
    let sock = dir.join("tmux.sock");
    let log = dir.join("calls.log");
    let wrapper = dir.join("tmux-wrapper");
    testkit::write_exe(
        &wrapper,
        &format!("#!/bin/sh\necho \"$*\" >> {}\nexec tmux -S {} -f /dev/null \"$@\"\n", log.display(), sock.display()),
    );
    let tmux = Tmux::with_bin(wrapper.to_str().unwrap());
    let health_sh = dir.join("health.sh");
    fs::write(&health_sh, "sleep 60\n").unwrap();

    assert!(tmux.ok(&["new-session", "-d", "-s", "cockpit", "-x", "200", "-y", "50", "sleep 60"]));
    let health = tmux
        .run(&["split-window", "-P", "-F", "#{pane_id}", "-d", "-h", "-f", "-l", "33%", &format!("sh {}", health_sh.display())])
        .expect("split");
    assert!(tmux.ok(&["set-option", "-p", "-t", &health, "@cockpit", "health"]));
    assert!(tmux.ok(&["set-option", "-w", "@cockpit_up", "1"]));

    let conf = Conf {
        spira_release: String::new(),
        cock: dir.path().to_path_buf(),
        run: dir.path().to_path_buf(),
        cwd: dir.path().display().to_string(),
        right_pct: 33,
        bottom_pct: 28,
        mail_cmd: None,
        heal_cooldown_secs: 60,
        idle_secs: 600,
        mouse_on: true,
        clipboard_on: true,
        spira_repo: String::new(),
        spira_conf: None,
    };
    let layout = Layout { tmux: Tmux::with_bin(wrapper.to_str().unwrap()), conf };
    let me = std::env::current_exe().unwrap();
    layout.ensure(&me, &me).expect("first ensure");
    fs::write(&log, "").unwrap();
    layout.ensure(&me, &me).expect("second ensure");
    let calls = fs::read_to_string(&log).unwrap();
    let _ = tmux.ok(&["kill-server"]);
    assert_eq!(calls.lines().count(), 1, "{calls}");
}
