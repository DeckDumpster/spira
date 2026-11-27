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

const SUB_HOUR: [&str; 12] = [
    "publish-backlog.observe",
    "publish.observe",
    "cockpit-tier-600s.observe",
    "trigger.mimir.observe",
    "intake.gh.observe",
    "intake.kit_bump.observe",
    "brain-dirty.observe",
    "watch.store_unit.observe",
    "roundvm.section.observe",
    "roundvm.section.publish",
    "cockpit.ensure.observe",
    "unit.start_installed",
];

fn probe_kv(f: &Fixture, probe: &str) -> String {
    let out: serde_json::Value = serde_json::from_str(&last_output(f, "cockpit-tier-600s.observe")).unwrap_or_default();
    out["fragments"][probe]["kv"].to_string()
}

#[test_id(trka3uo)]
#[test]
fn the_sub_hour_observes_run_no_git_over_an_idle_hour_and_a_moved_ref_wakes_only_what_reads_it() {
    let mut f = fixture(testkit::env);
    f.cockpit_input = cockpit_600s_every_probe();
    spira_config::process::with_fresh(|| {
    assert_traps_and_double_are_live(&f);
    assert_stubs_log(&f);
    let extra = spira_config::release_env::child_path_env_for_process().unwrap();
    for program in ["bash", "drift.sh", "tsd-query.sh"] {
        Command::new(program).args(["-c", "true"]).envs(extra.clone()).output().unwrap();
        assert_eq!(started(&f, program), 1, "a {program} started the way a probe starts one is counted");
    }
    Command::new("yield.sh").output().unwrap();
    assert_eq!(started(&f, "yield.sh"), 1, "the yield.sh stub is what PATH resolves, so a started one is seen");
    let records = std::path::PathBuf::from(spira_config::process::cfg("SPIRA_RUN").unwrap()).join("gate-yield");
    std::fs::create_dir_all(&records).unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    std::fs::write(records.join("planted"), format!("at={now}\nsuite=test-planted.sh\nverdict=GATE_FAULT\n")).unwrap();
    let (warm, idle) = (START + HOUR, START + 2 * HOUR);
    let unsent_passes = |f: &Fixture| f.seen.lock().unwrap().iter().filter(|(n, _)| n == "cockpit-tier-600s.observe").count();
    let (mut yields_at_warm, mut yields_at_idle, mut passes_at_warm, mut passes_at_idle) = (0, 0, 0, 0);
    let shells = |f: &Fixture| ["bash", "drift.sh", "tsd-query.sh"].map(|p| started(f, p));
    let (mut shells_at_warm, mut shells_at_idle) = ([0; 3], [0; 3]);
    let (mut at_dirtied, mut at_new_ref) = (0, 0);
    let mut statute_before = String::new();
    let marks = [warm, idle, idle + 16 * MINUTE, idle + 32 * MINUTE, idle + 43 * MINUTE, idle + 54 * MINUTE];
    let (mut at_warm, mut at_idle) = (0, 0);
    let ensures = |f: &Fixture| f.units.calls().iter().filter(|c| *c == &format!("start {ENSURE_UNIT}")).count();
    let (mut ensures_at_warm, mut ensures_at_idle, mut ensures_after_touch) = (0, 0, 0);
    let (mut pool_at_warm, mut pool_at_idle) = (0, 0);
    let mut unsent_before = String::new();
    let second = f.dir_path().join("second");
    run_hours(&f, &SUB_HOUR, (&[], 0), &marks, &mut |t| match t {
        t if t == warm => {
            (at_warm, ensures_at_warm) = (logged(&f).len(), ensures(&f));
            (yields_at_warm, passes_at_warm) = (started(&f, "yield.sh"), unsent_passes(&f));
            pool_at_warm = vm_pool_publishes(&f);
            shells_at_warm = shells(&f);
        }
        t if t == idle => {
            (at_idle, ensures_at_idle) = (logged(&f).len(), ensures(&f));
            (yields_at_idle, passes_at_idle) = (started(&f, "yield.sh"), unsent_passes(&f));
            pool_at_idle = vm_pool_publishes(&f);
            shells_at_idle = shells(&f);
            commit(&f.local, "sp-cccccc landed", LANDED_S + 10);
            git(&f.local, &["branch", "-f", "local/main"], 1);
        }
        t if t == idle + 16 * MINUTE => {
            touch(&f.release.join("bin/health"), 1_000_000_005);
            let backlog = last_output(&f, "publish-backlog.observe");
            assert!(backlog.contains("\"count\":1"), "a landing commit is one unpublished: {backlog}");
            git(f.dir_path(), &["clone", "-q", f.forge.to_str().unwrap(), second.to_str().unwrap()], 1);
            commit(&second, "someone else", LANDED_S + 20);
            git(&second, &["push", "-q", "origin", "HEAD:main"], 1);
            commit(&f.local, "sp-dddddd landed", LANDED_S + 30);
            git(&f.local, &["branch", "-f", "local/main"], 1);
        }
        t if t == idle + 32 * MINUTE => {
            ensures_after_touch = ensures(&f);
            let publish = last_output(&f, "publish.observe");
            assert!(publish.contains("\"diverged\":true"), "a forge moved by someone else is diverged once the landing ref moves: {publish}");
            std::fs::write(f.wiki.join("new.md"), "new").unwrap();
            (at_dirtied, statute_before) = (logged(&f).len(), probe_kv(&f, "cockpit.statute"));
            std::fs::write(f.local.join("stray"), "stray").unwrap();
            std::fs::write(f.wiki.join(STATUTE_PAGE), "### law-one\n### law-two\n").unwrap();
            git(&f.wiki, &["add", STATUTE_PAGE], 2);
            commit(&f.wiki, "a second statute", LANDED_S + 40);
        }
        t if t == idle + 43 * MINUTE => {
            assert!(last_output(&f, "brain-dirty.observe").contains("new.md"), "{}", last_output(&f, "brain-dirty.observe"));
            unsent_before = probe_kv(&f, "cockpit.unsent");
            at_new_ref = logged(&f).len();
            git(&f.local, &["branch", "spira/sp-x"], 1);
            let client = f.proc_root.join("201");
            std::fs::create_dir_all(&client).unwrap();
            std::fs::write(client.join("stat"), format!("201 (tmux: client) S 50 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 {} 2010 0\n", ["0"; 2].join(" "))).unwrap();
        }
        _ => {}
    });
    assert_only_the_double_answered(&f);
    let all = logged(&f);
    let statute_show = format!("-C {} show HEAD:{STATUTE_PAGE}", f.wiki.display());
    let status_of_local = format!("-C {} status --porcelain", f.local.display());
    assert!(all[..at_warm].iter().any(|l| l.ends_with(&statute_show)), "warm-up never read the statute page, so the idle hour proves nothing about it");
    assert_eq!(shells_at_idle, shells_at_warm, "an idle virtual hour started bash, drift.sh or tsd-query.sh");
    let (before_new_ref, after_dirtied) = (&all[at_dirtied..at_new_ref], &all[at_dirtied..]);
    assert_eq!(before_new_ref.iter().filter(|l| l.ends_with(&status_of_local)).count(), 1, "a written file in the checkout is one status read: {before_new_ref:?}");
    assert_eq!(after_dirtied.iter().filter(|l| l.ends_with(&statute_show)).count(), 1, "a wiki commit is one read of the statute page: {after_dirtied:?}");
    assert!(statute_before.contains(r#""SP_STATUTE_PAGE_N":"1""#), "{statute_before}");
    assert!(probe_kv(&f, "cockpit.statute").contains(r#""SP_STATUTE_PAGE_N":"2""#), "{}", probe_kv(&f, "cockpit.statute"));
    assert!(probe_kv(&f, "cockpit.drift").contains(r#""SP_CHECKOUT_DRIFT":"DIRTY:1""#), "{}", probe_kv(&f, "cockpit.drift"));
    for read in [" show local/main:Cargo.toml", "log --format=%ct %s --after=@"] {
        assert!(all[..at_warm].iter().any(|l| l.contains(read)), "warm-up never ran {read:?}, so the idle hour proves nothing about it");
    }
    assert_eq!(at_idle, at_warm, "an idle virtual hour logged git: {}\n{}", argv_counts(&all[at_warm..at_idle]), all[at_warm..at_idle].join("\n"));
    assert!(passes_at_idle - passes_at_warm >= 5, "the unsent probe ran {} times in the idle hour, so it proves little", passes_at_idle - passes_at_warm);
    assert_eq!(yields_at_idle - yields_at_warm, 0, "an idle virtual hour started yield.sh");
    assert!(unsent_before.contains("SP_YIELD_REDS") && unsent_before.contains("test-planted.sh x1"), "the unsent section carries the reading of the record under the run directory: {unsent_before}");
    assert_eq!((ensures_at_warm, ensures_at_idle), (1, 1), "the cockpit is ensured once on the first tick and not over an idle hour");
    assert_eq!(ensures_after_touch, 2, "a touched bin/health is one more start on the next tick");
    assert_eq!(ensures(&f), 3, "a new tmux client is one more start, and nothing else started one after the touch");
    assert_eq!(started(&f, "tmux"), 0, "the cockpit task read /proc and started no tmux");
    assert!(pool_at_warm >= 1, "the warm-up never published VmPool/pool, so the idle hour proves nothing about it");
    let idle_publishes = vm_pool_publishes(&f) - pool_at_idle;
    assert!((1..=4).contains(&idle_publishes), "an idle pool publishes VmPool/pool at most once per 15 virtual minutes, got {idle_publishes} in 54");
    assert_eq!(started(&f, "round-vm"), 0, "the VmPool section is published in process, not by starting round-vm");
    assert_ne!(probe_kv(&f, "cockpit.unsent"), unsent_before, "a new spira/* branch changes the unsent section");
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
