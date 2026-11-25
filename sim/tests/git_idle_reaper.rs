//! A refused or idle reaper pass asks git nothing; only a terminal checkout is asked about.
//! This binary sets PATH, so it is its own process.

#[macro_use]
extern crate spira_test_id;

#[path = "git_idle_fixture/mod.rs"]
mod fixture;

use fixture::*;

#[test_id(t67ro7u)]
#[test]
fn the_reaper_observe_asks_git_only_about_a_checkout_the_cheap_rules_leave_undecided() {
    let f = fixture(testkit::env);
    spira_config::process::with_fresh(|| {
    let names = ["hel.reap_terminal.observe"];
    let root = f.effects.world.run_dir.join("worktree");
    std::fs::create_dir_all(&root).unwrap();
    for n in [".testbatch-1", "sp-live01", "sp-keep01", "sp-term01"] {
        git(&f.local, &["worktree", "add", "-q", "--detach", root.join(n).to_str().unwrap()], 1);
    }
    std::fs::write(root.join("sp-keep01/.spira-keep"), "").unwrap();
    let (warm, idle, after) = (START + HOUR, START + 2 * HOUR, START + 3 * HOUR);
    let (mut at_warm, mut at_idle) = (0, 0);
    let under = root.display().to_string();
    run_hours(&f, &names, (&[], 0), &[warm, idle, after], &mut |t| match t {
        t if t == warm => {
            let refused = logged(&f);
            assert!(!refused.iter().any(|l| l.contains(&under)), "a pass the empty live list refused asked git: {}", argv_counts(&refused));
            *f.checkout_rows.lock().unwrap() = vec![("sp-live01".into(), "WORKING".into()), ("sp-term01".into(), "WORKING".into())];
            at_warm = refused.len();
        }
        t if t == idle => {
            at_idle = logged(&f).len();
            *f.checkout_rows.lock().unwrap() = vec![("sp-live01".into(), "WORKING".into()), ("sp-term01".into(), "LANDED".into())];
        }
        _ => {}
    });
    let all = logged(&f);
    assert!(f.seen.lock().unwrap().iter().filter(|(n, _)| n == names[0]).count() >= 2, "fewer than two passes ran");
    let idle_hour = &all[at_warm..at_idle];
    assert!(!idle_hour.iter().any(|l| l.contains(&under)), "git asked about a kept checkout: {}\n{}", argv_counts(idle_hour), idle_hour.join("\n"));
    let asked: Vec<&String> = all[at_idle..].iter().filter(|l| l.contains(&under)).collect();
    assert!(!asked.is_empty(), "the terminal checkout was never asked about: {}", argv_counts(&all[at_idle..]));
    assert!(asked.iter().all(|l| l.contains("sp-term01")), "git asked about more than the terminal checkout: {}\n{}", argv_counts(&all[at_idle..]), asked.iter().map(|l| l.as_str()).collect::<Vec<_>>().join("\n"));
    for verb in ["rev-parse --git-common-dir", "rev-list -n1 HEAD"] {
        assert!(asked.iter().any(|l| l.contains(verb)), "no {verb:?} on the terminal checkout: {}", argv_counts(&all[at_idle..]));
    }
    })
    .unwrap();
}
