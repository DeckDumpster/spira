//! The sub-hour observes start no git over an idle hour; a moved ref wakes only what reads it.
//! This binary sets PATH, so it is its own process.

#[macro_use]
extern crate spira_test_id;

#[path = "git_idle_fixture/mod.rs"]
mod fixture;

use fixture::*;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

const SUB_HOUR: [&str; 10] = ["publish-backlog.observe", "publish.observe", "cockpit-tier-600s.observe", "trigger.mimir.observe", "intake.gh.observe", "intake.kit_bump.observe", "brain-dirty.observe", "watch.store_unit.observe", "roundvm.section.observe", "roundvm.section.publish"];

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
    let (mut pool_at_warm, mut pool_at_idle) = (0, 0);
    let mut unsent_before = String::new();
    let second = f.dir_path().join("second");
    run_hours(&f, &SUB_HOUR, (&[], 0), &marks, &mut |t| match t {
        t if t == warm => {
            at_warm = logged(&f).len();
            pool_at_warm = vm_pool_publishes(&f);
        }
        t if t == idle => {
            at_idle = logged(&f).len();
            pool_at_idle = vm_pool_publishes(&f);
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
    assert!(pool_at_warm >= 1, "the warm-up never published VmPool/pool, so the idle hour proves nothing about it");
    let idle_publishes = vm_pool_publishes(&f) - pool_at_idle;
    assert!((1..=4).contains(&idle_publishes), "an idle pool publishes VmPool/pool at most once per 15 virtual minutes, got {idle_publishes} in 54");
    assert_eq!(started(&f, "round-vm"), 0, "the VmPool section is published in process, not by starting round-vm");
    assert_ne!(unsent_kv(&f), unsent_before, "a new spira/* branch changes the unsent section");
    assert!(!unsent_before.is_empty());
    })
    .unwrap();
}

struct Holder(std::process::Child);

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_holder(transcript: &Path) -> Holder {
    use std::io::BufRead;
    let mut child = Command::new("sh").arg0("claude").args(["-c", "exec 3<\"$1\"; echo open; read held", "sh"]).arg(transcript).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
    let mut said = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut said).unwrap();
    assert_eq!(said.trim(), "open", "the holder has the transcript open before the sim looks");
    Holder(child)
}

fn concierge_starts(f: &Fixture) -> usize {
    f.units.calls().iter().filter(|c| *c == "start concierge.service").count()
}

#[test_id(tx2g5qt)]
#[test]
fn an_idle_hour_starts_neither_the_concierge_service_nor_a_program_and_a_dead_holder_starts_it_once() {
    let f = fixture(testkit::env);
    spira_config::process::with_fresh(|| {
    let (run, projects) = (std::path::PathBuf::from(spira_config::process::cfg("SPIRA_RUN").unwrap()), std::path::PathBuf::from(spira_config::process::cfg("SPIRA_TOKEN_PROJECTS").unwrap()));
    let slug = projects.join("slug");
    std::fs::create_dir_all(&slug).unwrap();
    let transcript = slug.join("sess-1.jsonl");
    std::fs::write(&transcript, "").unwrap();
    std::fs::write(run.join("concierge-session"), "sess-1\n/brain\n").unwrap();
    let mut holder = Some(spawn_holder(&transcript));
    for stub in ["concierge.sh", "release"] {
        Command::new(stub).output().unwrap();
        assert_eq!(started(&f, stub), 1, "the {stub} stub is what PATH resolves, so a started program is seen");
    }
    std::fs::remove_file(&f.started).unwrap();
    let (first, idle, killed, restored) = (START + 5 * MINUTE, START + 65 * MINUTE, START + 75 * MINUTE, START + 95 * MINUTE);
    let (mut at_first, mut at_idle, mut at_killed) = (0, 0, 0);
    run_hours(&f, &["concierge.ensure.observe", "unit.start_installed"], (&[], 0), &[first, idle, killed, restored], &mut |t| match t {
        t if t == first => at_first = concierge_starts(&f),
        t if t == idle => {
            at_idle = concierge_starts(&f);
            drop(holder.take());
        }
        t if t == killed => {
            at_killed = concierge_starts(&f);
            holder = Some(spawn_holder(&transcript));
        }
        _ => {}
    });
    assert_eq!(at_first, 1, "the first tick starts the service once");
    assert_eq!(at_idle, 1, "an idle hour started concierge.service again: {:?} {}", f.units.calls(), last_output(&f, "concierge.ensure.observe"));
    assert_eq!(at_killed, 2, "the dead holder is one start on the next tick: {:?}", f.units.calls());
    assert_eq!(concierge_starts(&f), 2, "the holder the start brought back is no change: {:?}", f.units.calls());
    for program in ["concierge.sh", "release"] {
        assert_eq!(started(&f, program), 0, "the task started {program}");
    }
    })
    .unwrap();
}
