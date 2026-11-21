//! `cr pick` through `cli::run`, over an in-memory tree and ledger: no git, no process.

use crate::cli::{run, Outcome};
use crate::ledger::{Ledger, LedgerFile, Row};
use crate::ports::{blob_of, Config, FixtureTree, History, Ports, SourceTree, VirtualClock};
use serde_json::Value;

mod gh;
mod findings;
mod review;
mod thread;

const DAY: u64 = 86_400_000;
pub(crate) const NOW: u64 = 100 * DAY;

pub(crate) fn cfg() -> Config {
    Config { page_lines: 10, exclude: ["**/fixtures/**", "**/*.generated.*", "**/vendor/**", "Cargo.lock"].map(String::from).to_vec(), churn_days: 60, rewrite_days: 45, count_limit: 7 }
}

fn big_source() -> String {
    let mut s = String::from("//! A large module.\n\n/// The public type.\npub struct Big {\n    pub n: u32,\n}\n\nimpl Big {\n    /// Makes one.\n    pub fn new() -> Big {\n        Big { n: 0 }\n    }\n\n    fn secret(&self) -> u32 {\n        self.n + 1\n    }\n}\n\nfn helper() -> u32 {\n    1\n}\n\nfn driver() -> u32 {\n    helper()\n}\n");
    for i in 0..20 {
        s.push_str(&format!("\nfn filler_{i}() -> u32 {{\n    {i}\n}}\n"));
    }
    s
}

pub(crate) fn fixture() -> FixtureTree {
    let store = "//! The store.\n\npub struct Store;\n\nimpl Store {\n    pub fn open() -> Store {\n        Store\n    }\n}\n";
    FixtureTree::new("c0ffee")
        .with_file("Cargo.toml", "[workspace]\nmembers = [\"demo\", \"other\"]\n")
        .with_file("demo/Cargo.toml", "[package]\nname = \"demo\"\n\n[[bin]]\nname = \"demo-tool\"\npath = \"src/main.rs\"\n\n[dependencies]\nother = { path = \"../other\" }\n")
        .with_file("other/Cargo.toml", "[package]\nname = \"other\"\n")
        .with_file("other/src/lib.rs", "pub mod util;\n")
        .with_file("other/src/util.rs", "//! Shared helpers.\n\npub fn twice(n: u32) -> u32 {\n    n * 2\n}\n")
        .with_file("demo/src/lib.rs", "pub mod store;\npub mod sink;\n")
        .with_file("demo/src/main.rs", "fn main() {}\n")
        .with_file("demo/src/store.rs", store)
        .with_file("demo/src/store/tests.rs", "use super::*;\n\n#[test]\nfn opens() {\n    Store::open();\n}\n")
        .with_file("demo/tests/it.rs", "use demo::store::Store;\n\n#[test]\nfn it() {\n    Store::open();\n}\n")
        .with_file("demo/src/sink.rs", "pub trait Sink {\n    fn put(&self, b: &[u8]);\n}\n")
        .with_file("demo/src/file_sink.rs", "use crate::sink::Sink;\n\npub struct FileSink;\n\nimpl Sink for FileSink {\n    fn put(&self, _b: &[u8]) {}\n}\n")
        .with_file("demo/src/mem_sink.rs", "use crate::sink::Sink;\n\npub struct MemSink;\n\nimpl<'a> Sink for MemSink {\n    fn put(&self, _b: &[u8]) {}\n}\n")
        .with_file("demo/src/router.rs", "use crate::sink::Sink;\n\npub fn f(s: &dyn Sink) {\n    s.put(b\"x\");\n}\n")
        .with_file("demo/src/net.rs", "use other::util;\n\npub fn go() -> u32 {\n    util::twice(2)\n}\n")
        .with_file("demo/src/big.rs", &big_source())
        .with_file("demo/src/fixtures/blob.rs", "pub fn planted() {}\n")
        .with_file("demo/src/gen.generated.rs", "pub fn planted_too() {}\n")
        .with_file("spira/run.sh", "#!/bin/sh\nexec demo-tool --go\n")
        .with_file("spira/hooks/alpha-fence.sh", "#!/bin/sh\nexit 0\n")
        .with_file("spira/hooks/alpha-stop.sh", "#!/bin/sh\nexit 0\n")
        .with_file("spira/runes/alpha.md", "# alpha\n\nA persona.\n")
        .with_commits("demo/src/store.rs", &[NOW - DAY, NOW - 2 * DAY, NOW - 90 * DAY])
}

