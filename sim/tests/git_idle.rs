//! With no ref moved, the scheduler's per-minute tasks start no git; a moved ref wakes only the
//! reads that depend on it. This binary sets PATH, so it is its own process.

#[macro_use]
extern crate spira_test_id;

#[path = "git_idle_fixture/mod.rs"]
mod fixture;

use fixture::*;

#[test_id(tqoiwy1)]
#[test]
fn an_idle_hour_after_warm_up_logs_zero_git_and_a_moved_ref_wakes_only_its_own_reads() {
    let f = fixture(testkit::env);
    for shared in [&f.local, &f.pr] {
        per_worktree_config(shared);
    }
    spira_config::process::with_fresh(|| {
    assert_traps_and_double_are_live(&f);
    assert_stubs_log(&f);
    let warm = START + 10 * MINUTE;
    let (mut at_warm, mut at_idle, mut at_commit, mut at_branch, mut at_quiet_a, mut at_quiet_b) = (0, 0, 0, 0, 0, 0);
    let (mut programs_at_warm, mut programs_at_idle, mut programs_at_ready, mut idle_passes, mut rows_before_ready) = (0, Vec::new(), Vec::new(), 0, Vec::new());
    let (mut tower_at_warm, mut tower_at_idle, mut tower_at_edit, mut tower_at_boundary, mut level_before) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), (0.0, 0.0));
    let spec = f.overrides.join("round-vm-spill-ec2-always.override");
    std::fs::write(&spec, "needed() { return 1; }\n").unwrap();
    let ran = f.dir_path().join("overrides.log");
    let fail = f.dir_path().join("overrides.fail");
    for bin in [f.dir_path().join("bin"), f.dir_path().join("release/bin")] {
        testkit::write_exe(bin.join("overrides.sh"), &format!("#!/bin/sh\necho \"$*\" >> {}\ntouch {}\n[ -e {} ] && {{ echo 'overrides: ghost: failed' >&2; exit 1; }}\nexit 0\n", ran.display(), f.toml.display(), fail.display()));
    }
    let runs = || std::fs::read_to_string(&ran).unwrap_or_default().lines().count();
    let mut applied: std::collections::BTreeMap<&str, usize> = Default::default();
    let (mut sweep_at_warm, mut sweep_at_idle, mut guard_at_warm, mut towers_at_warm, mut towers_at_idle) = (Vec::new(), Vec::new(), (0, 0), 0, 0);
    let towers = |f: &Fixture| f.seen.lock().unwrap().iter().filter(|(n, _)| n == spira_runtime::tasks::d::watch_tower::OBSERVE).count();
    let names = ["watch.throughput.observe", "landing.observe", "overrides.apply", "inbox.notify.observe", "asks.ideas.observe", "batcher.local.observe", "cockpit-tier-60s.observe", "cr-cadence.observe", spira_runtime::tasks::d::watch_tower::OBSERVE];
    let sentinel = ["sentinel-pass.observe"];
    let reads = ["test-registry", "podman", "date", "timeout 5 date"];
    let counts = |f: &Fixture| reads.map(|p| started(f, p));
    let (mut reads_at_warm, mut reads_at_idle, mut reads_at_registry, mut reads_at_images, mut reads_at_expiry, mut reads_at_midnight) = ([0; 4], [0; 4], [0; 4], [0; 4], [0; 4], [0; 4]);
    let reconciled = untagged_images(&f);
    let (before_hour, after) = (START + 59 * MINUTE, warm + HOUR);
    let m = |n: u64| after + n * MINUTE;
    let (quiet_a, quiet_b) = (after - 7 * MINUTE, after - 4 * MINUTE);
    let mut marks: Vec<u64> = (0..=60).map(|k| warm + k * MINUTE).chain([before_hour, quiet_a, quiet_b, m(3), m(6), m(8), m(9), m(10), m(12), m(15), m(16), m(18), m(21)]).collect();
    marks.sort();
    marks.dedup();
    run_hours(&f, &names, (&sentinel, warm + HOUR - 10 * MINUTE), &marks, &mut |t| {
        let _ = reconciler::observe_untagged_images(&reconciled);
        for days_after in [0, 1] {
            spira_runtime::world::cr_open_epoch("UTC", 9, t / 1000, days_after).unwrap();
        }
        match t {
        t if t == warm => {
            applied.insert("warm", runs());
            (at_warm, programs_at_warm) = (logged(&f).len(), logged_programs(&f).len());
            reads_at_warm = counts(&f);
            sweep_at_warm = started_by_the_sweep(&f);
            guard_at_warm = (guard_reads(&logged(&f), &f.local), guard_reads(&logged(&f), &f.pr));
            towers_at_warm = towers(&f);
        }
        t if t == before_hour => drop(applied.insert("idle", runs())),
        t if t == quiet_a => {
            tower_at_warm = started_by_the_watchtower_checks(&f);
            at_quiet_a = logged(&f).len();
        }
        t if t == quiet_b => {
            at_quiet_b = logged(&f).len();
            tower_at_idle = started_by_the_watchtower_checks(&f);
            level_before = (host_level(&f, "certify_par"), host_level(&f, "stale_targets"));
            std::fs::write(&f.watchers, "a\nb\n").unwrap();
        }
        t if t == m(6) => {
            applied.insert("spec", runs());
            touch(&f.toml, 1_000_000_002);
            at_branch = logged(&f).len();
        }
        t if t == m(8) => rewrite(&f.images, "[1]"),
        t if t == m(9) => {
            applied.insert("toml", runs());
            std::fs::write(&fail, "").unwrap();
            touch(&spec, 1_000_000_003);
        }
        t if t == m(10) => reads_at_images = counts(&f),
        t if t == m(12) => {
            applied.insert("failing", runs());
            std::fs::remove_file(&fail).unwrap();
        }
        t if t == m(15) => drop(applied.insert("recovered", runs())),
        t if t == m(18) => {
            applied.insert("recovered-quiet", runs());
            tower_at_edit = started_by_the_watchtower_checks(&f);
            let doc = spira_config::load(&f.toml).unwrap();
            spira_config::serialize_and_write(&f.toml, &spira_config::set_path(&doc, "spira.certify_par", "7").unwrap()).unwrap();
            stale_target(&f, "sp-aaaaaa");
        }
        t if t == m(16) => reads_at_expiry = counts(&f),
        t if t == m(21) => reads_at_midnight = counts(&f),
        t if t == warm + HOUR => {
            applied.insert("boundary", runs());
            tower_at_boundary = started_by_the_watchtower_checks(&f);
            sweep_at_idle = started_by_the_sweep(&f);
            towers_at_idle = towers(&f);
            at_idle = logged(&f).len();
            programs_at_idle = logged_programs(&f);
            reads_at_idle = counts(&f);
            idle_passes = f.seen.lock().unwrap().iter().filter(|(n, _)| n == "sentinel-pass.observe").count();
            rows_before_ready = strand_rows(&f);
            make_ready(&f, "sp-zzzzzz");
            commit(&f.local, "sp-cccccc landed", LANDED_S + 10);
            git(&f.local, &["branch", "-f", "local/main"], 1);
            rewrite(&f.registry, "rows 22");
        }
        t if t == m(3) => {
            applied.insert("ref", runs());
            touch(&spec, 1_000_000_001);
            at_commit = logged(&f).len();
            programs_at_ready = logged_programs(&f);
            tower_at_edit = started_by_the_watchtower_checks(&f);
            let doc = spira_config::load(&f.toml).unwrap();
            spira_config::serialize_and_write(&f.toml, &spira_config::set_path(&doc, "spira.certify_par", "7").unwrap()).unwrap();
            stale_target(&f, "sp-aaaaaa");
            reads_at_registry = counts(&f);
            git(&f.pr, &["branch", "spira/sp-x"], 1);
        }
        _ => {}
        }
    });
    assert_only_the_double_answered(&f);
    assert!(towers_at_idle >= towers_at_warm + 2, "the watchtower swept {towers_at_warm} then {towers_at_idle} times, so the idle hour proves nothing");
    assert_eq!(sweep_at_idle, sweep_at_warm, "an idle hour of sweeps started programs:\n{}", sweep_at_idle[sweep_at_warm.len().min(sweep_at_idle.len())..].join("\n"));
    let sweep_output: serde_json::Value = serde_json::from_str(&last_output(&f, spira_runtime::tasks::d::watch_tower::OBSERVE)).unwrap_or_else(|e| panic!("the sweep answered no snapshot ({e}): {}", last_output(&f, spira_runtime::tasks::d::watch_tower::OBSERVE)));
    assert!(sweep_output["rendered"].as_str().unwrap_or_default().contains("branch-guard: clean"), "the guard answered: {sweep_output}");
    assert_eq!(guard_at_warm, (1, 1), "warm-up read each checkout's base tip once");
    assert_eq!((guard_reads(&logged(&f), &f.local), guard_reads(&logged(&f), &f.pr)), (2, 1), "the commit on the landing ref is one more guard read of that repo, and none of the other");
    let c = |k: &str| applied[k];
    assert!(c("warm") >= 1, "the override was never applied: {applied:?}");
    assert_eq!(c("idle"), c("warm"), "idle minutes started overrides.sh: {applied:?}");
    assert_eq!(c("boundary"), c("idle") + 1, "the hour boundary runs it once: {applied:?}");
    assert_eq!(c("ref"), c("boundary") + 1, "a moved landing ref is applied once: {applied:?}");
    assert_eq!(c("spec"), c("ref") + 1, "a touched spec is applied once: {applied:?}");
    assert_eq!(c("toml"), c("spec") + 1, "a touched config file is applied once: {applied:?}");
    assert_eq!(c("failing"), c("toml") + 3, "a failing pass runs again on every tick: {applied:?}");
    assert!(c("recovered") > c("failing"), "{applied:?}");
    assert_eq!(c("recovered-quiet"), c("recovered"), "a success is held again: {applied:?}");
    let all = logged(&f);
    let status_of_local = |l: &String| l.ends_with(&format!("-C {} status --porcelain", f.local.display()));
    let first_read = &all[at_warm..at_idle];
    assert!(first_read.len() == 1 && status_of_local(&first_read[0]), "an idle virtual hour's only git is the drift check's first status read:\n{}", first_read.join("\n"));
    assert_eq!(at_quiet_a, at_quiet_b, "a quiet stretch logged git:\n{}", all[at_quiet_a..at_quiet_b].join("\n"));
    assert!(!f.worktrees.parent().unwrap().join("conditions/drift-unreadable-units.count").exists(), "the units half was blind in the fixture");
    assert!(idle_passes >= 5, "the sentinel pass observed {idle_passes} times by the end of the idle hour");
    assert_eq!(programs_at_idle.len(), programs_at_warm, "an idle hour started programs:\n{}", programs_at_idle[programs_at_warm..].join("\n"));
    let ran = |name: &str| f.seen.lock().unwrap().iter().filter(|(n, _)| n == name).count();
    assert!(ran("inbox.notify.observe") >= 12 && ran("asks.ideas.observe") >= 12, "both tasks ran over the hour: {} {}", ran("inbox.notify.observe"), ran("asks.ideas.observe"));
    assert!(reads_at_warm.iter().take(3).all(|n| *n > 0), "warm-up read the registry, podman and date, so the idle hour proves something: {reads_at_warm:?}");
    assert_eq!(reads_at_idle, reads_at_warm, "an idle hour started test-registry, podman, date or timeout");
    assert_eq!(reads_at_registry[0], reads_at_idle[0] + 2, "a rewritten registry file is read once by each of its two readers");
    assert_eq!(reads_at_registry[1..], reads_at_idle[1..], "and by nothing else");
    assert_eq!(reads_at_midnight[0], reads_at_registry[0], "and not again");
    assert_eq!(reads_at_images[1], reads_at_registry[1] + 1, "a rewritten images.json is listed once");
    assert_eq!(reads_at_images[0], reads_at_registry[0], "and does not touch the registry");
    assert_eq!(reads_at_expiry, reads_at_images, "a quiet stretch after the rewrites starts no read");
    assert_eq!(reads_at_midnight[2] - reads_at_expiry[2], 2 * DATE_RUNS_PER_OPEN_EPOCH, "a crossed local midnight asks date again, once for each of today's and tomorrow's open");
    assert_eq!(reads_at_midnight[1], reads_at_expiry[1]);
    assert_eq!(reads_at_midnight[3], reads_at_expiry[3]);
    assert!(!rows_before_ready.iter().any(|r| r.contains("sp-zzzzzz")), "nothing is ready yet: {rows_before_ready:?}");
    let rows = strand_rows(&f);
    assert!(rows.iter().any(|r| r.contains("sp-zzzzzz")), "a bead made READY shows in the next strand rows: {rows:?}");
    assert_eq!(programs_at_ready.len(), programs_at_warm, "and reading it started no program: {programs_at_ready:?}");
    assert!(!tower_at_warm.is_empty(), "warm-up started none of {WATCHTOWER_PROGRAMS:?}, so the idle hour proves nothing");
    assert_eq!(tower_at_idle, tower_at_warm, "idle sentinel passes started programs:\n{}", tower_at_idle[tower_at_warm.len().min(tower_at_idle.len())..].join("\n"));
    let after_touch: Vec<&String> = tower_at_boundary[tower_at_idle.len()..].iter().collect();
    assert_eq!(after_touch.iter().filter(|l| l.starts_with("watchd units")).count(), 1, "a touched manifest runs watchd units once: {after_touch:?}");
    assert_eq!(after_touch.len(), 1, "and nothing else starts: {after_touch:?}");
    let after_edit: Vec<String> = started_by_the_watchtower_checks(&f)[tower_at_edit.len()..].to_vec();
    assert!(after_edit.iter().all(|l| l.starts_with("watchd units")) && after_edit.len() <= 2, "a changed certify_par and a stale target start no read of theirs; the config file is a manifest input, so only watchd may rerun: {after_edit:?}");
    assert_eq!((host_level(&f, "certify_par"), host_level(&f, "stale_targets")), (7.0, level_before.1 + 1.0), "the next Host section shows both: {}\nsentinel pass: {}", last_host_section(&f), conditions_check_refusal(&f));
    let local = f.local.display().to_string();
    let pr = f.pr.display().to_string();
    let after_commit: Vec<&String> = all[at_idle..at_commit].iter().filter(|l| !l.ends_with("rev-parse --verify --quiet refs/heads/spira/sp-zzzzzz")).collect();
    let logs = |lines: &[&String], repo: &str| lines.iter().filter(|l| l.contains(&format!("-C {repo} log "))).count();
    assert_eq!(logs(&after_commit, &local), 1, "a moved landing ref is read once: {after_commit:?}");
    assert_eq!(after_commit.iter().filter(|l| status_of_local(l)).count(), 1, "the changed checkout is read once: {after_commit:?}");
    assert_eq!(after_commit.len(), 2, "and nothing else asks git: {after_commit:?}");
    let counted: u64 = f.effects.world.landed_on_refs((warm + HOUR + 3 * MINUTE) / 1000, "sp").unwrap().iter().find(|r| r.repo == "spira").map(|r| r.hourly.iter().sum()).unwrap();
    assert_eq!(counted, 2, "landed_on_refs counts the new landing");
    let after_branch = &all[at_commit..at_branch];
    assert!(after_branch.iter().any(|l| l.contains(&format!("-C {pr} ")) && l.contains(" fetch ")), "a spira/* branch makes landing fetch: {after_branch:?}");
    })
    .unwrap();
}
#[test_id(tbsxtno)]
#[test]
fn the_sweeps_in_process_reads_reach_its_snapshot() {
    let f = fixture(testkit::env);
    spira_config::process::with_fresh(|| {
        let rune = spira_config::process::cfg("SPIRA_RUNES").unwrap().split_whitespace().next().expect("a rune").to_string();
        let idle: String = (0..5).map(|i| format!("2026-01-01T00:00:0{i}Z awake {rune} idle\n")).collect();
        let run = process_run();
        std::fs::write(run.join("dwarf-ledger.log"), idle).unwrap();
        std::fs::create_dir_all(run.join("release")).unwrap();
        let sha = "a".repeat(40);
        std::fs::write(run.join("release/hotfix"), format!("sha {sha}\nreason urgent\nat 2020-01-01T00:00:00Z\n")).unwrap();
        let bead = format!("{{\"bead_id\":\"sp-zzzzzz\",\"verdict\":\"claimable\",\"runes\":[\"{rune}\"],\"row\":{},\"content\":{}}}", lc_row("sp-zzzzzz"), plan_row("sp-zzzzzz"));
        *f.claimable.lock().unwrap() = format!("{{\"beads\":[{bead}]}}");
        let observe = spira_runtime::tasks::d::watch_tower::OBSERVE;
        let out = runtime::host::EffectRunner::run(f.effects.as_ref(), "k", &runtime::EffectSpec::keyed(observe, "k", String::new(), 60_000)).expect("the sweep ran");
        let snap: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(snap["hotfix"].as_str().is_some_and(|h| h.contains(&format!("RUNNING UNLANDED {sha}: urgent"))), "release status: {}", snap["hotfix"]);
        let references: Vec<&str> = snap["escalations"].as_array().unwrap().iter().filter_map(|e| e["reference"].as_str()).collect();
        assert!(references.contains(&format!("incident:idle-while-ready:{rune}").as_str()), "ready by rune: {references:?}");
    })
    .unwrap();
}

