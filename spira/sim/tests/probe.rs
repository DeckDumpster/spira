use serde_json::{json, Value};
use spira_sim::probe::{git_facts, landstate_of, names, parse_branches, probe, refuse_foreign, rows, GitFacts, Lineage};
use spira_sim::trace::{bead_row, violations, Snapshot, Trace};
use spira_sim::world::PRODUCTION_LOCATORS;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C").arg(repo).args(args)
        .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t.invalid")
        .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t.invalid")
        .output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

fn commit(work: &Path, file: &str, msg: &str) -> String {
    std::fs::write(work.join(file), msg).unwrap();
    git(work, &["add", file]);
    git(work, &["commit", "-q", "-m", msg]);
    git(work, &["rev-parse", "HEAD"])
}

/// The beads' git, as the fixture world holds it.
struct Shas {
    landed: String,
    certified: String,
    moved: String,
    unpublished: String,
}

/// A world on disk without a store: marker, work repo with `local/main` and `spira/<id>`
/// branches, a bare origin, a fixture with `server.port`, and a stub `spira-lc` that prints
/// `list_json` and records the environment it was given.
fn world(list_json: &str) -> (testkit::TempDir, Shas) {
    let w = testkit::TempDir::new("simprobe");
    std::fs::write(w.join(".sim-world"), "").unwrap();
    let work = w.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "--initial-branch=main"]);
    commit(&work, "seed", "sim seed");
    git(w.path(), &["init", "-q", "--bare", "--initial-branch=main", "origin.git"]);
    git(&work, &["remote", "add", "origin", &w.join("origin.git").display().to_string()]);
    git(&work, &["push", "-q", "origin", "main"]);

    // sp-l: built on spira/sp-l, landed onto local/main by a rebased commit naming it.
    git(&work, &["checkout", "-q", "-b", "spira/sp-l", "main"]);
    let landed = commit(&work, "l", "sp-l: the landed change");
    git(&work, &["checkout", "-q", "-b", "local/main", "main"]);
    commit(&work, "l2", "sp-l: the landed change (rebased)");
    // sp-c: certified at its branch tip. sp-m: branch moved after certification.
    git(&work, &["checkout", "-q", "-b", "spira/sp-c", "main"]);
    let certified = commit(&work, "c", "sp-c: work");
    git(&work, &["checkout", "-q", "-b", "spira/sp-m", "main"]);
    commit(&work, "m", "sp-m: work");
    let moved = commit(&work, "m2", "sp-m: more work");
    // sp-u: a branch the lifecycle has never heard of; sp-l.1 names a child, not sp-l.
    git(&work, &["checkout", "-q", "-b", "spira/sp-u", "main"]);
    let unpublished = commit(&work, "u", "sp-u: draft, see sp-l.1");
    git(&work, &["checkout", "-q", "main"]);

    let fx = w.join("db/fx");
    std::fs::create_dir_all(&fx).unwrap();
    std::fs::write(fx.join("server.port"), "40123\n").unwrap();
    std::fs::write(w.join("db.fixture"), fx.display().to_string()).unwrap();
    std::fs::create_dir_all(w.join("config")).unwrap();
    std::fs::create_dir_all(w.join("run/landstate")).unwrap();
    let bin = w.join("release/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(w.join("list.json"), list_json).unwrap();
    let log = w.join("lc.env").display().to_string();
    testkit::write_exe(bin.join("spira-lc"), &format!("#!/bin/sh\nenv > '{log}'\necho \"$@\" >> '{log}'\ncat '{}'\n", w.join("list.json").display()));
    (w, Shas { landed, certified, moved, unpublished })
}

fn clean(_: &str) -> Option<String> {
    None
}

fn by_bead(out: &str) -> BTreeMap<String, Value> {
    out.lines().map(|l| bead_row(&serde_json::from_str(l).unwrap()).unwrap()).map(|r| (r["bead"].as_str().unwrap().to_string(), r)).collect()
}

/// A lifecycle that agrees with the fixture's git.
fn clean_list(s: &Shas) -> String {
    json!([
        {"bead_id": "sp-l", "state": "LANDED", "tip": s.landed, "version": "7", "holds": "[]"},
        {"bead_id": "sp-c", "state": "CERTIFIED", "tip": s.certified, "version": 4, "holds": ["wait", "poison"]},
        {"bead_id": "sp-m", "state": "SUBMITTED", "tip": s.moved, "version": 2, "holds": null},
        {"bead_id": "sp-r", "state": "READY", "tip": null, "version": 1, "holds": "[]"}
    ])
    .to_string()
}