fn ports_with(tree: FixtureTree, ledger: Vec<Row>, config: Config) -> Ports {
    Ports { tree: Box::new(tree), ledger: Ledger::from_rows(ledger), config, ..ports_with_tree(Box::new(Unreadable)) }
}

fn ports_with_tree(tree: Box<dyn SourceTree>) -> Ports {
    Ports { run_dir: std::path::PathBuf::from("run-dir-unused"), gh: Box::new(gh::FakeGh::default()), repo_owner: String::new(), repo_name: String::new(), findings_dir: String::new(), committer: Box::new(NoCommit), source_repo: "src".into(), stdin: String::new(), clock: Box::new(VirtualClock::new(NOW)), tree, beads: Box::new(crate::ports::FixtureBeads::default()), registry: Box::new(crate::ports::FixtureRegistry::default()), ledger: Ledger::from_rows(vec![]), config: cfg(), env_seed: None }
}

fn ports() -> Ports {
    ports_with(fixture(), vec![], cfg())
}

fn go(p: &Ports, args: &[&str]) -> Outcome {
    let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    run(&argv, p)
}

fn json(p: &Ports, args: &[&str]) -> (i32, Value) {
    let mut a = args.to_vec();
    a.push("--json");
    let o = go(p, &a);
    (o.code, serde_json::from_str(&o.stdout).expect("--json prints one JSON object"))
}

