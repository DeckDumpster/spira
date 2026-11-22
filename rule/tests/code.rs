//! The statute code: the generator's output, the drift check, append numbering, `enact --part`.

#[macro_use]
extern crate spira_test_id;
use std::collections::BTreeMap;

use rule::code::{append, drift, generate, page_dates, parse_pointers, parse_toc, record_moves, render_pointers, render_toc, resolve, write_tree, Merged, Moved, Pointers, Retired};

const TOC: &str = r#"[[part]]
number = 1
name = "Alpha"
slug = "alpha"
scope = "First scope."
slugs = [
  "law-one",
  "law-two",
]

[[part]]
number = 2
name = "Beta"
slug = "beta"
scope = "Second scope."
slugs = [
  "law-three",
]
"#;

const TODAY: &str = "2026-11-22";

fn gen(parts: &[rule::code::Part], pointers: &Pointers, text_of: impl Fn(&str) -> Option<String>) -> BTreeMap<String, String> {
    generate(parts, pointers, text_of, &BTreeMap::new(), TODAY)
}

fn framed(tags: &str, body: &str) -> String {
    format!("---\ntype: note\ncreated: {TODAY}\nupdated: {TODAY}\ntags: [{tags}]\n---\n{body}")
}

fn book(slug: &str) -> Option<String> {
    match slug {
        "law-one" => Some("Do one. Then more.\n".into()),
        "law-two" => Some("Do two.".into()),
        "law-three" => Some("Do three. Always.\n".into()),
        _ => None,
    }
}

#[test_id(tp7vbg3)]
#[test]
fn three_statutes_in_two_parts_generate_the_expected_tree() {
    let _env = testkit::env_read();
    let parts = parse_toc(TOC).unwrap();
    let tree = generate(&parts, &Pointers::default(), book, &BTreeMap::new(), TODAY);
    let want: BTreeMap<&str, String> = BTreeMap::from([
        ("index.md", framed("law", "# Statute code\n\n- Part 1 — Alpha — First scope.\n- Part 2 — Beta — Second scope.\n")),
        ("1-alpha/index.md", framed("law, statute-part", "# Part 1 — Alpha\n\nFirst scope.\n\n- §1.1 law-one — Do one.\n- §1.2 law-two — Do two.\n")),
        ("1-alpha/1.1-law-one.md", framed("law, statute", "# §1.1 law-one\n\nDo one. Then more.\n")),
        ("1-alpha/1.2-law-two.md", framed("law, statute", "# §1.2 law-two\n\nDo two.\n")),
        ("2-beta/index.md", framed("law, statute-part", "# Part 2 — Beta\n\nSecond scope.\n\n- §2.1 law-three — Do three.\n")),
        ("2-beta/2.1-law-three.md", framed("law, statute", "# §2.1 law-three\n\nDo three. Always.\n")),
    ]);
    let got: BTreeMap<&str, String> = tree.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    assert_eq!(got, want);
}

#[test_id(t7lngxe)]
#[test]
fn a_regenerated_page_keeps_its_dates_until_its_body_changes() {
    let _env = testkit::env_read();
    let parts = parse_toc(TOC).unwrap();
    let first = generate(&parts, &Pointers::default(), book, &BTreeMap::new(), TODAY);
    let again = generate(&parts, &Pointers::default(), book, &first, "2026-12-01");
    assert_eq!(again, first);
    let changed = generate(&parts, &Pointers::default(), |s| if s == "law-two" { Some("Do two, differently.".into()) } else { book(s) }, &first, "2026-12-01");
    for (k, v) in &changed {
        if k == "1-alpha/1.2-law-two.md" || k == "1-alpha/index.md" {
            assert!(v.contains("created: 2026-11-22\nupdated: 2026-12-01\n"), "{v}");
        } else {
            assert_eq!(v, &first[k], "{k}");
        }
    }
    let bare: BTreeMap<String, String> = first.iter().map(|(k, v)| (k.clone(), v.split_once("---\n").unwrap().1.split_once("---\n").unwrap().1.to_string())).collect();
    let fresh = generate(&parts, &Pointers::default(), book, &bare, "2026-12-01");
    assert!(fresh.values().all(|v| v.contains("created: 2026-12-01\nupdated: 2026-12-01\n")));
    assert_eq!(page_dates(None, "b", "2026-12-01"), ("2026-12-01".to_string(), "2026-12-01".to_string()));
}

