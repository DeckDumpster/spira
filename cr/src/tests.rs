//! `cr pick` through `cli::run`, over an in-memory tree and ledger: no git, no process.

use crate::cli::{run, Outcome};
use crate::ledger::{Attention, ClosedAs, Ledger, LedgerFile, Row};
use crate::ports::{blob_of, Config, FixtureTree, History, Ports, SourceTree, VirtualClock};
use serde_json::Value;

mod findings;
mod review;
mod thread;

pub(crate) const SOURCE_URL: &str = "https://github.com/acme/demo";
const DAY: u64 = 86_400_000;
pub(crate) const NOW: u64 = 100 * DAY;

pub(crate) fn cfg() -> Config {
    Config { page_lines: 10, exclude: ["**/fixtures/**", "**/*.generated.*", "**/vendor/**", "Cargo.lock"].map(String::from).to_vec(), churn_days: 60, rewrite_days: 45, count_limit: 7, status_limit: 10 }
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

thread_local! {
    static FINDINGS_DIRS: std::cell::RefCell<Vec<testkit::TempDir>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A findings directory holding `rows` as its ledger, kept for the life of the test's thread.
pub(crate) fn ledger_dir(rows: &[Row]) -> String {
    let dir = testkit::TempDir::new("cr-ledger");
    let text: String = rows.iter().map(|r| format!("{}\n", serde_json::to_string(r).unwrap())).collect();
    std::fs::write(dir.path().join(crate::ledger::FILE), text).unwrap();
    let path = dir.path().to_str().unwrap().to_string();
    FINDINGS_DIRS.with(|d| d.borrow_mut().push(dir));
    path
}

pub(crate) fn ledger_rows(p: &Ports) -> Vec<Row> {
    Ledger::parse(&std::fs::read_to_string(std::path::Path::new(&p.findings_dir).join(crate::ledger::FILE)).unwrap()).unwrap().rows
}

/// A committer that records what it was asked to commit; `fail` makes the next commits fail and
/// `history` answers whether the ledger was ever committed.
#[derive(Default)]
pub(crate) struct Recorder {
    pub log: std::cell::RefCell<Vec<(Vec<String>, String)>>,
    pub last: std::cell::RefCell<Vec<String>>,
    pub fail: std::cell::Cell<bool>,
    pub history: std::cell::Cell<bool>,
}

pub(crate) struct SharedRecorder(pub std::rc::Rc<Recorder>);

impl crate::ports::Committer for SharedRecorder {
    fn commit(&self, dir: &std::path::Path, paths: &[&str], message: &str) -> Result<(), String> {
        if self.0.fail.get() {
            return Err("git commit failed".into());
        }
        let now: Vec<String> = paths.iter().map(|p| std::fs::read_to_string(dir.join(p)).unwrap()).collect();
        if *self.0.last.borrow() != now {
            *self.0.last.borrow_mut() = now;
            self.0.log.borrow_mut().push((paths.iter().map(|s| s.to_string()).collect(), message.to_string()));
        }
        Ok(())
    }
    fn has_history(&self, _: &std::path::Path, _: &str) -> Result<bool, String> {
        Ok(self.0.history.get())
    }
}

/// Posts the unit's files as a collection and records it on review `number`, as a seal would.
pub(crate) fn attach_collection(margin: &margin_client::FakeMargin, tree: &FixtureTree, run: &std::path::Path, number: u32) -> crate::record::Review {
    use margin_client::Margin;
    let mut r = crate::record::load(run, number).unwrap();
    let docs: Vec<margin_client::Doc> = r.unit.files.iter().map(|f| margin_client::Doc { path: f.path.clone(), kind: "code".into(), text: String::from_utf8(tree.read(&f.path).unwrap()).unwrap() }).collect();
    let c = margin.post_collection(&r.unit.title, "cover", &docs, None).unwrap();
    r.collection = Some(c.id);
    r.url = Some(c.url);
    crate::record::save(run, &r).unwrap();
    r
}

fn ports_with(tree: FixtureTree, ledger: Vec<Row>, config: Config) -> Ports {
    Ports { tree: Box::new(tree), findings_dir: ledger_dir(&ledger), config, ..ports_with_tree(Box::new(Unreadable)) }
}

fn ports_with_tree(tree: Box<dyn SourceTree>) -> Ports {
    Ports { run_dir: std::path::PathBuf::from("run-dir-unused"), margin: Box::new(margin_client::FakeMargin::default()), findings_dir: String::new(), committer: Box::new(NoCommit), source_repo: "src".into(), source_url: Ok(SOURCE_URL.into()), stdin: String::new(), clock: Box::new(VirtualClock::new(NOW)), tree, beads: Box::new(crate::ports::FixtureBeads::default()), registry: Box::new(crate::ports::FixtureRegistry::default()), config: cfg(), env_seed: None }
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
        review: format!("r-{unit_id}"),
        kind: kind.into(),
        unit_id: unit_id.into(),
        commit: "old".into(),
        files: files.iter().map(|(p, b)| LedgerFile { path: p.to_string(), blob: b.to_string() }).collect(),
        opened_at: at,
        closed_at: None,
        closed_as: None,
        attention: None,
    }
}

fn closed(r: Row, at: u64, how: ClosedAs) -> Row {
    r.closing(at, how, None)
}

#[test_id(tdi4akr)]
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

#[test_id(tpkund3)]
#[test]
fn text_form_names_unit_weight_stratum_seed_and_commit() {
    let o = go(&ports(), &["pick", "--seed", "7"]);
    assert_eq!(o.code, 0);
    for want in ["unit: module:demo/src/router.rs", "kind: module", "weight: 2.000", "stratum: module has never been sent", "seed: 7", "commit: c0ffee", "aim: none"] {
        assert!(o.stdout.contains(want), "missing {want:?} in:\n{}", o.stdout);
    }
}

#[test_id(tjdhmw5)]
#[test]
fn the_seed_defaults_to_the_environment_then_the_clock_and_is_printed() {
    let mut p = ports();
    p.env_seed = Some("41".into());
    assert_eq!(json(&p, &["pick"]).1["seed"], 41);
    p.env_seed = None;
    assert_eq!(json(&p, &["pick"]).1["seed"], NOW);
}

#[test_id(t2gw4az)]
#[test]
fn a_module_unit_holds_the_source_its_test_sibling_and_the_integration_test() {
    let (_, v) = json(&ports(), &["pick", "--aim", "demo/src/store.rs"]);
    assert_eq!(v["unit"]["kind"], "module");
    assert_eq!(unit_files(&v), ["demo/src/store.rs", "demo/src/store/tests.rs", "demo/tests/it.rs"]);
    assert_eq!(v["weight"]["commits"], 2);
}

#[test_id(t1rmmcp)]
#[test]
fn a_trait_unit_holds_the_trait_its_impls_and_the_dyn_caller() {
    let (_, v) = json(&ports(), &["pick", "--aim", "Sink"]);
    assert_eq!(v["unit"]["kind"], "trait");
    assert_eq!(unit_files(&v), ["demo/src/sink.rs", "demo/src/file_sink.rs", "demo/src/mem_sink.rs", "demo/src/router.rs"]);
}

fn two_worlds() -> FixtureTree {
    FixtureTree::new("c0ffee")
        .with_file("Cargo.toml", "[workspace]\nmembers = [\"a\", \"b\", \"c\"]\n")
        .with_file("a/Cargo.toml", "[package]\nname = \"a\"\n\n[dependencies]\nc = { path = \"../c\" }\n")
        .with_file("b/Cargo.toml", "[package]\nname = \"b\"\n\n[dependencies]\na = { path = \"../a\" }\n")
        .with_file("c/Cargo.toml", "[package]\nname = \"c\"\n")
        .with_file("c/src/lib.rs", "pub mod ports;\n")
        .with_file("c/src/ports.rs", "pub trait World { fn far(&self); }\n")
        .with_file("a/src/ports.rs", "pub trait World { fn go(&self); }\n")
        .with_file("a/src/real.rs", "use crate::ports::World;\n\npub struct Real;\n\nimpl World for Real {\n    fn go(&self) {}\n}\n")
        .with_file("a/src/far.rs", "use c::ports::World;\n\npub struct Far;\n\nimpl World for Far {\n    fn far(&self) {}\n}\n")
        .with_file("a/src/lib.rs", "pub mod ports;\npub mod real;\n\npub fn run(w: &dyn ports::World) {}\n")
        .with_file("b/src/ports.rs", "pub trait World { fn stop(&self); }\n")
        .with_file("b/src/real.rs", "use crate::ports::World;\n\npub struct Real;\n\nimpl World for Real {\n    fn stop(&self) {}\n}\n")
        .with_file("b/src/bridge.rs", "use a::ports::World;\n\npub struct Bridge;\n\nimpl World for Bridge {\n    fn go(&self) {}\n}\n")
        .with_file("b/src/main.rs", "fn go(w: &dyn a::ports::World) {}\n")
}

fn trait_unit_files(units: &[crate::units::Unit], id: &str) -> Vec<String> {
    units.iter().find(|u| u.id == id).unwrap_or_else(|| panic!("no unit {id}")).files.iter().map(|f| f.path.clone()).collect()
}

#[test_id(tqii6im)]
#[test]
fn a_trait_unit_holds_only_what_resolves_to_that_trait_by_path() {
    let ix = crate::index::Index::build(&two_worlds()).unwrap();
    let units = crate::units::trait_units(&ix);
    assert_eq!(trait_unit_files(&units, "trait:a/src/ports.rs::World"), ["a/src/ports.rs", "a/src/real.rs", "b/src/bridge.rs", "a/src/lib.rs", "b/src/main.rs"]);
    assert_eq!(trait_unit_files(&units, "trait:b/src/ports.rs::World"), ["b/src/ports.rs", "b/src/real.rs"]);
    assert_eq!(trait_unit_files(&units, "trait:c/src/ports.rs::World"), ["c/src/ports.rs", "a/src/far.rs"]);
}

#[test_id(tejcb8b)]
#[test]
fn a_trait_is_reached_by_glob_rename_and_crate_root_reexport() {
    let t = two_worlds()
        .with_file("a/src/lib.rs", "pub mod ports;\npub mod real;\npub use ports::World;\n")
        .with_file("a/src/glob.rs", "use crate::ports::*;\n\nimpl World for G {}\n")
        .with_file("a/src/renamed.rs", "use crate::ports::World as W;\n\nimpl W for R {}\n")
        .with_file("b/src/root.rs", "use a::World;\n\nimpl World for Q {}\n")
        .with_file("b/src/qualified.rs", "impl a::ports::World for P {}\n")
        .with_file("b/src/sup.rs", "use super::ports::World;\n\nimpl World for S {}\n");
    let ix = crate::index::Index::build(&t).unwrap();
    let files = trait_unit_files(&crate::units::trait_units(&ix), "trait:a/src/ports.rs::World");
    for f in ["a/src/glob.rs", "a/src/renamed.rs", "b/src/root.rs", "b/src/qualified.rs"] {
        assert!(files.contains(&f.to_string()), "{f} missing from {files:?}");
    }
    assert!(!files.contains(&"b/src/sup.rs".to_string()), "{files:?}");
}

#[test_id(tzc8sfr)]
#[test]
fn a_local_trait_of_the_same_name_shadows_a_glob_and_pub_crate_use_reexports() {
    let t = two_worlds()
        .with_file("a/src/lib.rs", "pub mod ports;\npub mod real;\npub mod mid;\n")
        .with_file("a/src/mid.rs", "pub(crate) use crate::ports::World;\n")
        .with_file("a/src/shadow.rs", "use crate::ports::*;\n\ntrait World {}\n\nimpl World for X {}\n")
        .with_file("a/src/viamid.rs", "use crate::mid::World;\n\nimpl World for M {}\n")
        .with_file("a/src/arg.rs", "use crate::ports as p;\n\nfn f(w: impl p::World) {}\nfn g(w: &dyn p::World) {}\n")
        .with_file("a/src/private.rs", "fn f(x: (u8)) {}\nuse crate::ports::World;\n")
        .with_file("a/src/viaprivate.rs", "use crate::private::World;\n\nimpl World for N {}\n");
    let ix = crate::index::Index::build(&t).unwrap();
    let files = trait_unit_files(&crate::units::trait_units(&ix), "trait:a/src/ports.rs::World");
    assert!(!files.contains(&"a/src/shadow.rs".to_string()), "{files:?}");
    assert!(!files.contains(&"a/src/viaprivate.rs".to_string()), "{files:?}");
    for f in ["a/src/viamid.rs", "a/src/arg.rs"] {
        assert!(files.contains(&f.to_string()), "{f} missing from {files:?}");
    }
}

#[test_id(t8vaty8)]
#[test]
fn a_boundary_unit_holds_the_caller_and_the_module_it_uses() {
    let p = ports_with(fixture(), vec![row("module", "module:elsewhere", NOW, &[("elsewhere.rs", "x")])], cfg());
    let (_, v) = json(&p, &["pick", "--aim", "other"]);
    assert_eq!(v["unit"]["id"], "boundary:demo/src/net.rs->other/src/util.rs");
    assert_eq!(unit_files(&v), ["demo/src/net.rs", "other/src/util.rs"]);
}

#[test_id(tgw8kcn)]
#[test]
fn aim_followed_by_json_takes_json_as_the_aim_and_help_as_a_value_is_no_help() {
    let o = go(&ports(), &["pick", "--aim", "--json"]);
    assert_eq!(o.code, 2);
    assert!(o.stderr.contains("matched nothing"), "{}", o.stderr);
    assert!(go(&ports(), &["pick", "--aim", "-h"]).stdout.is_empty());
}

#[test_id(tne1ly2)]
#[test]
fn every_default_exclude_form_weighs_zero() {
    for path in ["a/fixtures/x.rs", "fixtures/x.rs", "a/b.generated.rs", "x/vendor/y/z.rs", "Cargo.lock", "sub/Cargo.lock"] {
        assert!(crate::glob::any(&cfg().exclude, path), "{path}");
    }
    for path in ["a/fixture/x.rs", "a/b.rs", "Cargo.toml"] {
        assert!(!crate::glob::any(&cfg().exclude, path), "{path}");
    }
}

#[test_id(tbbh51t)]
#[test]
fn a_script_unit_holds_the_script_and_the_binary_it_runs() {
    let (_, v) = json(&ports(), &["pick", "--aim", "spira/run.sh"]);
    assert_eq!(v["unit"]["kind"], "script");
    assert_eq!(unit_files(&v), ["spira/run.sh", "demo/src/main.rs"]);
}

#[test_id(tplfsb2)]
#[test]
fn a_persona_unit_groups_hooks_by_prefix_with_the_prompt() {
    let (_, v) = json(&ports(), &["pick", "--aim", "spira/hooks/alpha-stop.sh"]);
    assert_eq!(v["unit"]["kind"], "persona");
    assert_eq!(unit_files(&v), ["spira/hooks/alpha-fence.sh", "spira/hooks/alpha-stop.sh", "spira/runes/alpha.md"]);
}

#[test_id(tx1ecut)]
#[test]
fn a_large_file_is_drawn_whole() {
    let p = ports();
    let o = go(&p, &["pick", "--aim", "demo/src/big.rs", "--seed", "3"]);
    let line = o.stdout.lines().find(|l| l.starts_with("  demo/src/big.rs")).unwrap();
    assert!(line.starts_with("  demo/src/big.rs (") && line.ends_with(" lines)"), "{}", o.stdout);
    assert!(!o.stdout.contains("oversized"), "{}", o.stdout);
    let (_, v) = json(&p, &["pick", "--aim", "demo/src/big.rs", "--seed", "3"]);
    for f in v["unit"]["files"].as_array().unwrap() {
        let mut keys: Vec<&String> = f.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["lines", "path"], "{f}");
    }
}

