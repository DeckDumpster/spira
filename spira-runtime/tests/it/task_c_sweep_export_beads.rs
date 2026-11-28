//! T1–T6 for `sweep.export_beads`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/sweep-export-beads.md`. Red until the task is ported.

use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::c::sweep_export_beads::{self as task, Carry, Phase, Snapshot, SweepExportBeads as H};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "21600"), ("SPIRA_EXPORT_GIT_RETRY_WAIT", "10")];
const TICK: u64 = 21_600_000;

fn dump(ids: &[&str], mirrored_count: usize, mirror_sha: &str) -> Snapshot {
    Snapshot { content: ids.iter().map(|id| serde_json::json!({"id": id, "title": format!("bead {id}")})).collect(), mirrored_count, mirror_bytes_sha: mirror_sha.into(), ..Snapshot::default() }
}

/// A dump whose render is byte-equal to what is mirrored now: the probe reports the sha256 of the
/// mirror's bytes, so an unchanged mirror carries the sha of this very render.
fn current(ids: &[&str], mirrored_count: usize) -> Snapshot {
    let mut s = dump(ids, mirrored_count, "");
    s.mirror_bytes_sha = task::sha_hex(task::render(&s).unwrap().as_bytes());
    s
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test_id(tsex5s5)]
#[test]
fn t1_registered_boots_and_arms_a_6h_tick() {
    t::t1_registered_and_boots::<H>(SET, TICK);
}

#[test_id(tga32yt)]
#[test]
fn t2_a_render_identical_to_the_mirror_touches_and_commits_nothing() {
    let h = t::handler::<H>(SET);
    let unchanged = current(&["sp-a", "sp-b"], 2);
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(unchanged.clone()), TICK).unwrap();
    assert_eq!(names(&acts), [task::TOUCH], "an unchanged pass bumps the mirror's mtime only");
    let mut rig = t::rig::<H>(1, SET, &json(&unchanged));
    t::assert_ported(&rig, task::STREAM);
    rig.advance(TICK).unwrap();
    assert!(rig.requested().iter().all(|q| q.name == task::OBSERVE || q.name == task::TOUCH), "{:?}", rig.requested());
}

#[test_id(tukzr9y)]
#[test]
fn t3_rendered_written_committed_published_and_publish_before_committed_is_refused() {
    let h = t::handler::<H>(SET);
    let at = |phase| Carry { phase, sha: Some("s1".into()), refused: None };
    let ok = |e: &str| Input::Outcome { effect: e.into(), ok: true, body: "s1".into() };
    let (c, acts) = task::decide(&h.cfg, &at(Phase::Rendered), &ok(task::WRITE), TICK).unwrap();
    assert_eq!((c.phase, names(&acts)), (Phase::Written, vec![task::COMMIT]));
    let (c, acts) = task::decide(&h.cfg, &c, &ok(task::COMMIT), TICK).unwrap();
    assert_eq!((c.phase, names(&acts)), (Phase::Committed, vec![task::PUBLISH]));
    let (c, _) = task::decide(&h.cfg, &c, &ok(task::PUBLISH), TICK).unwrap();
    assert_eq!(c.phase, Phase::Published);
    let refused = task::decide(&h.cfg, &at(Phase::Written), &ok(task::PUBLISH), TICK).unwrap_err();
    assert!(refused.contains("written"), "{refused}");
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(dump(&[], 10, "x")), TICK).unwrap();
    assert_eq!((c.phase, c.refused.as_deref(), acts.len()), (Phase::Refused, Some("no-sp"), 0));
}

#[test_id(t2yxn2b)]
#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_no_spira_lc_or_python3() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/c/sweep_export_beads.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test_id(t24uery)]
#[test]
fn t5_a_changed_dump_writes_sorted_by_id_and_a_half_shrink_refuses() {
    let h = t::handler::<H>(SET);
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(dump(&["sp-b", "sp-a", "sp-c"], 2, "old")), TICK).unwrap();
    assert_eq!(names(&acts), [task::WRITE]);
    let lines: Vec<&str> = acts[0].input.lines().filter(|l| l.contains("\"id\"")).collect();
    assert!(lines.len() == 3 && lines[0].contains("sp-a") && lines[2].contains("sp-c"), "sorted by id: {lines:?}");
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(dump(&["sp-a"], 10, "old")), TICK).unwrap();
    assert_eq!((c.refused.as_deref(), acts.len()), (Some("shrink"), 0));
}