#[test_id(tj6g6wd)]
#[test]
fn every_generated_page_satisfies_the_frontmatter_schema() {
    let _env = testkit::env_read();
    let parts = parse_toc(TOC).unwrap();
    let tag = |t: &str| !t.is_empty() && t.split('-').all(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
    let date = |d: &str| d.len() == 10 && d.bytes().enumerate().all(|(i, b)| if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() });
    for (path, page) in gen(&parts, &Pointers::default(), book) {
        let rest = page.strip_prefix("---\n").unwrap_or_else(|| panic!("{path}: no block"));
        let block = rest.split_once("\n---\n").unwrap_or_else(|| panic!("{path}: unclosed")).0;
        let get = |k: &str| block.lines().find_map(|l| l.strip_prefix(&format!("{k}: "))).unwrap_or_else(|| panic!("{path}: no {k}"));
        assert_eq!(get("type"), "note");
        assert!(date(get("created")) && date(get("updated")), "{path}");
        let tags = get("tags").strip_prefix('[').and_then(|t| t.strip_suffix(']')).unwrap_or_else(|| panic!("{path}: tags not a list"));
        assert!(tags.split(", ").all(tag), "{path}: {tags}");
    }
}

#[test_id(thrcxc0)]
#[test]
fn toc_renders_to_itself() {
    let _env = testkit::env_read();
    assert_eq!(render_toc(&parse_toc(TOC).unwrap()), TOC);
}

#[test_id(t1ik5p4)]
#[test]
fn a_hand_edit_or_a_stray_file_is_drift() {
    let _env = testkit::env_read();
    let parts = parse_toc(TOC).unwrap();
    let tree = gen(&parts, &Pointers::default(), book);
    let dir = testkit::TempDir::new("law-drift");
    write_tree(&dir, TOC, &tree).unwrap();
    assert!(drift(&dir, &tree).is_empty());

    std::fs::write(dir.join("1-alpha/1.2-law-two.md"), "# §1.2 law-two\n\nDo two, edited.\n").unwrap();
    assert_eq!(drift(&dir, &tree), vec!["1-alpha/1.2-law-two.md: differs from what toc.toml generates"]);
    write_tree(&dir, TOC, &tree).unwrap();

    std::fs::write(dir.join("stray.md"), "x").unwrap();
    assert_eq!(drift(&dir, &tree), vec!["stray.md: not generated by toc.toml"]);
    write_tree(&dir, TOC, &tree).unwrap();
    assert!(drift(&dir, &tree).is_empty(), "write_tree must remove what the toc does not generate");

    std::fs::remove_file(dir.join("2-beta/index.md")).unwrap();
    assert_eq!(drift(&dir, &tree), vec!["2-beta/index.md: missing"]);
}

#[test_id(thce0ou)]
#[test]
fn append_adds_at_the_end_and_moves_nothing() {
    let _env = testkit::env_read();
    let parts = parse_toc(TOC).unwrap();
    let before = gen(&parts, &Pointers::default(), book);
    let after = append(&parts, 1, "law-four").unwrap();
    assert_eq!(after[0].slugs, ["law-one", "law-two", "law-four"]);
    let grown = gen(&after, &Pointers::default(), |s| book(s).or(Some("Four.".into())));
    for (path, body) in &before {
        if path != "index.md" && !path.ends_with("/index.md") {
            assert_eq!(grown.get(path), Some(body), "{path} moved");
        }
    }
    assert!(grown.contains_key("1-alpha/1.3-law-four.md"));
}

#[test_id(ta8dgbo)]
#[test]
fn append_refuses_an_unknown_part_and_a_slug_in_another_part() {
    let _env = testkit::env_read();
    let parts = parse_toc(TOC).unwrap();
    assert!(append(&parts, 9, "law-four").unwrap_err().contains("no Part 9"));
    assert!(append(&parts, 2, "law-one").unwrap_err().contains("Part 1"));
    assert_eq!(append(&parts, 1, "law-one").unwrap(), parts, "an amendment keeps its place");
}

#[test_id(ttzjq4x)]
#[test]
fn a_retired_slug_keeps_its_number() {
    let _env = testkit::env_read();
    let parts = parse_toc(TOC).unwrap();
    let gone = |s: &str| if s == "law-one" { None } else { book(s) };
    let tree = gen(&parts, &Pointers::default(), gone);
    assert!(tree["1-alpha/1.1-law-one.md"].contains("[Reserved]"));
    assert!(tree.contains_key("1-alpha/1.2-law-two.md"));

    let p = Pointers { retired: vec![Retired { slug: "law-one".into(), reason: "Superseded by hand.".into(), replaced_by: Some("law-three".into()), on: "2026-10-28".into() }], ..Pointers::default() };
    let tree = gen(&parts, &p, gone);
    assert_eq!(tree["1-alpha/1.1-law-one.md"], framed("law, statute", "# §1.1 law-one\n\n[Retired 2026-10-28] Superseded by hand.\n\nReplaced by §2.1 law-three.\n"));
    assert!(!tree["1-alpha/1.1-law-one.md"].contains("[Reserved]"));
}

