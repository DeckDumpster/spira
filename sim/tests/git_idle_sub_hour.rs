//! The sub-hour observes start no git over an idle hour; a moved ref wakes only what reads it.
//! This binary sets PATH, so it is its own process.

#[macro_use]
extern crate spira_test_id;

#[path = "git_idle_fixture/mod.rs"]
mod fixture;

use fixture::*;

const SUB_HOUR: [&str; 7] = ["publish-backlog.observe", "publish.observe", "cockpit-tier-600s.observe", "trigger.mimir.observe", "intake.gh.observe", "intake.kit_bump.observe", "brain-dirty.observe"];

fn unsent_kv(f: &Fixture) -> String {
    let out: serde_json::Value = serde_json::from_str(&last_output(f, "cockpit-tier-600s.observe")).unwrap_or_default();
    out["fragments"]["cockpit.unsent"]["kv"].to_string()
}

#[test_id(trka3uo)]
#[test]
fn the_sub_hour_observes_run_no_git_over_an_idle_hour_and_a_moved_ref_wakes_only_what_reads_it() {
    let f = fixture(testkit::env);
    spira_config::process::with_fresh(|| {
    assert_traps_and_double_are_live(&f);
    let (warm, idle) = (START + HOUR, START + 2 * HOUR);
    let marks = [warm, idle, idle + 16 * MINUTE, idle + 32 * MINUTE, idle + 43 * MINUTE, idle + 54 * MINUTE];
    let (mut at_warm, mut at_idle) = (0, 0);
    let mut unsent_before = String::new();
    let second = f.dir_path().join("second");
    run_hours(&f, &SUB_HOUR, (&[], 0), &marks, &mut |t| match t {
        t if t == warm => at_warm = logged(&f).len(),
        t if t == idle => {
            at_idle = logged(&f).len();
            commit(&f.local, "sp-cccccc landed", LANDED_S + 10);
            git(&f.local, &["branch", "-f", "local/main"], 1);
        }
        t if t == idle + 16 * MINUTE => {
            let backlog = last_output(&f, "publish-backlog.observe");
            assert!(backlog.contains("\"count\":1"), "a landing commit is one unpublished: {backlog}");
            git(f.dir_path(), &["clone", "-q", f.forge.to_str().unwrap(), second.to_str().unwrap()], 1);
            commit(&second, "someone else", LANDED_S + 20);
            git(&second, &["push", "-q", "origin", "HEAD:main"], 1);
            commit(&f.local, "sp-dddddd landed", LANDED_S + 30);
            git(&f.local, &["branch", "-f", "local/main"], 1);
        }
        t if t == idle + 32 * MINUTE => {
            let publish = last_output(&f, "publish.observe");
            assert!(publish.contains("\"diverged\":true"), "a forge moved by someone else is diverged once the landing ref moves: {publish}");
            std::fs::write(f.wiki.join("new.md"), "new").unwrap();
        }
        t if t == idle + 43 * MINUTE => {
            assert!(last_output(&f, "brain-dirty.observe").contains("new.md"), "{}", last_output(&f, "brain-dirty.observe"));
            unsent_before = unsent_kv(&f);
            git(&f.local, &["branch", "spira/sp-x"], 1);
        }
        _ => {}
    });
    assert_only_the_double_answered(&f);
    let all = logged(&f);
    for read in [" show local/main:Cargo.toml", "log --format=%ct %s --after=@"] {
        assert!(all[..at_warm].iter().any(|l| l.contains(read)), "warm-up never ran {read:?}, so the idle hour proves nothing about it");
    }
    assert_eq!(at_idle, at_warm, "an idle virtual hour logged git: {}\n{}", argv_counts(&all[at_warm..at_idle]), all[at_warm..at_idle].join("\n"));
    assert_ne!(unsent_kv(&f), unsent_before, "a new spira/* branch changes the unsent section");
    assert!(!unsent_before.is_empty());
    })
    .unwrap();
}