struct Store(std::sync::Mutex<Vec<Vec<String>>>, usize);

impl spira_store::lc_call::Transport for Store {
    fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
        self.0.lock().unwrap().push(argv.to_vec());
        let out = match argv.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            ["list", "--bare", rest @ ..] => {
                let (after, limit) = match rest {
                    ["--after", a, "--limit", l] => (Some(*a), l.parse::<usize>().unwrap()),
                    ["--limit", l] => (None, l.parse::<usize>().unwrap()),
                    other => panic!("unexpected list args {other:?}"),
                };
                let rows: Vec<String> = (0..self.1).map(|n| format!("sp-{n:05}")).filter(|id| after.is_none_or(|a| id.as_str() > a)).take(limit).map(|id| format!("{{\"bead_id\":\"{id}\",\"state\":\"OPEN\"}}")).collect();
                format!("[{}]", rows.join(","))
            }
            ["content", "list", "--id", ids, ..] => format!("[{}]", ids.split(',').map(|i| format!("{{\"id\":\"{i}\"}}")).collect::<Vec<_>>().join(",")),
            ["content", "memories", ..] => "{}".into(),
            _ => String::new(),
        };
        Ok(spira_store::lc_call::Reply { code: 0, out, err: String::new(), read_ended: false })
    }
}

#[test_id(t8lv1ej)]
#[test]
fn the_probe_reads_content_in_bounded_id_chunks_never_one_whole_store_dump() {
    let wiki = testkit::TempDir::new("export-beads-chunks");
    let store = std::sync::Arc::new(Store(Default::default(), 2 * task::CONTENT_CHUNK + 1));
    let lc = spira_store::lc_call::LcClient::over(store.clone(), true);
    let snap: Snapshot = serde_json::from_str(&task::observe(&lc, wiki.path().to_str().unwrap()).unwrap()).unwrap();
    assert_eq!(snap.content.len(), 2 * task::CONTENT_CHUNK + 1, "every bead is still read");
    let content_lists: Vec<Vec<String>> = store.0.lock().unwrap().iter().filter(|a| a.starts_with(&["content".into(), "list".into()])).cloned().collect();
    assert_eq!(content_lists.len(), 3);
    for call in &content_lists {
        assert_eq!(call[2], "--id", "{call:?}");
        assert!(call[3].split(',').count() <= task::CONTENT_CHUNK, "a chunk is bounded");
    }
}