fn unit_files(v: &Value) -> Vec<String> {
    v["unit"]["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap().to_string()).collect()
}

fn row(kind: &str, unit_id: &str, at: u64, files: &[(&str, &str)]) -> Row {
    Row {
        review: "r1".into(),
        at,
        commit: "old".into(),
        kind: kind.into(),
        unit_id: unit_id.into(),
        files: files.iter().map(|(p, b)| LedgerFile { path: p.to_string(), blob: b.to_string() }).collect(),
    }
}

#[test]
fn a_fixed_seed_draws_the_same_unit_with_the_same_terms() {
    let p = ports();
    let a = json(&p, &["pick", "--seed", "7"]);
    let b = json(&p, &["pick", "--seed", "7"]);
    assert_eq!(a, b);
    assert_eq!(a.0, 0);
    let v = a.1;
    assert_eq!(v["seed"], 7);
    assert_eq!(v["commit"], "c0ffee");
    assert_eq!(v["unit"]["id"], "module:demo/src/router.rs");
    assert_eq!(v["stratum"]["kind"], "module");
    assert_eq!(v["weight"]["review_factor"], 4.0);
    assert_eq!(v["weight"]["commits"], 0);
    assert_eq!(v["weight"]["dependents"], 0);
    assert_eq!(v["weight"]["lines"], 5);
    assert_eq!(v["weight"]["size_term"], 0.5);
    assert_eq!(v["weight"]["total"], 2.0);
}

#[test]
fn text_form_names_unit_weight_stratum_seed_and_commit() {
    let o = go(&ports(), &["pick", "--seed", "7"]);
    assert_eq!(o.code, 0);
    for want in ["unit: module:demo/src/router.rs", "kind: module", "weight: 2.000", "stratum: module has never been sent", "seed: 7", "commit: c0ffee", "aim: none"] {
        assert!(o.stdout.contains(want), "missing {want:?} in:\n{}", o.stdout);
    }
}

#[test]
fn the_seed_defaults_to_the_environment_then_the_clock_and_is_printed() {
    let mut p = ports();
    p.env_seed = Some("41".into());
    assert_eq!(json(&p, &["pick"]).1["seed"], 41);
    p.env_seed = None;
    assert_eq!(json(&p, &["pick"]).1["seed"], NOW);
}

#[test]
fn a_module_unit_holds_the_source_its_test_sibling_and_the_integration_test() {
    let (_, v) = json(&ports(), &["pick", "--aim", "demo/src/store.rs"]);
    assert_eq!(v["unit"]["kind"], "module");
    assert_eq!(unit_files(&v), ["demo/src/store.rs", "demo/src/store/tests.rs", "demo/tests/it.rs"]);
    assert_eq!(v["weight"]["commits"], 2);
}

#[test]
fn a_trait_unit_holds_the_trait_its_impls_and_the_dyn_caller() {
    let (_, v) = json(&ports(), &["pick", "--aim", "Sink"]);
    assert_eq!(v["unit"]["kind"], "trait");
    assert_eq!(unit_files(&v), ["demo/src/sink.rs", "demo/src/file_sink.rs", "demo/src/mem_sink.rs", "demo/src/router.rs"]);
}

#[test]
fn a_boundary_unit_holds_the_caller_and_the_module_it_uses() {
    let p = ports_with(fixture(), vec![row("module", "module:elsewhere", NOW, &[("elsewhere.rs", "x")])], cfg());
    let (_, v) = json(&p, &["pick", "--aim", "other"]);
    assert_eq!(v["unit"]["id"], "boundary:demo/src/net.rs->other/src/util.rs");
    assert_eq!(unit_files(&v), ["demo/src/net.rs", "other/src/util.rs"]);
}

#[test]
fn aim_followed_by_json_takes_json_as_the_aim_and_help_as_a_value_is_no_help() {
    let o = go(&ports(), &["pick", "--aim", "--json"]);
    assert_eq!(o.code, 2);
    assert!(o.stderr.contains("matched nothing"), "{}", o.stderr);
    assert!(go(&ports(), &["pick", "--aim", "-h"]).stdout.is_empty());
}

#[test]
fn every_default_exclude_form_weighs_zero() {
    for path in ["a/fixtures/x.rs", "fixtures/x.rs", "a/b.generated.rs", "x/vendor/y/z.rs", "Cargo.lock", "sub/Cargo.lock"] {
        assert!(crate::glob::any(&cfg().exclude, path), "{path}");
    }
    for path in ["a/fixture/x.rs", "a/b.rs", "Cargo.toml"] {
        assert!(!crate::glob::any(&cfg().exclude, path), "{path}");
    }
}

#[test]
fn a_script_unit_holds_the_script_and_the_binary_it_runs() {
    let (_, v) = json(&ports(), &["pick", "--aim", "spira/run.sh"]);
    assert_eq!(v["unit"]["kind"], "script");
    assert_eq!(unit_files(&v), ["spira/run.sh", "demo/src/main.rs"]);
}

#[test]
fn a_persona_unit_groups_hooks_by_prefix_with_the_prompt() {
    let (_, v) = json(&ports(), &["pick", "--aim", "spira/hooks/alpha-stop.sh"]);
    assert_eq!(v["unit"]["kind"], "persona");
    assert_eq!(unit_files(&v), ["spira/hooks/alpha-fence.sh", "spira/hooks/alpha-stop.sh", "spira/runes/alpha.md"]);
}

#[test]
fn a_large_file_is_drawn_whole() {
    let p = ports();
    let o = go(&p, &["pick", "--aim", "demo/src/big.rs", "--seed", "3"]);
    let line = o.stdout.lines().find(|l| l.starts_with("  demo/src/big.rs")).unwrap();
    assert!(line.starts_with("  demo/src/big.rs (") && line.ends_with(" lines)"), "{}", o.stdout);
    let (_, v) = json(&p, &["pick", "--aim", "demo/src/big.rs", "--seed", "3"]);
    for f in v["unit"]["files"].as_array().unwrap() {
        let mut keys: Vec<&String> = f.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["lines", "path"], "{f}");
    }
}

#[test]
fn excluded_paths_are_never_drawn_across_a_hundred_seeds() {
    let loose = Config { exclude: vec![], ..cfg() };
    let p = ports_with(fixture(), vec![], loose);
    let (code, v) = json(&p, &["pick", "--aim", "demo/src/fixtures/blob.rs"]);
    assert_eq!(code, 0, "the matcher must be able to find the planted file: {v}");

    let p = ports();
    for seed in 0..100 {
        let (_, v) = json(&p, &["pick", "--seed", &seed.to_string()]);
        for f in unit_files(&v) {
            assert!(!f.contains("fixtures") && !f.contains(".generated."), "seed {seed} drew {f}");
        }
    }
    let (code, v) = json(&p, &["pick", "--aim", "demo/src/gen.generated.rs"]);
    assert_eq!(code, 1, "{v}");
}