#[test_id(tlmtzwi)]
#[test]
fn a_retired_slug_leaves_the_toc_and_its_pointer_stays_and_the_later_sections_move() {
    let _env = testkit::env_read();
    let toc = parse_toc(TOC).unwrap();
    let p = Pointers { retired: vec![Retired { slug: "law-one".into(), reason: "Gone.".into(), replaced_by: Some("law-three".into()), on: "2026-10-28".into() }], ..Pointers::default() };
    let held = store(&["law-two", "law-three"]);
    let landed = BTreeMap::from([("law-one".to_string(), "1.1".to_string()), ("law-two".to_string(), "1.2".to_string()), ("law-three".to_string(), "2.1".to_string())]);
    let (toc, p) = rule::code::in_force(&toc, &p, &held, &landed, "2026-11-20").unwrap();
    assert_eq!(toc[0].slugs, ["law-two"]);
    assert_eq!(p.moved, [Moved { section: "1.2".into(), slug: "law-two".into(), on: "2026-11-20".into() }]);
    assert!(rule::code::check_pointers(&toc, &p).is_empty());
    let r = resolve(&toc, &p, "1.1").unwrap();
    assert_eq!(r.slug, "law-two", "a current section wins over a pointer");
    assert_eq!(resolve(&toc, &p, "law-one").unwrap().slug, "law-three");
}

#[test_id(tsqu5c1)]
#[test]
fn a_merged_slug_renders_where_it_went() {
    let _env = testkit::env_read();
    let parts = parse_toc(TOC).unwrap();
    let p = Pointers { merged: vec![Merged { from: "law-one".into(), into: "law-three".into(), reason: "Same rule.".into(), on: "2026-10-28".into() }], ..Pointers::default() };
    let tree = gen(&parts, &p, |s| if s == "law-one" { None } else { book(s) });
    assert_eq!(tree["1-alpha/1.1-law-one.md"], framed("law, statute", "# §1.1 law-one\n\n[Merged into §2.1 law-three] Same rule.\n"));
}

#[test_id(tyb9llx)]
#[test]
fn pointers_render_to_themselves_and_refuse_what_they_do_not_know() {
    let _env = testkit::env_read();
    let p = Pointers {
        moved: vec![Moved { section: "1.2".into(), slug: "law-two".into(), on: "2026-10-28".into() }],
        merged: vec![Merged { from: "law-one".into(), into: "law-two".into(), reason: "Same.".into(), on: "2026-10-28".into() }],
        retired: vec![Retired { slug: "law-three".into(), reason: "Gone.".into(), replaced_by: None, on: "2026-10-28".into() }],
    };
    assert_eq!(parse_pointers(&render_pointers(&p)).unwrap(), p);
    assert_eq!(render_pointers(&parse_pointers(&render_pointers(&p)).unwrap()), render_pointers(&p));
    assert!(parse_pointers("version = 2\n").unwrap_err().contains("version"));
    assert!(parse_pointers("").unwrap_err().contains("version"));
    assert!(parse_pointers("version = 1\nextra = 1\n").unwrap_err().contains("extra"));
    assert!(parse_pointers("version = 1\n[[moved]]\nsection = \"1.1\"\nslug = \"a\"\non = \"d\"\nwhy = \"x\"\n").unwrap_err().contains("why"));
}

fn ptrs(toml_rows: &str) -> Pointers {
    parse_pointers(&format!("version = 1\n{toml_rows}")).unwrap()
}

#[test_id(tviqcfr)]
#[test]
fn a_retired_slug_with_no_replacement_ends_the_chain_with_its_reason() {
    let _env = testkit::env_read();
    let toc = parse_toc(TOC).unwrap();
    let p = ptrs("[[retired]]\nslug = \"law-three\"\nreason = \"Obsolete.\"\non = \"d\"\n");
    let r = resolve(&toc, &p, "2.1").unwrap();
    assert_eq!(r.retired.unwrap().reason, "Obsolete.");
}

#[test_id(t1tnnol)]
#[test]
fn a_toc_that_drops_a_numbered_slug_is_refused_by_name() {
    let _env = testkit::env_read();
    let old = BTreeMap::from([("law-one".to_string(), "1.1".to_string())]);
    let toc = parse_toc(&TOC.replace("  \"law-one\",\n", "")).unwrap();
    let held = BTreeMap::from([("law-one".to_string(), "Do one.".to_string())]);
    assert!(record_moves(&old, &toc, &Pointers::default(), "d", &held).unwrap_err().contains("law-one"));
    assert!(record_moves(&old, &toc, &Pointers::default(), "d", &BTreeMap::new()).unwrap().moved.is_empty(), "a slug the store lacks may leave");
}

