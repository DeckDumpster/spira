use serde_json::{json, Value};
use spira_sim::probe::{merge_bd, names, parse_branches, rows, GitFacts, Lineage};
use std::collections::BTreeMap;
use std::path::Path;

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
fn bd_rows_refuse_what_is_not_a_bead_list() {
    for bad in ["", "{}", "[{\"title\":\"no id\"}]"] {
        assert!(merge_bd(&mut Vec::new(), bad).is_err(), "{bad}");
    }
}

#[test]
fn a_worlds_landing_lineage_is_its_origin_trunk_unless_its_own_repository_is_queue_local() {
    let t = testkit::TempDir::new("probe-mode");
    let w = t.join("world");
    std::fs::create_dir_all(w.join("home")).unwrap();
    assert!(!spira_sim::probe::lands_on_origin(&w), "no registry yet reads as the default, queue.local");
    for (row, on_origin) in [("sim|/w/work|queue.local|local/main|||plan", false), ("sim|/w/work|push|origin/main|||plan", true), ("sim|/w/work|hold|origin/main|||plan", true)] {
        std::fs::write(w.join("home/registry-rows"), format!("{row}\nother|/w/repos/o/work|queue.local|local/main|||plan\n")).unwrap();
        assert_eq!(spira_sim::probe::lands_on_origin(&w), on_origin, "{row}");
    }
}

#[test]
fn a_bead_of_another_repository_is_held_against_that_repositorys_lineage() {
    let mut other = Lineage::default();
    other.messages.push("sp-d: landed in docs".to_string());
    let mut facts = GitFacts::default();
    facts.others.insert("docs".to_string(), spira_sim::probe::RepoLineages { local_main: Lineage::default(), origin_main: other });
    let list = r#"[{"bead_id":"sp-d","state":"LANDED","version":"1","repo":"docs"},{"bead_id":"sp-h","state":"LANDED","version":"1","repo":"sim"}]"#;
    let got = rows_of(list, &facts);
    assert_eq!(got["sp-d"]["on_origin_main"], true);
    assert_eq!(got["sp-d"]["on_local_main"], false);
    assert_eq!(got["sp-h"]["on_origin_main"], false, "a bead of the home repository is not found in docs' history");
}

fn deliver(root: &Path, mailbox: &str, sub: &str, file: &str, text: &str) {
    let d = root.join(mailbox).join(sub);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(file), text).unwrap();
}

#[test]
fn mail_rows_name_each_message_by_mailbox_kind_bead_read_state_and_subject() {
    let d = testkit::TempDir::new("simprobe-mail");
    let root = d.join("mail");
    deliver(&root, "concierge", "new", "1.0.1", "From: a\nSubject: loop x3\nX-Spira-Kind: note\nX-Spira-Bead: sp-1\n\nbody\n");
    deliver(&root, "operator", "cur", "2.0.1:2,S", "Subject: folded\n subject\nX-Spira-Kind: question\n\nX-Spira-Bead: sp-body\n");
    deliver(&root, "operator", "cur", "3.0.1:2,R", "Subject: seen but not flagged S\n\nbody\n");
    let rows = spira_sim::probe::mail_rows(&root);
    assert_eq!(
        rows,
        vec![
            json!({"mailbox": "concierge", "kind": "note", "bead": "sp-1", "read": false, "subject": "loop x3"}),
            json!({"mailbox": "operator", "kind": "question", "bead": "", "read": true, "subject": "folded subject"}),
            json!({"mailbox": "operator", "kind": "", "bead": "", "read": false, "subject": "seen but not flagged S"}),
        ]
    );
    assert!(spira_sim::probe::mail_rows(&d.join("absent")).is_empty());
}

#[test]
fn a_worlds_mail_is_unmuted_and_rooted_inside_it() {
    let s = spira_sim::world::config_settings(Path::new("/w"), &[], Path::new("/w/bin/gh"));
    assert!(s.contains(&("spira.mail_mute".to_string(), "false".to_string())));
    assert_eq!(spira_sim::world::mail_root(Path::new("/w")), Path::new("/w").join("home").join("spira").join("run").join("mail"));
}