#[test_id(t88d7ee)]
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

const STORE: &str = "//! The store.\n\npub struct Store;\n\nimpl Store {\n    pub fn open() -> Store {\n        Store\n    }\n}\n";
const STORE_ID: &str = "module:demo/src/store.rs";

fn store_row() -> Row {
    row("module", STORE_ID, NOW - 40 * DAY, &[("demo/src/store.rs", &blob_of(STORE.as_bytes()))])
}

#[test_id(td8lepk)]
#[test]
fn a_review_closed_unpublished_leaves_its_unit_never_reviewed() {
    let open = store_row();
    let p = ports_with(fixture(), vec![open.clone()], cfg());
    let (code, v) = json(&p, &["pick", "--aim", "demo/src/store.rs"]);
    assert_eq!(code, 1, "an open row alone holds the unit: {v}");

    let p = ports_with(fixture(), vec![open.clone(), closed(open.clone(), NOW - DAY, ClosedAs::Unpublished)], cfg());
    let (code, v) = json(&p, &["pick", "--aim", "demo/src/store.rs"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["weight"]["review_factor"].as_f64().unwrap(), 4.0);

    let p = ports_with(fixture(), vec![open.clone(), closed(open.clone(), NOW - DAY, ClosedAs::Published)], cfg());
    let (code, v) = json(&p, &["pick", "--aim", "demo/src/store.rs"]);
    assert_eq!(code, 1, "a published review at the same blob is not drawn: {v}");
}

#[test_id(tqze4u9)]
#[test]
fn a_changed_file_is_drawable_again_and_weighs_by_lines_changed() {
    let open = store_row();
    let rows = vec![open.clone(), closed(open, NOW - 30 * DAY, ClosedAs::Published)];
    let old = blob_of(STORE.as_bytes());
    let edited = fixture().with_file("demo/src/store.rs", &format!("{STORE}// edited\n")).with_changed("demo/src/store.rs", &old, 4);
    let (code, v) = json(&ports_with(edited, rows.clone(), cfg()), &["pick", "--aim", "demo/src/store.rs"]);
    assert_eq!(code, 0, "{v}");
    let w = &v["weight"];
    assert_eq!(w["changed_lines"], 4);
    let lines = w["lines"].as_f64().unwrap();
    let term = 1.0 + (4.0 / lines).min(1.0);
    assert!((w["changed_term"].as_f64().unwrap() - term).abs() < 1e-9, "{w}");
    assert!((w["review_factor"].as_f64().unwrap() - 2.0).abs() < 1e-9, "{w}");
    let product = w["review_factor"].as_f64().unwrap() * term * w["churn_term"].as_f64().unwrap() * w["dependents_term"].as_f64().unwrap() * w["size_term"].as_f64().unwrap();
    assert!((w["total"].as_f64().unwrap() - product).abs() < 1e-9, "{w}");

    let unmeasured = fixture().with_file("demo/src/store.rs", &format!("{STORE}// edited\n"));
    let (_, v) = json(&ports_with(unmeasured, rows, cfg()), &["pick", "--aim", "demo/src/store.rs"]);
    let (changed, lines) = (v["weight"]["changed_lines"].as_f64().unwrap(), v["weight"]["lines"].as_f64().unwrap());
    assert_eq!(changed, 10.0, "an undiffable blob counts as wholly changed: {v}");
    assert!((v["weight"]["changed_term"].as_f64().unwrap() - (1.0 + changed / lines)).abs() < 1e-9, "{v}");
}

#[test_id(tc6s3vw)]
#[test]
fn a_renamed_file_keeps_its_review() {
    let mut old = row("module", "module:demo/src/old.rs", NOW - 40 * DAY, &[("demo/src/old.rs", &blob_of(STORE.as_bytes()))]);
    old.commit = "before".into();
    let rows = vec![old.clone(), closed(old, NOW - DAY, ClosedAs::Published)];
    let renamed = fixture().with_rename("before", "demo/src/old.rs", "demo/src/store.rs");
    let (code, v) = json(&ports_with(renamed, rows.clone(), cfg()), &["pick", "--aim", "demo/src/store.rs"]);
    assert_eq!(code, 1, "the renamed file is still reviewed: {v}");
    let (code, v) = json(&ports_with(fixture(), rows, cfg()), &["pick", "--aim", "demo/src/store.rs"]);
    assert_eq!(code, 0, "without the rename the new path is unreviewed: {v}");
}

#[test_id(t39gry8)]
#[test]
fn pick_refuses_when_the_ledger_was_deleted_after_being_committed_or_the_findings_dir_is_unnamed() {
    let mut p = ports();
    std::fs::remove_file(std::path::Path::new(&p.findings_dir).join(crate::ledger::FILE)).unwrap();
    let (code, v) = json(&p, &["pick"]);
    assert_eq!(code, 0, "no ledger and no history is the empty ledger: {v}");
    let rec = std::rc::Rc::new(Recorder::default());
    rec.history.set(true);
    p.committer = Box::new(SharedRecorder(rec));
    let (code, v) = json(&p, &["pick"]);
    assert_eq!(code, 1);
    assert!(v["error"].as_str().unwrap().contains("restore it from git"), "{v}");
    let mut p = ports();
    p.findings_dir = String::new();
    let (code, v) = json(&p, &["pick"]);
    assert_eq!(code, 1);
    assert!(v["error"].as_str().unwrap().contains("SPIRA_CR_FINDINGS_DIR"), "{v}");
}

#[test_id(t3orwqe)]
#[test]
fn with_only_modules_in_the_ledger_the_next_pick_is_not_a_module() {
    let p = ports_with(fixture(), vec![row("module", "module:elsewhere", NOW - DAY, &[("elsewhere.rs", "x")])], cfg());
    for seed in 0..20 {
        let (_, v) = json(&p, &["pick", "--seed", &seed.to_string()]);
        assert_ne!(v["unit"]["kind"], "module", "seed {seed}");
        assert_eq!(v["stratum"]["kind"], "trait");
    }
}

#[test_id(toabce1)]
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

#[test_id(tj1m6x0)]
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

#[test_id(t8shrde)]
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

#[test_id(tln9vf1)]
#[test]
fn a_bare_positional_exits_two_naming_aim() {
    let o = go(&ports(), &["pick", "store"]);
    assert_eq!(o.code, 2);
    assert!(o.stderr.contains("--aim store"), "{}", o.stderr);
    let (code, v) = json(&ports(), &["pick", "store"]);
    assert_eq!(code, 2);
    assert!(v["error"].as_str().unwrap().contains("--aim"));
}

#[test_id(t9e9c6u)]
#[test]
fn help_documents_every_flag_and_the_exit_codes() {
    let o = go(&ports(), &["-h"]);
    assert_eq!(o.code, 0);
    for want in ["--aim", "--seed", "--json", "--review", "--thread", "cr close", "Exit codes", "  0 ", "  1 ", "  2 "] {
        assert!(o.stdout.contains(want), "{want}");
    }
    for gone in ["adopt", "--said", "ledger-close", "--comment", "--sha256", "pull request"] {
        assert!(!o.stdout.contains(gone), "{gone}");
    }
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

#[test_id(tfqslgb)]
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

#[test_id(tj0csw1)]
#[test]
fn count_lists_everything_without_a_closing_line_when_nothing_is_cut() {
    let o = go(&count_ports(), &["count", "--regex", "hit_c", "--limit", "5"]);
    assert_eq!(o.stdout.lines().count(), 1, "{}", o.stdout);
    assert!(o.stdout.starts_with("total 0 lines in 0 files"));
    let o = go(&count_ports(), &["count", "--regex", "hit_39", "--limit", "5"]);
    assert_eq!(o.stdout.lines().collect::<Vec<_>>(), ["total 2 lines in 2 files (4 files searched, 0 comment lines skipped) at beef01", "a/one.rs:41: let hit_39 = 1;", "a/two.rs:40: let hit_39 = 1;"]);
}

#[test_id(tkq4wim)]
#[test]
fn count_path_globs_narrow_the_search_and_may_repeat() {
    let (_, v) = json(&count_ports(), &["count", "--regex", "hit_", "--path", "a/**"]);
    assert_eq!((v["total"].clone(), v["searched"].clone()), (89.into(), 2.into()));
    let (_, v) = json(&count_ports(), &["count", "--regex", "hit_", "--path", "a/two.rs", "--path", "b/three.rs"]);
    assert_eq!((v["total"].clone(), v["files"].clone()), (70.into(), 2.into()));
}

#[test_id(tp3gfr8)]
#[test]
fn count_ignores_excluded_paths_and_prose_files() {
    let (_, v) = json(&count_ports(), &["count", "--regex", "hit_8$|hit_md", "--limit", "50"]);
    let paths: Vec<&str> = v["sites"].as_array().unwrap().iter().map(|s| s["path"].as_str().unwrap()).collect();
    assert!(!paths.iter().any(|p| p.contains("fixtures") || p.ends_with(".md")), "{paths:?}");
}

#[test_id(t1u2knx)]
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
    fn has_history(&self, _: &std::path::Path, _: &str) -> Result<bool, String> {
        Ok(false)
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
    fn renamed_since(&self, _: &str) -> Result<std::collections::BTreeMap<String, String>, String> {
        Ok(Default::default())
    }
    fn changed_lines(&self, _: &str, _: &str) -> Option<u32> {
        None
    }
}

#[test_id(tcw1twd)]
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

#[test_id(t6x6wzm)]
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

#[test_id(t706i8n)]
#[test]
fn count_limit_defaults_to_the_configured_key() {
    let (_, v) = json(&count_ports(), &["count", "--regex", "hit_"]);
    assert_eq!((v["sites"].as_array().unwrap().len(), v["more"].clone()), (7, 112.into()));
}

#[test_id(tyqk7z8)]
#[test]
fn count_json_matches_the_text_totals_and_names_the_commit() {
    let (code, v) = json(&count_ports(), &["count", "--regex", "hit_", "--limit", "2"]);
    assert_eq!(code, 0);
    assert_eq!((v["total"].clone(), v["files"].clone(), v["searched"].clone(), v["skipped_comments"].clone(), v["more"].clone()), (119.into(), 3.into(), 4.into(), 1.into(), 117.into()));
    assert_eq!(v["commit"], "beef01");
    assert_eq!(v["sites"][1], serde_json::json!({"path": "a/one.rs", "line": 3, "text": "let hit_1 = 1;"}));
}

#[test_id(t4ygjby)]
#[test]
fn count_comment_markers_follow_the_file_type() {
    let t = FixtureTree::new("c1").with_file("x.sh", "# hit\n  hit\n").with_file("y.py", "# hit\nhit\n").with_file("z.toml", "# hit\n").with_file("w.rs", "# hit\n// hit\n");
    let (_, v) = json(&ports_with(t, vec![], cfg()), &["count", "--regex", "hit"]);
    assert_eq!((v["total"].clone(), v["skipped_comments"].clone()), (3.into(), 4.into()));
}

#[test_id(tjdc8mk)]
#[test]
fn help_documents_count_and_its_exit_codes() {
    let o = go(&count_ports(), &["-h"]);
    for want in ["cr count", "--regex", "--path", "--limit"] {
        assert!(o.stdout.contains(want), "{want}");
    }
}

fn attended(comments: &[(&str, u32)]) -> Vec<Row> {
    let mut open = row(
        "persona",
        "persona:elsewhere",
        NOW - 3 * DAY,
        &[("elsewhere.md", "x")],
    );
    open.review = "r-attended".into();
    let how = Attention {
        comments_by_path: comments.iter().map(|(p, n)| (p.to_string(), *n)).collect(),
        finding_kinds: Default::default(),
    };
    vec![
        open.clone(),
        open.closing(NOW - 2 * DAY, ClosedAs::Published, Some(how)),
    ]
}

#[test_id(t0fd2bz)]
#[test]
fn his_comments_raise_weights_in_their_area_only() {
    let p = ports_with(fixture(), attended(&[("demo/src/old.rs", 3)]), cfg());
    let (_, plain) = json(&ports(), &["pick", "--aim", "demo/src/store.rs"]);
    let (_, demo) = json(&p, &["pick", "--aim", "demo/src/store.rs"]);
    let (_, other) = json(&p, &["pick", "--aim", "other/src/util.rs"]);
    assert_eq!(
        (
            demo["weight"]["attention_share"].as_f64(),
            demo["weight"]["attention_term"].as_f64()
        ),
        (Some(1.0), Some(2.0)),
        "{demo}"
    );
    assert_eq!(
        (
            other["weight"]["attention_share"].as_f64(),
            other["weight"]["attention_term"].as_f64()
        ),
        (Some(0.0), Some(1.0)),
        "{other}"
    );
    let (a, b) = (
        demo["weight"]["total"].as_f64().unwrap(),
        plain["weight"]["total"].as_f64().unwrap(),
    );
    assert!((a - 2.0 * b).abs() < 1e-9, "{a} vs {b}");
}

#[test_id(ty56wtg)]
#[test]
fn a_renamed_file_keeps_his_comments_in_its_new_area() {
    let mut rows = attended(&[("demo/src/old.rs", 1), ("other/x.rs", 1)]);
    for r in &mut rows {
        r.commit = "before".into();
    }
    let p = ports_with(
        fixture().with_rename("before", "demo/src/old.rs", "other/src/old.rs"),
        rows,
        cfg(),
    );
    let (_, v) = json(&p, &["pick", "--aim", "other/src/util.rs"]);
    assert_eq!(v["weight"]["attention_share"].as_f64(), Some(1.0), "{v}");
}

#[test_id(ty9zap7)]
#[test]
fn attention_counts_only_published_reviews() {
    let mut rows = attended(&[("demo/a.rs", 3)]);
    rows[1].closed_as = Some(ClosedAs::Unpublished);
    let (_, v) = json(
        &ports_with(fixture(), rows, cfg()),
        &["pick", "--aim", "demo/src/store.rs"],
    );
    assert_eq!(v["weight"]["attention_term"].as_f64(), Some(1.0), "{v}");
}

#[test_id(tm1aytu)]
#[test]
fn attention_does_not_move_the_rotation_of_kinds() {
    let mut rows = vec![row(
        "module",
        "module:elsewhere",
        NOW - DAY,
        &[("elsewhere.rs", "x")],
    )];
    rows[0].review = "r-module".into();
    rows.extend(
        attended(&[("demo/src/store.rs", 50)])
            .into_iter()
            .skip(1)
            .map(|mut r| {
                r.kind = "module".into();
                r
            }),
    );
    let p = ports_with(fixture(), rows, cfg());
    for seed in 0..20 {
        let (_, v) = json(&p, &["pick", "--seed", &seed.to_string()]);
        assert_ne!(v["unit"]["kind"], "module", "seed {seed}");
        assert_eq!(v["stratum"]["kind"], "trait");
    }
}

#[test_id(tki31r8)]
#[test]
fn the_weight_line_names_his_comments_in_the_candidates_area() {
    let p = ports_with(fixture(), attended(&[("demo/src/old.rs", 3)]), cfg());
    let o = go(&p, &["pick", "--aim", "demo/src/store.rs"]);
    assert!(o.stdout.contains("x attention 2.000 (3 of his 3 comments in demo)"), "{}", o.stdout);
}