#[test_id(t5hy2gr)]
#[test]
fn a_move_records_the_old_number_once() {
    let _env = testkit::env_read();
    let old = BTreeMap::from([("law-one".to_string(), "1.1".to_string()), ("law-two".to_string(), "1.2".to_string())]);
    let toc = parse_toc(&TOC.replace("  \"law-two\",\n", "").replace("  \"law-three\",", "  \"law-three\",\n  \"law-two\",")).unwrap();
    let p = record_moves(&old, &toc, &Pointers::default(), "2026-10-28", &store(&["law-one", "law-two"])).unwrap();
    assert_eq!(p.moved, [Moved { section: "1.2".into(), slug: "law-two".into(), on: "2026-10-28".into() }]);
    assert_eq!(record_moves(&old, &toc, &p, "later", &store(&["law-one", "law-two"])).unwrap(), p);
}

#[test_id(t545n6i)]
#[test]
fn a_slug_in_two_parts_is_refused() {
    let _env = testkit::env_read();
    let bad = format!("{TOC}\n[[part]]\nnumber = 3\nname = \"C\"\nslug = \"c\"\nscope = \"s\"\nslugs = [\"law-one\"]\n");
    assert!(parse_toc(&bad).unwrap_err().contains("law-one"));
}

#[test_id(tbvbfql)]
#[test]
fn a_trailing_citation_stays_out_of_the_shipped_section_and_a_midtext_parenthesis_stays_in() {
    let _env = testkit::env_read();
    let parts = parse_toc(TOC).unwrap();
    let tree = gen(&parts, &Pointers::default(), |slug| match slug {
        "law-one" => Some("Do one. (per the case that produced it: details.)\n".into()),
        "law-two" => Some("Do two (per the spec) always. (sp-abc123.1: the scar, told at length.)".into()),
        "law-three" => Some("Do three (per the spec) always. (spa-x: not a bead) (per a ruling) (sp-abc123: the scar)".into()),
        _ => None,
    });
    assert_eq!(tree["1-alpha/1.1-law-one.md"], framed("law, statute", "# §1.1 law-one\n\nDo one.\n"));
    assert_eq!(tree["1-alpha/1.2-law-two.md"], framed("law, statute", "# §1.2 law-two\n\nDo two (per the spec) always.\n"));
    assert_eq!(tree["2-beta/2.1-law-three.md"], framed("law, statute", "# §2.1 law-three\n\nDo three (per the spec) always. (spa-x: not a bead)\n"));
}

#[test_id(ttle7bw)]
#[test]
fn behind_names_store_slugs_the_toc_and_pointers_do_not_account_for() {
    let _env = testkit::env_read();
    let toc = parse_toc(TOC).unwrap();
    let p = ptrs("[[retired]]\nslug = \"law-five\"\nreason = \"Gone.\"\non = \"d\"\n");
    let store: BTreeMap<String, String> = ["law-one", "law-three", "law-four", "law-five", "law-six", "sop-x"].iter().map(|s| (s.to_string(), String::new())).collect();
    assert_eq!(rule::code::behind(&toc, &p, &store), ["law-four", "law-six"]);
}

fn store(slugs: &[&str]) -> BTreeMap<String, String> {
    slugs.iter().map(|s| (s.to_string(), format!("Do {s}."))).collect()
}

#[test_id(tk2i1zn)]
#[test]
fn catch_up_appends_every_behind_slug_and_numbers_the_enacted_key_in_its_part() {
    let _env = testkit::env_read();
    let toc = parse_toc(TOC).unwrap();
    let book = store(&["law-one", "law-two", "law-three", "law-four", "law-five", "law-six"]);
    let (out, _) = rule::code::catch_up(&toc, &Pointers::default(), &book, Some(("law-six", 2)), &BTreeMap::new(), "d").unwrap();
    assert_eq!(out[0].slugs, ["law-one", "law-two", "law-five", "law-four"].map(String::from));
    assert_eq!(out[1].slugs, ["law-three", "law-six"].map(String::from));
    assert!(rule::code::behind(&out, &Pointers::default(), &book).is_empty());
    assert_eq!(out.iter().map(|p| p.slugs.len()).sum::<usize>(), 6);
}

#[test_id(tjo1wgd)]
#[test]
fn the_law_book_log_is_watched() {
    let _env = testkit::env_read();
    let rows = include_str!("../../spira/watchers");
    assert!(rows.lines().any(|l| l.starts_with("law-book|log|")), "no law-book log row in spira/watchers");
}