#[test]
fn a_unit_sent_at_its_current_blob_is_not_drawn_until_the_file_changes() {
    let store = "//! The store.\n\npub struct Store;\n\nimpl Store {\n    pub fn open() -> Store {\n        Store\n    }\n}\n";
    let sent = row("module", "module:demo/src/store.rs", NOW - DAY, &[("demo/src/store.rs", &blob_of(store.as_bytes()))]);
    let p = ports_with(fixture(), vec![sent.clone()], cfg());
    let (code, v) = json(&p, &["pick", "--aim", "demo/src/store.rs"]);
    assert_eq!(code, 1, "{v}");

    let changed = fixture().with_file("demo/src/store.rs", &format!("{store}// edited\n"));
    let p = ports_with(changed, vec![sent], cfg());
    let (code, v) = json(&p, &["pick", "--aim", "demo/src/store.rs"]);
    assert_eq!(code, 0, "{v}");
    assert!(v["weight"]["review_factor"].as_f64().unwrap() < 4.0);
}

#[test]
fn with_only_modules_in_the_ledger_the_next_pick_is_not_a_module() {
    let p = ports_with(fixture(), vec![row("module", "module:elsewhere", NOW - DAY, &[("elsewhere.rs", "x")])], cfg());
    for seed in 0..20 {
        let (_, v) = json(&p, &["pick", "--seed", &seed.to_string()]);
        assert_ne!(v["unit"]["kind"], "module", "seed {seed}");
        assert_eq!(v["stratum"]["kind"], "trait");
    }
}

#[test]
fn an_empty_kind_falls_through_to_the_next() {
    let tree = FixtureTree::new("c").with_file("Cargo.toml", "[workspace]\nmembers = [\"k\"]\n").with_file("k/Cargo.toml", "[package]\nname = \"k\"\n").with_file("k/src/lib.rs", "pub fn a() {}\n");
    let (_, v) = json(&ports_with(tree, vec![], cfg()), &["pick"]);
    assert_eq!(v["stratum"]["kind"], "module");
    assert_eq!(v["stratum"]["skipped"], serde_json::json!([]));
    let p = ports_with(FixtureTree::new("c").with_file("Cargo.toml", "[workspace]\nmembers = [\"k\"]\n").with_file("k/Cargo.toml", "[package]\nname = \"k\"\n").with_file("k/src/lib.rs", "pub fn a() {}\n").with_file("x/hooks/b-c.sh", "#\n"), vec![row("persona", "p", NOW, &[])], cfg());
    let (_, v) = json(&p, &["pick"]);
    assert_eq!(v["stratum"]["kind"], "module");
}

#[test]
fn aim_resolves_a_path_a_crate_a_type_and_words() {
    let p = ports();
    let (_, v) = json(&p, &["pick", "--aim", "demo/src/sink.rs"]);
    assert_eq!(v["aim"]["resolution"], "path");
    let (_, v) = json(&p, &["pick", "--aim", "other"]);
    assert_eq!(v["aim"]["resolution"], "crate");
    assert!(unit_files(&v).iter().any(|f| f.starts_with("other/")));
    let (_, v) = json(&p, &["pick", "--aim", "Store"]);
    assert_eq!(v["aim"]["resolution"], "identifier");
    assert_eq!(v["unit"]["id"], "module:demo/src/store.rs");
    let (_, v) = json(&p, &["pick", "--aim", "Sink"]);
    assert_eq!(v["aim"]["resolution"], "identifier");
    assert_eq!(v["unit"]["kind"], "trait");
    let (_, v) = json(&p, &["pick", "--aim", "SHARED helpers"]);
    assert_eq!(v["aim"]["resolution"], "words");
    assert!(unit_files(&v).contains(&"other/src/util.rs".to_string()));
}

#[test]
fn an_aim_that_matches_nothing_exits_two_and_names_each_resolution() {
    let o = go(&ports(), &["pick", "--aim", "nothing-matches-this"]);
    assert_eq!(o.code, 2);
    for r in ["path:", "crate:", "identifier:", "words:"] {
        assert!(o.stderr.contains(&format!("tried {r}")), "{}", o.stderr);
    }
    let (code, v) = json(&ports(), &["pick", "--aim", "nothing-matches-this"]);
    assert_eq!((code, v["ok"].clone(), v["tried"].as_array().unwrap().len()), (2, Value::Bool(false), 4));
}

#[test]
fn a_bare_positional_exits_two_naming_aim() {
    let o = go(&ports(), &["pick", "store"]);
    assert_eq!(o.code, 2);
    assert!(o.stderr.contains("--aim store"), "{}", o.stderr);
    let (code, v) = json(&ports(), &["pick", "store"]);
    assert_eq!(code, 2);
    assert!(v["error"].as_str().unwrap().contains("--aim"));
}