#[test]
fn names_matches_whole_bead_ids_only() {
    assert!(names("sp-l: x", "sp-l"));
    assert!(names("fix (sp-l).", "sp-l"));
    assert!(names("see sp-l.", "sp-l"));
    assert!(!names("sp-l.1: child", "sp-l"));
    assert!(!names("sp-lx: other", "sp-l"));
    assert!(!names("xsp-l", "sp-l"));
    assert!(!names("sp-l-2", "sp-l"));
}

#[test]
fn lineage_and_branch_parsing() {
    let l = Lineage::parse("aaa\0sp-a: one\n\n\x1e\nbbb\0seed\n\x1e\n");
    assert!(l.shas.contains("aaa") && l.shas.contains("bbb"));
    assert!(l.holds("sp-a", None));
    assert!(l.holds("sp-z", Some("bbb")));
    assert!(!l.holds("sp-z", Some("ccc")));
    let b = parse_branches("refs/heads/spira/sp-a 111\nrefs/heads/main 222\nrefs/heads/spira/sp-b.1 333\n");
    assert_eq!(b.len(), 2);
    assert_eq!(b["sp-a"], "111");
    assert_eq!(b["sp-b.1"], "333");
}

#[test]
fn rows_from_fixture_lifecycle_json_and_git() {
    let (w, s) = world("[]");
    std::fs::write(w.join("run/landstate/sp-c"), format!("CERTIFIED {} 1800000000\n", s.certified)).unwrap();
    let facts = git_facts(&w).unwrap();
    let ls = landstate_of(&w);
    let rows: BTreeMap<String, Value> = rows(&clean_list(&s), &ls, &facts).unwrap().into_iter().map(|r| (r["bead"].as_str().unwrap().to_string(), bead_row(&r).unwrap())).collect();
    assert_eq!(rows.keys().cloned().collect::<Vec<_>>(), ["sp-c", "sp-l", "sp-m", "sp-r", "sp-u"]);

    let l = &rows["sp-l"];
    assert_eq!((l["lc_state"].as_str(), l["lc_version"].as_i64(), l["holds"].as_str()), (Some("LANDED"), Some(7), Some("")));
    assert_eq!(l["branch_tip"], s.landed);
    assert_eq!(l["on_local_main"], true, "a rebased commit naming the bead is on local/main");
    assert_eq!(l["on_origin_main"], false, "nothing was published");
    assert_eq!(l["landstate"], Value::Null);

    let c = &rows["sp-c"];
    assert_eq!((c["lc_tip"].as_str(), c["branch_tip"].as_str()), (Some(s.certified.as_str()), Some(s.certified.as_str())));
    assert_eq!(c["holds"], "poison,wait");
    assert_eq!(c["landstate"], "CERTIFIED");
    assert_eq!(c["on_local_main"], false);

    assert_eq!(rows["sp-m"]["lc_version"], 2);
    assert_eq!(rows["sp-m"]["holds"], "");
    let r = &rows["sp-r"];
    assert_eq!((r["lc_tip"].clone(), r["branch_tip"].clone()), (Value::Null, Value::Null));

    let u = &rows["sp-u"];
    assert_eq!((u["lc_state"].clone(), u["holds"].clone()), (Value::Null, Value::Null));
    assert_eq!(u["branch_tip"], s.unpublished);
    assert_eq!(u["on_local_main"], false);
    assert!(rows.values().all(|r| r["bd_status"].is_null()));

    // Publishing local/main puts sp-l on origin.
    git(&w.join("work"), &["push", "-q", "origin", "local/main:main"]);
    let facts = git_facts(&w).unwrap();
    let again = rows_of(&clean_list(&s), &facts);
    assert_eq!(again["sp-l"]["on_origin_main"], true);
}

fn rows_of(list: &str, facts: &GitFacts) -> BTreeMap<String, Value> {
    rows(list, &|_| None, facts).unwrap().into_iter().map(|r| (r["bead"].as_str().unwrap().to_string(), r)).collect()
}