#[test_id(t2df1u9)]
#[test]
fn a_dated_provenance_mark_never_reaches_a_rendered_file() {
    let _env = testkit::env_read();
    let toc = parse_toc("[[part]]\nnumber = 1\nname = \"A\"\nslug = \"a\"\nscope = \"s\"\nslugs = [\"law-one\", \"law-two\", \"law-three\"]\n").unwrap();
    let mark = |who: &str, day: &str| format!("(per {who}, 2026-{day})");
    let tree = gen(&toc, &Pointers::default(), |slug| match slug {
        "law-one" => Some(format!("Do one {}.", mark("Ann", "09-29"))),
        "law-two" => Some(format!("Do two {}. Then three.", mark("Ann", "10-15"))),
        "law-three" => Some("Do four (per the spec) always.".into()),
        _ => None,
    });
    let flat = |k: &str| tree[k].split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(flat("1-a/1.1-law-one.md").ends_with("Do one."));
    assert!(flat("1-a/1.2-law-two.md").ends_with("Do two. Then three."));
    assert!(flat("1-a/1.3-law-three.md").ends_with("Do four (per the spec) always."));
    let pat = spira_lint::rules::inventory::pattern_re(&[]).unwrap();
    for (f, content) in &tree {
        assert!(spira_lint::rules::inventory::scan(content.as_bytes(), &pat).is_empty(), "{f}: {content}");
    }
}

fn retired_record(replaced_by: &str) -> String {
    sop::shelf::Retired { by: "t".into(), at: "2026-11-20T10:00:00Z".into(), epoch: 1, reason: "renamed out of retired vocabulary".into(), text: "Do it.".into(), replaced_by: Some(replaced_by.into()) }.to_json()
}


#[test_id(t8k33lx)]
#[test]
fn store_pointers_reads_one_row_per_retirement_record_and_never_a_second_for_a_pointed_slug() {
    let _env = testkit::env_read();
    let mut held = store(&["law-one", "law-dwarf-x"]);
    held.insert("retired-law-aeon-x".into(), retired_record("law-dwarf-x")); // ffx-ok
    held.insert("retired-sop-aeon-y".into(), retired_record("sop-dwarf-y")); // ffx-ok
    let rows = rule::code::store_pointers(&held);
    assert_eq!(rows, [Retired { slug: "law-aeon-x".into(), reason: "renamed out of retired vocabulary".into(), replaced_by: Some("law-dwarf-x".into()), on: "2026-11-20".into() }]); // ffx-ok

    let have = Pointers { retired: vec![Retired { slug: "law-aeon-x".into(), reason: "by hand".into(), replaced_by: None, on: "d".into() }], ..Pointers::default() }; // ffx-ok
    assert_eq!(rule::code::with_store_pointers(have.clone(), &held), have);
    assert_eq!(rule::code::with_store_pointers(Pointers::default(), &held).retired, rows);
}

fn change_toc(slugs: &[&str]) -> Vec<rule::code::Part> {
    let mut toc = parse_toc(TOC).unwrap();
    toc[0].slugs = slugs.iter().map(|s| s.to_string()).collect();
    toc.truncate(1);
    toc
}

fn moved(slug: &str) -> Moved {
    Moved { section: "1.1".into(), slug: slug.into(), on: "2026-01-01".into() }
}

fn retired(slug: &str) -> Retired {
    Retired { slug: slug.into(), reason: "r".into(), replaced_by: None, on: "2026-01-01".into() }
}

#[test_id(tzwvr94)]
#[test]
fn a_book_change_names_the_slugs_it_adds_and_drops_and_counts_the_records_it_writes() {
    let _env = testkit::env_read();
    let before = change_toc(&["law-one", "law-two", "law-three"]);
    let after = change_toc(&["law-two", "law-three", "law-four"]);
    let old = Pointers { moved: vec![moved("law-two")], ..Pointers::default() };
    let new = Pointers { moved: vec![moved("law-two"), moved("law-three")], retired: vec![retired("law-one")], ..Pointers::default() };
    let change = rule::code::book_change(&before, &old, &after, &new);
    assert_eq!((change.added.as_slice(), change.dropped.as_slice()), (&["law-four".to_string()][..], &["law-one".to_string()][..]));
    assert_eq!((change.moved, change.retired, change.merged), (1, 1, 0));
    let said = change.to_string();
    assert!(said.contains("law-four") && said.contains("law-one") && said.contains("1 move(s), 1 retirement(s)"), "{said}");
}