#[test]
fn help_documents_every_flag_and_the_exit_codes() {
    let o = go(&ports(), &["-h"]);
    assert_eq!(o.code, 0);
    for want in ["--aim", "--seed", "--json", "Exit codes", "  0 ", "  1 ", "  2 "] {
        assert!(o.stdout.contains(want), "{want}");
    }
}

#[test]
fn the_ledger_round_trips_through_a_file() {
    let dir = testkit::TempDir::new("cr-ledger");
    let path = dir.path().join("cr").join("ledger.jsonl");
    assert!(Ledger::load(&path).unwrap().rows.is_empty());
    let r = row("module", "m", 5, &[("a.rs", "b1")]);
    Ledger::append(&path, &r).unwrap();
    Ledger::append(&path, &r).unwrap();
    assert_eq!(Ledger::load(&path).unwrap().rows, vec![r.clone(), r]);
}

fn lines(n: std::ops::Range<u32>) -> String {
    n.map(|i| format!("let hit_{i} = 1;\n")).collect()
}

fn count_tree() -> FixtureTree {
    FixtureTree::new("beef01")
        .with_file("a/one.rs", &format!("// let hit_c = 1;\n{}", lines(0..49)))
        .with_file("a/two.rs", &lines(0..40))
        .with_file("b/three.rs", &lines(0..30))
        .with_file("b/none.rs", "fn quiet() {}\n")
        .with_file("b/fixtures/planted.rs", &lines(0..9))
        .with_file("b/notes.md", "let hit_md = 1;\n")
}

fn count_ports() -> Ports {
    ports_with(count_tree(), vec![], cfg())
}

