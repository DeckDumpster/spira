mod brief;
mod finding;
mod landstate;
mod rules;
mod shell;

use finding::Finding;
use rules::Rules;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut json = false;
    let mut rules_path: Option<PathBuf> = None;
    let mut paths: Vec<PathBuf> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
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
        eprintln!("usage: lifecycle-guard [--json] [--rules <file>] <path>...");
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
    for root in &paths {
        match run_one(root, &rules) {
            Ok(mut f) => findings.append(&mut f),
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(2);
            }
        }
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

fn run_one(root: &Path, rules: &Rules) -> Result<Vec<Finding>, String> {
    let (shell_files, brief_files, rust_files) = discover(root)?;
    let mut findings = shell::scan(&shell_files, root, rules)?.into_findings();
    findings.extend(brief::scan(&brief_files, root));
    findings.extend(landstate::scan_rust(&rust_files, root));
    Ok(findings)
}

fn discover(root: &Path) -> Result<(Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>), String> {
    let mut shell_files = Vec::new();
    let mut brief_files = Vec::new();
    let mut rust_files = Vec::new();

    if root.is_file() {
        classify(root, &mut shell_files, &mut brief_files, &mut rust_files);
        return Ok((shell_files, brief_files, rust_files));
    }

    for entry in walkdir::WalkDir::new(root)
        .into_iter()
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
    if matches!(path.extension().and_then(|e| e.to_str()), Some("md") | Some("fayth")) {
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