#[test_id(tun5x30)]
#[test]
fn an_unchanged_book_says_no_statute_was_added_or_dropped() {
    let _env = testkit::env_read();
    let toc = change_toc(&["law-one", "law-two"]);
    let ptrs = Pointers { moved: vec![moved("law-one")], ..Pointers::default() };
    let change = rule::code::book_change(&toc, &ptrs, &toc, &ptrs);
    assert_eq!(change.to_string(), "no statute added or dropped, no pointer record written");
}

#[test_id(ty7zywz)]
#[test]
fn code_check_names_every_toc_slug_the_store_lacks() {
    let _env = testkit::env(&[]);
    let world = World::new("rule-check-names", "wiki/law", &["law-one", "law-two", "law-three"]);
    world.brain_with_remote();
    let elsewhere = testkit::TempDir::new("rule-check-names-cwd");
    let texts = world.texts.path().to_str().unwrap().to_string();
    let run = |args: &[&str]| world.rule(&[&["code"], args, &["--texts", &texts]].concat(), elsewhere.path());
    assert_eq!(run(&[]).status.code(), Some(0));
    std::fs::remove_file(world.texts.path().join("law-two.txt")).unwrap();
    std::fs::remove_file(world.texts.path().join("law-three.txt")).unwrap();
    let out = run(&["--check"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(said(&out).contains("law/toc.toml: lists statutes the store lacks: law-two, law-three\n"), "{}", said(&out)); // ffx-ok
}

struct World {
    dir: testkit::TempDir,
    texts: testkit::TempDir,
    toml: std::path::PathBuf,
    harness: std::path::PathBuf,
}

impl World {
    fn new(tag: &str, law_book: &str, slugs: &[&str]) -> World {
        let dir = testkit::TempDir::new(tag);
        let map = spira_config::process::fixture_repo_map(dir.path(), &["fixture"]);
        let wiki = dir.path().join("brain");
        let texts = testkit::TempDir::new(&format!("{tag}-texts"));
        for slug in slugs {
            std::fs::write(texts.path().join(format!("{slug}.txt")), format!("Do {slug}.")).unwrap();
        }
        let run = dir.path().join("run");
        std::fs::create_dir_all(&run).unwrap();
        let toml = spira_config::process::fixture_toml(
            dir.path(),
            &[
                ("SPIRA_REPO_MAP", map.to_str().unwrap()),
                ("SPIRA_HOME_REPO", "fixture"),
                ("SPIRA_WIKI", wiki.to_str().unwrap()),
                ("SPIRA_LAW_BOOK", law_book),
                ("SPIRA_STATUTE_DIR", texts.path().to_str().unwrap()),
                ("SPIRA_RUN", run.to_str().unwrap()),
            ],
        );
        let harness = spira_config::resolve::locate_home_for_process().unwrap();
        World { dir, texts, toml, harness }
    }

    fn wiki(&self) -> std::path::PathBuf {
        self.dir.path().join("brain")
    }

    fn book(&self) -> std::path::PathBuf {
        self.wiki().join("wiki/law")
    }

    fn brain_with_remote(&self) -> std::path::PathBuf {
        let bare = self.dir.path().join("remote.git");
        testkit::git(self.dir.path(), &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()]);
        let w = self.wiki();
        std::fs::create_dir_all(w.join("wiki/law")).unwrap();
        std::fs::create_dir_all(w.join("wiki/notes")).unwrap();
        testkit::git(&w, &["init", "-q", "-b", "main"]);
        testkit::git(&w, &["config", "user.email", "t@example.com"]);
        testkit::git(&w, &["config", "user.name", "t"]);
        testkit::git(&w, &["remote", "add", "origin", bare.to_str().unwrap()]);
        std::fs::write(w.join("wiki/law/toc.toml"), TOC).unwrap();
        let hook = bare.join("hooks/pre-receive");
        testkit::write_exe(
            &hook,
            r#"#!/bin/sh
bad=""
while read old new ref; do
  [ "$new" = 0000000000000000000000000000000000000000 ] && continue
  for f in $(git ls-tree -r --name-only "$new" | grep '^wiki/.*\.md$'); do
    page=$(git show "$new:$f")
    head=$(printf '%s\n' "$page" | sed -n '1p')
    ok=1
    [ "$head" = "---" ] || ok=0
    for k in type created updated tags; do
      printf '%s\n' "$page" | sed -n '2,/^---$/p' | grep -q "^$k: " || ok=0
    done
    [ $ok = 1 ] || bad="$bad$f  no valid frontmatter block\n"
  done
done
if [ -n "$bad" ]; then
  echo "REJECTED — refs/heads/main carries wiki pages that break the frontmatter schema:"
  printf "$bad"
  exit 1
fi
"#,
        );
        std::fs::write(w.join("wiki/notes/x.md"), "---\ntype: note\ncreated: 2026-01-01\nupdated: 2026-01-01\ntags: [x]\n---\nx\n").unwrap();
        testkit::git(&w, &["add", "-A"]);
        testkit::git(&w, &["commit", "-q", "-m", "seed"]);
        testkit::git(&w, &["push", "-q", "-u", "origin", "main"]);
        bare
    }

    fn rule(&self, args: &[&str], cwd: &std::path::Path) -> std::process::Output {
        std::process::Command::new(testkit::bin_exe("rule"))
            .args(["--home", self.harness.to_str().unwrap()])
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("SPIRA_TOML", &self.toml)
            .env("SPIRA_HOME", &self.harness)
            .env("SPIRA_RELEASE", &self.harness)
            .env("HOME", self.dir.path())
            .output()
            .unwrap()
    }
}

fn said(o: &std::process::Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

#[test_id(tipyxzt)]
#[test]
fn code_land_commits_and_pushes_the_book_in_the_wiki_and_leaves_the_home_repo_untouched() {
    let _env = testkit::env(&[]);
    let world = World::new("rule-land-book", "wiki/law", &["law-one", "law-two", "law-three", "law-extra"]);
    let home_repo = world.dir.path().join("fixture");
    std::fs::create_dir_all(&home_repo).unwrap();
    testkit::git(&home_repo, &["init", "-q", "-b", "main"]);
    std::fs::write(home_repo.join("f"), "f").unwrap();
    testkit::git(&home_repo, &["add", "-A"]);
    testkit::git(&home_repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "base"]);
    testkit::git(&home_repo, &["branch", "local/main"]);
    let bare = world.brain_with_remote();
    let w = world.wiki();
    std::fs::write(w.join("wiki/notes/x.md"), "---\ntype: note\ncreated: 2026-01-01\nupdated: 2026-01-01\ntags: [x]\n---\nmodified\n").unwrap();
    let before = (testkit::git(&home_repo, &["for-each-ref"]), testkit::git(&home_repo, &["worktree", "list"]));
    let elsewhere = testkit::TempDir::new("rule-land-book-cwd");

    let out = world.rule(&["code", "--land"], elsewhere.path());
    assert_eq!(out.status.code(), Some(0), "{}", said(&out));
    assert_eq!(testkit::git(&w, &["log", "-1", "--format=%s"]), "law: code book");
    let names = testkit::git(&w, &["show", "--name-only", "--format=", "HEAD"]);
    assert!(names.lines().all(|l| l.starts_with("wiki/law/")), "{names}");
    assert!(names.contains("law-extra.md"), "{names}");
    assert_eq!(testkit::git(&bare, &["rev-parse", "main"]), testkit::git(&w, &["rev-parse", "HEAD"]));
    assert_eq!(testkit::git(&w, &["status", "--porcelain"]), "M wiki/notes/x.md");
    assert_eq!((testkit::git(&home_repo, &["for-each-ref"]), testkit::git(&home_repo, &["worktree", "list"])), before);

    let head = testkit::git(&w, &["rev-parse", "HEAD"]);
    let again = world.rule(&["code", "--land"], elsewhere.path());
    assert_eq!(again.status.code(), Some(0), "{}", said(&again));
    assert!(said(&again).contains("law: book is up to date with the store."), "{}", said(&again));
    assert_eq!(testkit::git(&w, &["rev-parse", "HEAD"]), head);
}

#[test_id(t4x4h0c)]
#[test]
fn code_check_reads_the_book_in_the_configured_wiki() {
    let _env = testkit::env(&[]);
    let world = World::new("rule-check-book", "wiki/law", &["law-one", "law-two", "law-three"]);
    world.brain_with_remote();
    let elsewhere = testkit::TempDir::new("rule-check-book-cwd");
    let texts = world.texts.path().to_str().unwrap().to_string();
    let run = |args: &[&str]| world.rule(&[&["code"], args, &["--texts", &texts]].concat(), elsewhere.path());
    assert_eq!(run(&[]).status.code(), Some(0));
    let clean = run(&["--check"]);
    assert_eq!(clean.status.code(), Some(0), "{}", said(&clean));

    let book = world.book();
    let section = std::fs::read_dir(book.join("1-alpha")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).find(|n| n.contains("law-one")).unwrap();
    std::fs::write(book.join("1-alpha").join(&section), "edited\n").unwrap();
    std::fs::write(book.join("stray.md"), "x").unwrap();
    let drifted = run(&["--check"]);
    assert_eq!(drifted.status.code(), Some(1));
    let text = said(&drifted);
    assert!(text.contains(&format!("{section}: differs")), "{text}");
    assert!(text.contains("stray.md: not generated by toc.toml"), "{text}");

    assert_eq!(run(&[]).status.code(), Some(0));
    std::fs::write(world.texts.path().join("law-x.txt"), "Do x.").unwrap();
    let behind = run(&["--check"]);
    assert_eq!(behind.status.code(), Some(1));
    assert!(said(&behind).contains("behind the store by 1: law-x"), "{}", said(&behind));
}

#[test_id(tt3vhsr)]
#[test]
fn code_refuses_by_name_when_the_book_is_not_configured() {
    let _env = testkit::env(&[]);
    let world = World::new("rule-unconfigured", "", &["law-one"]);
    world.brain_with_remote();
    let elsewhere = testkit::TempDir::new("rule-unconfigured-cwd");
    let out = world.rule(&["code", "--check", "--texts", world.texts.path().to_str().unwrap()], elsewhere.path());
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("SPIRA_LAW_BOOK"), "{}", said(&out));
}