#[test]
fn count_totals_every_code_line_skips_comments_and_cuts_the_list_at_the_limit() {
    let o = go(&count_ports(), &["count", "--regex", "hit_", "--limit", "5"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let out: Vec<&str> = o.stdout.lines().collect();
    assert_eq!(out[0], "total 119 lines in 3 files (4 files searched, 1 comment lines skipped) at beef01");
    assert_eq!(&out[1..6], ["a/one.rs:2: let hit_0 = 1;", "a/one.rs:3: let hit_1 = 1;", "a/one.rs:4: let hit_2 = 1;", "a/one.rs:5: let hit_3 = 1;", "a/one.rs:6: let hit_4 = 1;"]);
    assert_eq!(out[6], "… 114 more");
    assert_eq!(out.len(), 7);
}

#[test]
fn count_lists_everything_without_a_closing_line_when_nothing_is_cut() {
    let o = go(&count_ports(), &["count", "--regex", "hit_c", "--limit", "5"]);
    assert_eq!(o.stdout.lines().count(), 1, "{}", o.stdout);
    assert!(o.stdout.starts_with("total 0 lines in 0 files"));
    let o = go(&count_ports(), &["count", "--regex", "hit_39", "--limit", "5"]);
    assert_eq!(o.stdout.lines().collect::<Vec<_>>(), ["total 2 lines in 2 files (4 files searched, 0 comment lines skipped) at beef01", "a/one.rs:41: let hit_39 = 1;", "a/two.rs:40: let hit_39 = 1;"]);
}

#[test]
fn count_path_globs_narrow_the_search_and_may_repeat() {
    let (_, v) = json(&count_ports(), &["count", "--regex", "hit_", "--path", "a/**"]);
    assert_eq!((v["total"].clone(), v["searched"].clone()), (89.into(), 2.into()));
    let (_, v) = json(&count_ports(), &["count", "--regex", "hit_", "--path", "a/two.rs", "--path", "b/three.rs"]);
    assert_eq!((v["total"].clone(), v["files"].clone()), (70.into(), 2.into()));
}

#[test]
fn count_ignores_excluded_paths_and_prose_files() {
    let (_, v) = json(&count_ports(), &["count", "--regex", "hit_8$|hit_md", "--limit", "50"]);
    let paths: Vec<&str> = v["sites"].as_array().unwrap().iter().map(|s| s["path"].as_str().unwrap()).collect();
    assert!(!paths.iter().any(|p| p.contains("fixtures") || p.ends_with(".md")), "{paths:?}");
}

#[test]
fn count_of_nothing_is_zero_and_shows_it_looked() {
    let o = go(&count_ports(), &["count", "--regex", "zzz_absent"]);
    assert_eq!(o.code, 0);
    assert!(o.stdout.starts_with("total 0 lines in 0 files (4 files searched,"), "{}", o.stdout);
}

pub(crate) struct NoCommit;
impl crate::ports::Committer for NoCommit {
    fn commit(&self, _: &std::path::Path, _: &[&str], _: &str) -> Result<(), String> {
        Ok(())
    }
}

struct Unreadable;
impl SourceTree for Unreadable {
    fn commit(&self) -> Result<String, String> {
        Ok("dead".into())
    }
    fn files(&self) -> Result<Vec<String>, String> {
        Err("the object store is gone".into())
    }
    fn read(&self, _: &str) -> Option<Vec<u8>> {
        None
    }
    fn blob(&self, _: &str) -> Option<String> {
        None
    }
    fn history(&self, _: &str, _: u64) -> History {
        History::default()
    }
    fn authors(&self, _: &str) -> Vec<String> {
        Vec::new()
    }
    fn log(&self, _: &str) -> Vec<crate::ports::CommitInfo> {
        Vec::new()
    }
    fn line_times(&self, _: &str) -> Vec<u64> {
        Vec::new()
    }
    fn removed_tests(&self, _: &[String]) -> Vec<crate::ports::RemovedTest> {
        Vec::new()
    }
    fn at(&self, _: &str) -> Result<Box<dyn SourceTree>, String> {
        Err("unreadable".into())
    }
}

#[test]
fn count_on_an_unreadable_tree_exits_one_and_prints_no_total() {
    let p = ports_with_tree(Box::new(Unreadable));
    let o = go(&p, &["count", "--regex", "x"]);
    assert_eq!(o.code, 1);
    assert!(!o.stdout.contains("total") && o.stderr.contains("object store"), "{o:?}");
    let (code, v) = json(&p, &["count", "--regex", "x"]);
    assert_eq!((code, v["total"].clone()), (1, Value::Null));
    assert!(v["error"].as_str().unwrap().contains("object store"));
}

#[test]
fn count_with_a_bad_regex_or_a_bare_positional_exits_two() {
    let o = go(&count_ports(), &["count", "--regex", "("]);
    assert_eq!(o.code, 2);
    let o = go(&count_ports(), &["count", "foo"]);
    assert_eq!(o.code, 2);
    assert!(o.stderr.contains("--regex foo"), "{}", o.stderr);
    let (code, v) = json(&count_ports(), &["count", "foo"]);
    assert_eq!((code, v["total"].clone()), (2, Value::Null));
    assert_eq!(go(&count_ports(), &["count"]).code, 2);
}

#[test]
fn count_limit_defaults_to_the_configured_key() {
    let (_, v) = json(&count_ports(), &["count", "--regex", "hit_"]);
    assert_eq!((v["sites"].as_array().unwrap().len(), v["more"].clone()), (7, 112.into()));
}

#[test]
fn count_json_matches_the_text_totals_and_names_the_commit() {
    let (code, v) = json(&count_ports(), &["count", "--regex", "hit_", "--limit", "2"]);
    assert_eq!(code, 0);
    assert_eq!((v["total"].clone(), v["files"].clone(), v["searched"].clone(), v["skipped_comments"].clone(), v["more"].clone()), (119.into(), 3.into(), 4.into(), 1.into(), 117.into()));
    assert_eq!(v["commit"], "beef01");
    assert_eq!(v["sites"][1], serde_json::json!({"path": "a/one.rs", "line": 3, "text": "let hit_1 = 1;"}));
}

#[test]
fn count_comment_markers_follow_the_file_type() {
    let t = FixtureTree::new("c1").with_file("x.sh", "# hit\n  hit\n").with_file("y.py", "# hit\nhit\n").with_file("z.toml", "# hit\n").with_file("w.rs", "# hit\n// hit\n");
    let (_, v) = json(&ports_with(t, vec![], cfg()), &["count", "--regex", "hit"]);
    assert_eq!((v["total"].clone(), v["skipped_comments"].clone()), (3.into(), 4.into()));
}

#[test]
fn help_documents_count_and_its_exit_codes() {
    let o = go(&count_ports(), &["-h"]);
    for want in ["cr count", "--regex", "--path", "--limit"] {
        assert!(o.stdout.contains(want), "{want}");
    }
}