#[test_id(trx3p6f)]
#[test]
fn a_podman_that_fails_to_list_images_is_asked_again_on_the_next_pass() {
    let _env = testkit::env_read();
    let dir = testkit::TempDir::new("untagged-failing");
    let root = dir.join("graph");
    std::fs::create_dir_all(root.join("overlay-images")).unwrap();
    std::fs::write(root.join("overlay-images/images.json"), "[]").unwrap();
    let asked = dir.join("asked");
    let podman = dir.join("podman");
    testkit::write_exe(&podman, &format!("#!/bin/sh\ncase \"$1\" in info) echo {};; *) echo \"$1\" >> {}; exit 1;; esac\n", root.display(), asked.display()));
    let seams = reconciler::Seams { podman: podman.display().to_string(), ..reconciler::Seams::bare() };
    let cfg = reconciler::Config::from_lookup(&|_| Ok("1".into()), seams, dir.join("run"), dir.path().display().to_string(), 0, String::new()).unwrap();
    for _ in 0..2 {
        let checks = reconciler::observe_untagged_images(&cfg);
        assert!(format!("{:?}", checks[0].raw).contains("Unobservable"), "{:?}", checks[0].raw);
    }
    assert_eq!(std::fs::read_to_string(&asked).unwrap().lines().count(), 2, "a failed listing is not held");
}

