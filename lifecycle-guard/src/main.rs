mod brief;
mod finding;
mod landstate;
mod rules;
mod shell;

use finding::{Class, Finding};
use rules::{Rules, GATE_CLASSES};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut json = false;
    let mut gate = false;
    let mut rules_path: Option<PathBuf> = None;
    let mut paths: Vec<PathBuf> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--gate" => gate = true,
            "--rules" => {
                i += 1;
                match args.get(i) {
                    Some(p) => rules_path = Some(PathBuf::from(p)),
                    None => {
                        eprintln!("--rules requires a path");
                        return ExitCode::from(2);
                    }
                }
            }
            other => paths.push(PathBuf::from(other)),
        }
        i += 1;
    }

    if paths.is_empty() {
        eprintln!("usage: lifecycle-guard [--json] [--gate] [--rules <file>] <path>...");
        return ExitCode::from(2);
    }

    let rules = match rules_path {
        Some(p) => match Rules::load(&p) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(2);
            }
        },
        None => Rules::default(),
    };

    let mut findings = Vec::new();
    let mut scanned = 0usize;
    for root in &paths {
        match run_one(root, &rules, gate) {
            Ok((n, mut f)) => {
                scanned += n;
                findings.append(&mut f);
            }
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(2);
            }
        }
    }

    if gate {
        return gate_verdict(scanned, &findings);
    }

    if json {
        match serde_json::to_string_pretty(&findings) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("serializing findings: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        for f in &findings {
            println!("{f}");
        }
    }

    if findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// The landing gate's fence (`step "$SPIRA_GUARD_BIN" --gate .` in gate.steps, sp-ts2qr).
///
/// Refuses (exit 1) on any finding of a [`GATE_CLASSES`] class, wherever it is — there is no
/// allow-list and no flag that makes one — and names the exits (law-a-refusal-names-its-exit).
/// Exit 2 when it scanned nothing: a fence that cannot check refuses
/// (law-a-control-that-cannot-check-must-refuse). On success it prints the gate's fence line,
/// `fence: lifecycle-guard checked <n> files` (gate/DESIGN.md "Every fence proves it
/// checked"), and one line counting what the other classes still find, so their backlog is
/// visible at every gate without failing it.
fn gate_verdict(scanned: usize, findings: &[Finding]) -> ExitCode {
    if scanned == 0 {
        eprintln!(
            "lifecycle-guard: REFUSED — checked 0 files: the gate's tree has no shell, Rust or persona \
             file under the path given, so the lifecycle barrier cannot be judged. Run it from the \
             tree's root (gate.steps: step \"$SPIRA_GUARD_BIN\" --gate .)."
        );
        return ExitCode::from(2);
    }
    let (enforced, other): (Vec<&Finding>, Vec<&Finding>) =
        findings.iter().partition(|f| GATE_CLASSES.contains(&f.class));
    if !other.is_empty() {
        let mut counts: Vec<(Class, usize)> = Vec::new();
        for f in &other {
            match counts.iter_mut().find(|(c, _)| *c == f.class) {
                Some((_, n)) => *n += 1,
                None => counts.push((f.class, 1)),
            }
        }
        let summary: Vec<String> = counts.iter().map(|(c, n)| format!("{}={n}", c.as_str())).collect();
        println!(
            "lifecycle-guard: not yet refused at the gate: {} ({} finding(s); `lifecycle-guard .` lists them)",
            summary.join(" "),
            other.len()
        );
    }
    if enforced.is_empty() {
        println!("fence: lifecycle-guard checked {scanned} files");
        return ExitCode::SUCCESS;
    }
    for f in &enforced {
        println!("{f}");
    }
    eprintln!(
        "lifecycle-guard: REFUSED — {} finding(s) reach the landstate ledger or a landed oracle. \
         The lifecycle machine is the only route to a bead's state (design \
         bead-lifecycle-state-machine §3.6(3)), and this gate has no allow-list. Exits: (1) read \
         or change the state through spira-lc (show / list / state / the lifecycle verbs) instead; \
         (2) if the analyser is wrong, correct its rule in lifecycle-guard/ on this branch — the \
         tree owns its gate, so the branch is judged by the rule it carries; (3) to land without \
         a certificate, an operator sets SPIRA_LAND_UNGATED=<reason> (queue/DESIGN.md D12).",
        enforced.len()
    );
    ExitCode::from(1)
}

fn run_one(root: &Path, rules: &Rules, gate: bool) -> Result<(usize, Vec<Finding>), String> {
    let (shell_files, brief_files, rust_files) = discover(root, gate)?;
    let scanned = shell_files.len() + brief_files.len() + rust_files.len();
    let mut findings = shell::scan(&shell_files, root, rules)?.into_findings();
    findings.extend(brief::scan(&brief_files, root));
    findings.extend(landstate::scan_rust(&rust_files, root));
    Ok((scanned, findings))
}

/// Whether a directory under the scanned root is outside the tree being judged: the VCS's
/// own store and build output always (a gate tree holds `target/`, with copies of crates'
/// sources in its registry checkouts), and under `--gate` also every `tests/fixtures/`
/// directory — data a test feeds a tool, this crate's own planted violations among them,
/// never code that runs. A suite or a test module that reaches the ledger is still scanned.
fn pruned(rel: &Path, gate: bool) -> bool {
    let comps: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    let Some(last) = comps.last() else {
        return false;
    };
    if last == ".git" || (comps.len() == 1 && last == "target") {
        return true;
    }
    gate && comps.len() >= 2 && last == "fixtures" && comps[comps.len() - 2] == "tests"
}

fn discover(root: &Path, gate: bool) -> Result<(Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>), String> {
    let mut shell_files = Vec::new();
    let mut brief_files = Vec::new();
    let mut rust_files = Vec::new();

    if root.is_file() {
        classify(root, &mut shell_files, &mut brief_files, &mut rust_files);
        return Ok((shell_files, brief_files, rust_files));
    }

    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !(e.file_type().is_dir() && pruned(e.path().strip_prefix(root).unwrap_or(e.path()), gate)))
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            classify(entry.path(), &mut shell_files, &mut brief_files, &mut rust_files);
        }
    }
    shell_files.sort();
    brief_files.sort();
    rust_files.sort();
    Ok((shell_files, brief_files, rust_files))
}

fn classify(
    path: &Path,
    shell_files: &mut Vec<PathBuf>,
    brief_files: &mut Vec<PathBuf>,
    rust_files: &mut Vec<PathBuf>,
) {
    if path.extension().and_then(|e| e.to_str()) == Some("sh") {
        shell_files.push(path.to_path_buf());
        return;
    }
    if path.extension().and_then(|e| e.to_str()) == Some("rs") {
        rust_files.push(path.to_path_buf());
        return;
    }
    if matches!(path.extension().and_then(|e| e.to_str()), Some("md") | Some("fayth"))
        && brief::is_persona_path(path)
    {
        brief_files.push(path.to_path_buf());
        return;
    }
    if has_shell_shebang(path) {
        shell_files.push(path.to_path_buf());
    }
}

fn has_shell_shebang(path: &Path) -> bool {
    let Ok(content) = std::fs::read(path) else {
        return false;
    };
    if !content.starts_with(b"#!") {
        return false;
    }
    let first_line = content
        .split(|&b| b == b'\n')
        .next()
        .unwrap_or(&[]);
    let first_line = String::from_utf8_lossy(first_line);
    first_line.contains("bash") || first_line.ends_with("/sh")
}
