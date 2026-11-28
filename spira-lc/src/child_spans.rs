//! Every process lc-serve spawns on a request is timed as a `child` phase; this reads the crate's
//! own sources, so a new spawn site that forgets `phases::child(` fails here.

use std::path::Path;

const SPAWNS: [&str; 6] = [
    "Command::new(",
    "bounded::bounded(",
    "bounded::bounded_for(",
    "proc::command(",
    "proc::bounded(",
    "proc::bounded_for(",
];
const WRAPPERS: [&str; 2] = ["spawn(", "spawn_with("];

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

fn without_test_modules(text: &str) -> String {
    let mut out = String::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if !(line.trim().starts_with("#[cfg(") && line.contains("test")) {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let mut depth = 0i32;
        let mut opened = false;
        for l in lines.by_ref() {
            depth += l.matches('{').count() as i32 - l.matches('}').count() as i32;
            opened |= l.contains('{');
            if (opened && depth <= 0) || (!opened && l.trim_end().ends_with(';')) {
                break;
            }
        }
    }
    out
}

fn is_fn_start(line: &str) -> bool {
    let t = line.trim_start();
    ["fn ", "pub fn ", "pub(crate) fn ", "pub(super) fn "]
        .iter()
        .any(|p| t.starts_with(p))
}

/// The spawn sites in `text` whose function neither wraps them in `phases::child(` nor hands the
/// built command to `spawn`/`spawn_with`; a function returning a `Command` is judged by its file.
fn unwrapped(text: &str) -> Vec<String> {
    if !SPAWNS.iter().any(|s| text.contains(s)) {
        return Vec::new();
    }
    let text = without_test_modules(text);
    let lines: Vec<&str> = text.lines().collect();
    let mut bad = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if is_comment(line) || !SPAWNS.iter().any(|s| line.contains(s)) {
            continue;
        }
        let start = (0..=i).rev().find(|j| is_fn_start(lines[*j])).unwrap_or(0);
        let end = (i + 1..lines.len())
            .find(|j| is_fn_start(lines[*j]))
            .unwrap_or(lines.len());
        let body = lines[start..end].join("\n");
        let sites = lines[start..end].iter().filter(|l| !is_comment(l) && SPAWNS.iter().any(|s| l.contains(s))).count();
        let wraps = body.matches("phases::child(").count() + WRAPPERS.iter().map(|w| body.matches(&format!(" {w}")).count() + body.matches(&format!("({w}")).count()).sum::<usize>();
        let builds_only = ["-> Command", "-> std::process::Command", "-> Result<Command", "-> std::io::Result<spira_config::bounded::Bounded>"].iter().any(|r| lines[start].contains(r));
        let wrapped = wraps >= sites || (builds_only && text.contains("phases::child("));
        if !wrapped {
            bad.push(line.trim().to_string());
        }
    }
    bad
}

fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if name != "tests" {
                sources(&p, out);
            }
        } else if name.ends_with(".rs")
            && name != "tests.rs"
            && !name.ends_with("_tests.rs")
            && name != "child_spans.rs"
            && name != "proc.rs"
        {
            out.push(p);
        }
    }
}

#[test_id(txvdfxn)]
#[test]
fn every_spawn_site_runs_inside_a_child_phase() {
    let mut files = Vec::new();
    sources(
        &testkit::manifest_dir().join("src"),
        &mut files,
    );
    let mut bad = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        bad.extend(
            unwrapped(&text)
                .into_iter()
                .map(|l| format!("{}: {l}", f.display())),
        );
    }
    assert!(
        bad.is_empty(),
        "spawn sites outside phases::child(:\n{}",
        bad.join("\n")
    );
}

#[test_id(timawbt)]
#[test]
fn the_matcher_finds_planted_spawns_and_passes_wrapped_ones() {
    let planted = "fn a() {\n    let o = Command::new(\"git\").output();\n}\nfn b() {\n    let o = bounded::bounded(\"git\").output();\n}\nfn c() {\n    let o = bounded::bounded_for(\"git\", 1).output();\n}\n";
    assert_eq!(unwrapped(planted).len(), 3);
    let doored = "fn a() {\n    let o = proc::command(\"git\").unwrap().output();\n}\nfn b() {\n    let o = crate::proc::bounded_for(\"git\", \"1\");\n}\n";
    assert_eq!(unwrapped(doored).len(), 2);
    let mixed = "fn a() {\n    state_store::phases::child(\"git\", || Command::new(\"git\").output());\n    Command::new(\"git\").output();\n}\n";
    assert_eq!(unwrapped(mixed).len(), 2, "one wrapped site must not hide an unwrapped one");
    let ok = "fn a() {\n    let o = state_store::phases::child(\"git\", || Command::new(\"git\").output());\n}\nfn b(cmd: Command) {\n    let c = Command::new(\"x\");\n    spawn(c);\n}\n#[cfg(test)]\nmod t {\n    fn x() { Command::new(\"git\"); }\n}\n";
    assert!(unwrapped(ok).is_empty(), "{:?}", unwrapped(ok));
}