#[test]
fn rows_refuse_unparseable_lifecycle_output() {
    let g = GitFacts::default();
    assert!(rows("cannot tell", &|_| None, &g).is_err());
    assert!(rows("{}", &|_| None, &g).is_err());
    assert!(rows(r#"[{"state":"READY"}]"#, &|_| None, &g).is_err());
    assert!(rows(r#"[{"bead_id":"sp-a","version":"x"}]"#, &|_| None, &g).is_err());
}

#[test]
fn probe_runs_the_worlds_spira_lc_in_a_cleared_environment() {
    let (w, s) = world("");
    std::fs::write(w.join("list.json"), clean_list(&s)).unwrap();
    let env = |k: &str| (k == "SPIRA_RUN").then(|| w.join("run").display().to_string());
    let out = probe(&w, &env).unwrap();
    assert_eq!(by_bead(&out).len(), 5);
    let lc_env = std::fs::read_to_string(w.join("lc.env")).unwrap();
    assert!(lc_env.contains("SPIRA_LC_PORT=40123\n"), "{lc_env}");
    assert!(lc_env.contains(&format!("SPIRA_TOML={}\n", w.join("config/lc.toml").display())), "{lc_env}");
    assert!(lc_env.lines().last() == Some("list"), "{lc_env}");
    for k in PRODUCTION_LOCATORS {
        assert!(!lc_env.contains(&format!("{k}=")), "{k} leaked into spira-lc's environment");
    }
}

#[test]
fn probe_refuses_each_production_locator_and_runs_nothing() {
    for key in PRODUCTION_LOCATORS {
        let (w, s) = world("");
        std::fs::write(w.join("list.json"), clean_list(&s)).unwrap();
        let env = |k: &str| (k == *key).then(|| "/prod/x".to_string());
        let e = probe(&w, &env).unwrap_err();
        assert!(e.contains(key), "{e}");
        assert!(!w.join("lc.env").exists(), "{key}: spira-lc ran despite the refusal");
    }
}

#[test]
fn probe_refuses_a_directory_that_is_not_a_world() {
    let d = testkit::TempDir::new("simprobe-not");
    assert!(refuse_foreign(&d, &clean).unwrap_err().contains("not a sim world"));
    let (w, _) = world("[]");
    assert!(refuse_foreign(&w, &|k| (k == "SPIRA_RUN").then(|| w.join("run").display().to_string())).is_ok());
    assert!(refuse_foreign(&w, &|k| (k == "SPIRA_RUN").then(|| format!("{}/../elsewhere", w.display()))).is_err());
}

fn hits(beads: Vec<Value>) -> Vec<(String, String)> {
    let d = testkit::TempDir::new("simprobe-trace");
    let t = Trace::create(&d).unwrap();
    t.snapshot(1, &Snapshot { beads, ..Default::default() }).unwrap();
    let db = t.load(&d).unwrap();
    let mut v: Vec<(String, String)> = violations(&db, &[])
        .unwrap()
        .into_iter()
        .map(|v| (v.view, serde_json::from_str::<Value>(&v.row).unwrap()["bead"].as_str().unwrap_or("").to_string()))
        .collect();
    v.sort();
    v
}

/// law-absence-needs-a-positive-control: the views are empty on the probe's clean rows only
/// because the probe can also produce rows that trip them.
#[test]
fn probe_rows_trip_the_matching_views_and_a_clean_set_trips_none() {
    if !spira_sim::trace::duckdb_available() {
        return;
    }
    let (w, s) = world("");
    std::fs::write(w.join("list.json"), clean_list(&s)).unwrap();
    std::fs::write(w.join("run/landstate/sp-c"), format!("CERTIFIED {} 1800000000\n", s.certified)).unwrap();
    let env = |k: &str| (k == "SPIRA_RUN").then(|| w.join("run").display().to_string());
    let clean_rows: Vec<Value> = by_bead(&probe(&w, &env).unwrap()).into_values().collect();
    assert_eq!(hits(clean_rows), vec![]);

    // Planted: sp-m's lifecycle tip is the pre-move commit; sp-u is LANDED but nothing on
    // local/main names it; sp-r's landstate says CERTIFIED while its lifecycle says READY.
    let stale = git(&w.join("work"), &["rev-parse", "spira/sp-m~1"]);
    let bad = json!([
        {"bead_id": "sp-l", "state": "LANDED", "tip": s.landed, "version": 7, "holds": "[]"},
        {"bead_id": "sp-c", "state": "CERTIFIED", "tip": s.certified, "version": 4, "holds": "[]"},
        {"bead_id": "sp-m", "state": "SUBMITTED", "tip": stale, "version": 2, "holds": "[]"},
        {"bead_id": "sp-r", "state": "READY", "tip": null, "version": 1, "holds": "[]"},
        {"bead_id": "sp-u", "state": "LANDED", "tip": s.unpublished, "version": 9, "holds": "[]"}
    ]);
    std::fs::write(w.join("list.json"), bad.to_string()).unwrap();
    std::fs::write(w.join("run/landstate/sp-r"), "CERTIFIED none 1800000000\n").unwrap();
    let bad_rows: Vec<Value> = by_bead(&probe(&w, &env).unwrap()).into_values().collect();
    let want = |v: &str, b: &str| (v.to_string(), b.to_string());
    assert_eq!(
        hits(bad_rows),
        vec![
            want("inv_landed_is_on_local_main", "sp-u"),
            want("inv_landstate_certified_needs_lifecycle", "sp-r"),
            want("inv_tip_matches_branch", "sp-m"),
        ]
    );
}