fn mirrored_wiki(tag: &str, lines: &[String]) -> testkit::TempDir {
    let wiki = testkit::TempDir::new(tag);
    let out = wiki.path().join(task::REL);
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    std::fs::write(out, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    wiki
}

fn bead_line(id: &str) -> String {
    format!("{{\"id\":\"{id}\",\"lifecycle_state\":\"\"}}")
}

#[test_id(ts0xo9x)]
#[test]
fn a_mirrored_bead_without_a_lifecycle_row_is_still_read() {
    let memory = r#"{"_type":"memory","key":"k","value":"v"}"#.to_string();
    let wiki = mirrored_wiki("export-beads-union", &[bead_line("sp-zz001"), bead_line("sp-zz002"), memory]);
    let store = std::sync::Arc::new(Store(Default::default(), 3));
    let lc = spira_store::lc_call::LcClient::over(store.clone(), true);
    let snap: Snapshot = serde_json::from_str(&task::observe(&lc, wiki.path().to_str().unwrap()).unwrap()).unwrap();
    let ids: Vec<&str> = snap.content.iter().map(|v| v["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["sp-00000", "sp-00001", "sp-00002", "sp-zz001", "sp-zz002"]);
    assert_eq!(snap.mirrored_count, 2);
    let calls = store.0.lock().unwrap().clone();
    assert!(calls.iter().filter(|a| a.starts_with(&["content".into(), "list".into()])).all(|a| a[3].split(',').count() <= task::CONTENT_CHUNK), "{calls:?}");
}

#[test_id(txfzk2t)]
#[test]
fn a_mirror_over_twice_the_lifecycle_store_is_rewritten_not_refused_as_a_shrink() {
    let n = task::CONTENT_CHUNK;
    let lines: Vec<String> = (0..2 * n + 1).map(|i| bead_line(&format!("sp-{i:05}"))).collect();
    let wiki = mirrored_wiki("export-beads-outgrown", &lines);
    let store = std::sync::Arc::new(Store(Default::default(), n));
    let lc = spira_store::lc_call::LcClient::over(store.clone(), true);
    let snap: Snapshot = serde_json::from_str(&task::observe(&lc, wiki.path().to_str().unwrap()).unwrap()).unwrap();
    let h = t::handler::<H>(SET);
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(snap.clone()), TICK).unwrap();
    assert_eq!((c.refused.as_deref(), names(&acts)), (None, vec![task::WRITE]));
    assert_eq!(snap.content.len(), 2 * n + 1, "a bead in both sources is read once");
    let calls = store.0.lock().unwrap().clone();
    assert!(calls.iter().filter(|a| a.starts_with(&["content".into(), "list".into()])).all(|a| a[3].split(',').count() <= n), "{calls:?}");
}

#[test_id(t7pjl81)]
#[test]
fn an_unreadable_mirror_fails_the_probe_instead_of_reading_as_empty() {
    let wiki = testkit::TempDir::new("export-beads-unreadable");
    std::fs::create_dir_all(wiki.path().join(task::REL)).unwrap();
    let lc = spira_store::lc_call::LcClient::over(std::sync::Arc::new(Store(Default::default(), 3)), true);
    let err = task::observe(&lc, wiki.path().to_str().unwrap()).unwrap_err();
    assert!(err.contains(task::REL), "{err}");
}

#[test_id(t82aaq3)]
#[test]
fn the_lifecycle_store_is_read_as_cursor_pages_never_as_list_all() {
    let wiki = testkit::TempDir::new("export-beads-pages");
    let store = std::sync::Arc::new(Store(Default::default(), 2 * 1000 + 1));
    let lc = spira_store::lc_call::LcClient::over(store.clone(), true);
    let snap: Snapshot = serde_json::from_str(&task::observe(&lc, wiki.path().to_str().unwrap()).unwrap()).unwrap();
    assert_eq!(snap.lifecycle.len(), 2001);
    let calls = store.0.lock().unwrap().clone();
    assert!(calls.iter().all(|a| a[0] != "list-all"), "{calls:?}");
    let lists: Vec<&Vec<String>> = calls.iter().filter(|a| a[0] == "list").collect();
    assert_eq!(lists.len(), 3, "{lists:?}");
    assert!(lists.iter().all(|a| a.ends_with(&["--limit".into(), "1000".into()])), "{lists:?}");
    assert_eq!(lists[0], &["list", "--bare", "--limit", "1000"]);
    assert_eq!(lists[1][2..4], ["--after".to_string(), "sp-00999".to_string()]);
    assert_eq!(lists[2][2..4], ["--after".to_string(), "sp-01999".to_string()]);
}

use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::tasks::b::kit::FIRE;

/// A render written to the mirror whose commit failed; the next world finds the mirror equal to it.
fn written_but_uncommitted() -> TaskRig {
    let ids = ["sp-a", "sp-b"];
    let mut r = TaskRig::new(1, t::handler::<H>(SET).registration())
        .unwrap()
        .double(task::OBSERVE, kit::cycling(vec![dump(&ids, 2, ""), current(&ids, 2)], 1))
        .double(task::WRITE, |text| Ok(task::sha_hex(text.as_bytes())))
        .double(task::COMMIT, |_| Err("index.lock exists".into()));
    let (_, stream, kind) = H::boot();
    r.tick(stream, kind).unwrap();
    r
}

#[test_id(tpgqmhk)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&written_but_uncommitted(), task::NAME);
    for key in ["phase", "sha", "refused"] {
        assert!(rows.iter().any(|k| k == key), "{key} in {rows:?}");
    }
}

#[test_id(tmxzknp)]
#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(written_but_uncommitted, || t::handler::<H>(SET).registration(), task::STREAM, runtime::event::EventKind::new(task::TICK, FIRE));
}