#[test_id(tqmslin)]
#[test]
fn show_resolves_a_section_through_the_wiki_book_and_follows_one_moved_hop() {
    let _env = testkit::env(&[]);
    let world = World::new("rule-show-book", "wiki/law", &["law-one", "law-two", "law-three"]);
    world.brain_with_remote();
    std::fs::write(world.book().join("pointers.toml"), "version = 1\n\n[[moved]]\nsection = \"9.9\"\nslug = \"law-two\"\non = \"2026-01-01\"\n").unwrap();
    let elsewhere = testkit::TempDir::new("rule-show-book-cwd");
    let current = world.rule(&["show", "1.2"], elsewhere.path());
    assert_eq!(current.status.code(), Some(0), "{}", said(&current));
    assert_eq!(String::from_utf8_lossy(&current.stdout).trim(), "Do law-two.");
    let moved = world.rule(&["show", "9.9"], elsewhere.path());
    assert_eq!(moved.status.code(), Some(0), "{}", said(&moved));
    assert_eq!(String::from_utf8_lossy(&moved.stdout).trim(), "§9.9 moved: law-two is now §1.2\nDo law-two.");
}

#[test_id(tj78yi7)]
#[test]
fn code_land_records_a_statute_renamed_in_the_store_as_a_pointer_and_commits_only_the_book() {
    let _env = testkit::env(&[]);
    let world = World::new("rule-land-renamed", "wiki/law", &["law-one", "law-aeon-x", "law-two"]); // ffx-ok
    world.brain_with_remote();
    let w = world.wiki();
    std::fs::write(w.join("wiki/law/toc.toml"), "[[part]]\nnumber = 1\nname = \"Alpha\"\nslug = \"alpha\"\nscope = \"First scope.\"\nslugs = [\n  \"law-one\",\n  \"law-aeon-x\",\n  \"law-two\",\n]\n").unwrap(); // ffx-ok
    let elsewhere = testkit::TempDir::new("rule-land-renamed-cwd");
    let landed = world.rule(&["code", "--land"], elsewhere.path());
    assert_eq!(landed.status.code(), Some(0), "{}", said(&landed));

    std::fs::remove_file(world.texts.path().join("law-aeon-x.txt")).unwrap(); // ffx-ok
    std::fs::write(world.texts.path().join("law-dwarf-x.txt"), "Do it.").unwrap();
    std::fs::write(world.texts.path().join("retired-law-aeon-x.txt"), retired_record("law-dwarf-x")).unwrap(); // ffx-ok
    let out = world.rule(&["code", "--land"], elsewhere.path());
    assert_eq!(out.status.code(), Some(0), "{}", said(&out));
    assert_eq!(testkit::git(&w, &["log", "-1", "--format=%s"]), "law: code book");
    let names = testkit::git(&w, &["show", "--name-only", "--format=", "HEAD"]);
    assert!(names.lines().all(|l| l.starts_with("wiki/law/")), "{names}");

    let toc = parse_toc(&std::fs::read_to_string(world.book().join("toc.toml")).unwrap()).unwrap();
    assert_eq!(toc[0].slugs, ["law-one", "law-dwarf-x", "law-two"]);
    assert!(world.book().join("1-alpha/1.2-law-dwarf-x.md").exists());
    assert!(!testkit::git(&w, &["ls-files", "wiki/law"]).contains("aeon")); // ffx-ok
    let pointers = parse_pointers(&std::fs::read_to_string(world.book().join("pointers.toml")).unwrap()).unwrap();
    assert_eq!((pointers.retired[0].slug.as_str(), pointers.retired[0].replaced_by.as_deref()), ("law-aeon-x", Some("law-dwarf-x"))); // ffx-ok
    let clean = world.rule(&["code", "--check"], elsewhere.path());
    assert_eq!(clean.status.code(), Some(0), "{}", said(&clean));
}