#[test_id(t2wpptx)]
#[test]
fn a_store_with_no_images_file_asks_podman_on_every_pass() {
    let _env = testkit::env_read();
    let dir = testkit::TempDir::new("untagged-no-file");
    let root = dir.join("graph");
    std::fs::create_dir_all(&root).unwrap();
    let asked = dir.join("asked");
    let podman = dir.join("podman");
    testkit::write_exe(&podman, &format!("#!/bin/sh\ncase \"$1\" in info) echo {};; *) echo \"$1\" >> {}; echo sha;; esac\n", root.display(), asked.display()));
    let seams = reconciler::Seams { podman: podman.display().to_string(), ..reconciler::Seams::bare() };
    let cfg = reconciler::Config::from_lookup(&|_| Ok("1".into()), seams, dir.join("run"), dir.path().display().to_string(), 0, String::new()).unwrap();
    for _ in 0..2 {
        reconciler::observe_untagged_images(&cfg);
    }
    assert_eq!(std::fs::read_to_string(&asked).unwrap().lines().count(), 2, "with no images.json there is nothing to key a held count on");
}

#[test_id(t0ikhij)]
#[test]
fn an_idle_hour_of_the_cockpit_drift_probe_starts_no_drift_script_and_reads_a_dirtied_checkout_once() {
    let mut f = fixture(testkit::env);
    f.cockpit_input = COCKPIT_DRIFT;
    spira_config::process::with_fresh(|| {
    assert_traps_and_double_are_live(&f);
    assert_stubs_log(&f);
    let warm = START + 11 * MINUTE;
    let idle = warm + HOUR;
    let statuses = |f: &Fixture| logged(f).iter().filter(|l| l.ends_with(&format!("-C {} status --porcelain", f.local.display()))).count();
    let drift_scripts = |f: &Fixture| std::fs::read_to_string(&f.started).unwrap_or_default().lines().filter(|l| l.starts_with("bash ") && l.contains("drift.sh")).count();
    let fragment = |f: &Fixture| {
        let out: serde_json::Value = serde_json::from_str(&last_output(f, "cockpit-tier-600s.observe")).unwrap_or_default();
        out["fragments"]["cockpit.drift"]["kv"].to_string()
    };
    let path = spira_config::release_env::child_path_env_for_process().unwrap();
    std::process::Command::new("bash").envs(path).args(["drift.sh", "checkout"]).stdout(std::process::Stdio::null()).status().unwrap();
    assert_eq!(drift_scripts(&f), 1, "a bash start naming drift.sh is seen, so a count of zero means none ran");
    let (mut statuses_at_warm, mut statuses_at_idle, mut scripts_at_warm, mut scripts_at_idle) = (0, 0, 0, 0);
    let (mut at_warm, mut at_idle) = (String::new(), String::new());
    run_hours(&f, &["cockpit-tier-600s.observe"], (&[], 0), &[warm, idle, idle + 11 * MINUTE], &mut |t| {
        if t == warm {
            (statuses_at_warm, scripts_at_warm, at_warm) = (statuses(&f), drift_scripts(&f), fragment(&f));
        } else if t == idle {
            (statuses_at_idle, scripts_at_idle, at_idle) = (statuses(&f), drift_scripts(&f), fragment(&f));
            std::fs::write(f.local.join("stray"), "stray").unwrap();
        }
    });
    assert_eq!(scripts_at_idle - scripts_at_warm, 0, "an idle hour started drift.sh");
    let passes = f.seen.lock().unwrap().len();
    assert!(passes >= 7, "the drift probe was observed {passes} times, so the idle hour proves nothing");
    assert_eq!(statuses_at_warm, 1, "the warm-up reads the checkout, so the idle hour proves something: {:?}", logged(&f));
    assert_eq!(statuses_at_idle, statuses_at_warm, "an idle hour ran git status: {:?}", logged(&f));
    assert!(at_warm.contains(r#""SP_CHECKOUT_DRIFT":"OK""#) && at_idle == at_warm, "{at_warm} then {at_idle}");
    assert_eq!(statuses(&f), statuses_at_idle + 1, "a new untracked file is one git status: {:?}", logged(&f));
    assert!(fragment(&f).contains(r#""SP_CHECKOUT_DRIFT":"DIRTY:1""#), "{}", fragment(&f));
    })
    .unwrap();
}

fn conditions_check_refusal(f: &Fixture) -> String {
    let snap: serde_json::Value = serde_json::from_str(&last_output(f, "sentinel-pass.observe")).unwrap_or_default();
    snap["tower"].as_array().into_iter().flatten().filter_map(|t| t.as_str()).find(|t| t.starts_with("watchtower: conditions-check")).unwrap_or("no conditions-check entry in the last sentinel pass").to_string()
}

#[test_id(tnd8kl4)]
#[test]
fn the_livelock_probe_starts_no_bash_over_an_idle_hour_and_still_reports_an_ask_without_overseer() {
    let mut f = fixture(testkit::env);
    f.cockpit_input = COCKPIT_LIVELOCK;
    spira_config::process::with_fresh(|| {
    assert_traps_and_double_are_live(&f);
    assert_stubs_log(&f);
    let extra = spira_config::release_env::child_path_env_for_process().unwrap();
    std::process::Command::new("bash").args(["-c", "true"]).envs(extra).output().unwrap();
    assert_eq!(started(&f, "bash"), 1, "a bash started the way a probe starts one is counted");
    let lib = std::path::PathBuf::from(std::env::var("SPIRA_HOME").unwrap()).join("lib.sh");
    assert!(lib.is_file(), "no lib.sh at SPIRA_HOME, so a probe that sources it would be skipped: {}", lib.display());
    let ask = spira_config::process::cfg("SPIRA_ASK_LABEL").unwrap();
    let (warm, idle, asked) = (START + HOUR, START + 2 * HOUR, START + 2 * HOUR + 11 * MINUTE);
    let (mut at_warm, mut at_idle) = (0, 0);
    let observed = |f: &Fixture| f.seen.lock().unwrap().len();
    let (mut seen_at_warm, mut seen_at_idle) = (0, 0);
    run_hours(&f, &["cockpit-tier-600s.observe"], (&[], 0), &[warm, idle, asked], &mut |t| match t {
        t if t == warm => (at_warm, seen_at_warm) = (started(&f, "bash"), observed(&f)),
        t if t == idle => {
            (at_idle, seen_at_idle) = (started(&f, "bash"), observed(&f));
            let rows = format!(r#"[{{"id":"sp-zzzzzz","title":"q","status":"open","labels":["{ask}"]}}]"#);
            *f.labelled.lock().unwrap() = Some((ask.clone(), rows));
        }
        _ => {}
    });
    assert_only_the_double_answered(&f);
    assert!(seen_at_idle - seen_at_warm >= 5, "the probe ran {} times in the idle hour", seen_at_idle - seen_at_warm);
    assert_eq!(at_idle - at_warm, 0, "an idle virtual hour started bash");
    let out: serde_json::Value = serde_json::from_str(&last_output(&f, "cockpit-tier-600s.observe")).unwrap();
    let kv = out["fragments"]["cockpit.livelock"]["kv"].to_string();
    assert!(kv.contains("SP_LIVELOCK0") && kv.contains("LIVELOCK sp-zzzzzz ask-no-overseer"), "{kv}");
    })
    .unwrap();
}

fn age(tree: &std::path::Path, by: std::time::Duration) {
    let old = std::time::SystemTime::now() - by;
    let dotgit = std::fs::read_to_string(tree.join(".git")).unwrap();
    let admin = std::path::PathBuf::from(dotgit.trim().strip_prefix("gitdir:").unwrap().trim());
    for p in [admin.join("HEAD"), admin.join("index"), tree.to_path_buf()] {
        std::fs::File::open(&p).unwrap().set_modified(old).unwrap();
    }
}

#[test_id(tx3aih0)]
#[test]
fn the_stale_reaper_asks_git_only_about_a_tree_its_cheap_rules_leave_undecided() {
    let f = fixture(testkit::env);
    spira_config::process::with_fresh(|| {
    use spira_runtime::tasks::c::hel_reap_stale as task;
    let names = ["hel.reap_stale.observe"];
    let root = f.effects.world.run_dir.join("worktree");
    std::fs::create_dir_all(&root).unwrap();
    git(&f.local, &["worktree", "add", "-q", "--detach", root.join(".testbatch-1").to_str().unwrap()], 1);
    for id in ["sp-young01", "sp-done01", "sp-open01", "sp-merged01"] {
        git(&f.local, &["worktree", "add", "-q", "-b", &format!("spira/{id}"), root.join(id).to_str().unwrap(), "local/main"], 1);
    }
    commit(&root.join("sp-open01"), "sp-open01 unmerged work", LANDED_S + 5);
    for id in ["sp-done01", "sp-open01", "sp-merged01"] {
        age(&root.join(id), std::time::Duration::from_secs(3 * 3600));
    }
    *f.checkout_rows.lock().unwrap() = [("sp-young01", "READY"), ("sp-done01", "LANDED"), ("sp-open01", "READY"), ("sp-merged01", "READY")].map(|(i, s)| (i.to_string(), s.to_string())).to_vec();
    let marks = [START + 30 * MINUTE, START + 90 * MINUTE, START + 150 * MINUTE];
    let passes = || f.seen.lock().unwrap().iter().filter(|(n, _)| n == names[0]).count();
    let mut at = Vec::new();
    let (mut snapshot, mut decided_snapshot) = (String::new(), String::new());
    run_hours(&f, &names, (&[], 0), &marks, &mut |t| {
        at.push((logged(&f).len(), passes()));
        if t == marks[1] {
            let lc = spira_store::lc_call::LcClient::configured().unwrap();
            let real = task::world(&f.effects.world.run_dir, f.release.to_str().unwrap(), &lc).unwrap();
            snapshot = task::observe(&real, &lc, &f.proc_root).unwrap();
            let after_first = logged(&f).len();
            for id in ["sp-open01", "sp-merged01"] {
                git(&f.local, &["worktree", "remove", "--force", root.join(id).to_str().unwrap()], 1);
            }
            let detached = root.join("sp-detach01");
            git(&f.local, &["worktree", "add", "-q", "--detach", detached.to_str().unwrap(), "local/main"], 1);
            age(&detached, std::time::Duration::from_secs(3 * 3600));
            f.checkout_rows.lock().unwrap().push(("sp-detach01".into(), "READY".into()));
            decided_snapshot = task::observe(&real, &lc, &f.proc_root).unwrap();
            at.push((after_first, logged(&f).len()));
        }
    });
    let all = logged(&f);
    let (first_at, second_at, direct, third_at) = (at[0], at[1], at[2], at[3]);
    assert_eq!((first_at.1, second_at.1, third_at.1), (1, 2, 3), "one pass ends at each mark: {at:?}");
    let first = &all[..first_at.0];
    let second = &all[first_at.0..second_at.0];
    let variant = &all[direct.1..third_at.0];
    let report = |lines: &[String]| format!("{}\n{}", argv_counts(lines), lines.join("\n"));
    assert!(!first.iter().any(|l| l.contains("merge-base")), "the pass asked merge-base: {}", report(first));
    assert_eq!(second.len(), first.len(), "a pass with nothing moved starts as many git processes as the first:\n{}\n---\n{}", report(first), report(second));
    let landrefs = ["local/main", "main"];
    let local = f.local.display().to_string();
    let list = |repo: &str| format!("-C {repo} worktree list --porcelain");
    let merge_read = |base: &str| format!("-C {local} for-each-ref --merged={base} --format=%(refname:short) refs/heads");
    assert!(first.iter().any(|l| l.ends_with(&merge_read("local/main"))), "an undecided tree is read: {}", report(first));
    for b in landrefs {
        assert!(first.iter().filter(|l| l.ends_with(&merge_read(b))).count() <= 1, "{b} is read once: {}", report(first));
    }
    let repos = [local.clone(), f.pr.display().to_string()];
    assert!(first.iter().all(|l| repos.iter().any(|r| l.ends_with(&list(r))) || landrefs.iter().any(|b| l.ends_with(&merge_read(b)))), "{}", report(first));
    assert_eq!(variant.len(), repos.len(), "with no undecided tree: {}", report(variant));
    for r in &repos {
        for lines in [first, variant] {
            assert_eq!(lines.iter().filter(|l| l.ends_with(&list(r))).count(), 1, "one worktree list of {r}: {}", report(lines));
        }
    }

    let snap: task::Snapshot = serde_json::from_str(&snapshot).unwrap();
    let tree = |id: &str| snap.repos.iter().flat_map(|r| &r.worktrees).find(|t| t.id == id).unwrap_or_else(|| panic!("{id} not scanned: {snapshot}"));
    assert_eq!(tree("sp-merged01").merged_into.as_deref(), Some("local/main"));
    assert_eq!(tree("sp-open01").merged_into, None);
    assert_eq!(tree("sp-done01").bead_state.as_deref(), Some("LANDED"));
    let resolved = |json: &str| -> std::collections::BTreeMap<String, (usize, Vec<String>)> {
        serde_json::from_str::<task::Snapshot>(json).unwrap().repos.into_iter().map(|r| (r.name, (r.worktrees.len(), r.landrefs))).collect()
    };
    let refs: Vec<String> = landrefs.map(String::from).to_vec();
    assert_eq!(resolved(&snapshot)["spira"], (4, refs));
    assert_eq!(resolved(&snapshot)["prrepo"], (0, vec![]), "a repository with no tree resolves no land ref");
    assert_eq!(resolved(&decided_snapshot)["spira"], (3, vec![]), "nor does one whose trees are decided, a detached one included");
    use spira_runtime::tasks::b::kit::{testing, Input};
    let handler = testing::handler::<task::HelReapStale>(&[(task::CADENCE_KEY, "3600"), (task::IDLE_HOURS_KEY, "48")]);
    let (_, acts) = task::decide(&handler.cfg, &task::Carry { phase: task::Phase::Observing }, &Input::Observed(snap), START).unwrap();
    let mut destroyed: Vec<(String, String)> = acts.iter().map(|a| {
        assert_eq!(a.name, task::DESTROY_WORKTREE);
        let d: task::Destroy = serde_json::from_str(&a.input).unwrap();
        (d.id, d.why)
    }).collect();
    destroyed.sort();
    assert_eq!(destroyed, [("sp-done01".to_string(), "reaper: bead is LANDED".to_string()), ("sp-merged01".to_string(), "reaper: branch is merged into local/main".to_string())]);
    })
    .unwrap();
}
