//! What the tree tells agents to type must be something the tool accepts: no brief passes
//! `mail send` an option `run_send` refuses, and nothing calls the retired `cockpit/ask.sh`.

use std::fs;
use std::path::{Path, PathBuf};

const MAIN: &str = include_str!("main.rs");

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn accepted_send_options() -> Vec<String> {
    let body = MAIN.split("fn run_send(").nth(1).and_then(|r| r.split("\nfn ").next()).unwrap_or_default();
    body.lines().filter_map(|l| l.trim().strip_prefix('"')?.split('"').next().filter(|o| o.starts_with("--")).map(str::to_string)).collect()
}

fn refused_send_options(brief: &str, accepted: &[String]) -> Vec<String> {
    let mut out = vec![];
    for line in brief.lines().filter(|l| l.contains("mail send ")) {
        for tok in line.split_whitespace().filter(|t| t.starts_with("--")) {
            let opt = tok.trim_end_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-')).split('=').next().unwrap_or_default();
            if opt.len() > 2 && !accepted.iter().any(|a| a == opt) {
                out.push(opt.to_string());
            }
        }
    }
    out
}

fn files_under(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            files_under(&p, exts, out);
        } else if p.extension().and_then(|x| x.to_str()).is_some_and(|x| exts.contains(&x)) {
            out.push(p);
        }
    }
}

fn names_the_retired_ask_script(text: &str) -> bool {
    text.lines().any(|l| l.contains("cockpit/ask.sh") || l.match_indices("SPIRA_ASK").any(|(i, m)| !l[i + m.len()..].starts_with('_')))
}

// covers: UC-operator-channel-13
#[test]
fn the_option_check_flags_a_planted_unknown_option_and_passes_the_real_ones() {
    let accepted = accepted_send_options();
    for must in ["--from", "--subject", "--kind", "--default", "--class", "--bead", "--dry-run"] {
        assert!(accepted.iter().any(|a| a == must), "{must} not read from run_send: {accepted:?}");
    }
    assert_eq!(refused_send_options("run `mail send operator --subject x --frobnicate`", &accepted), ["--frobnicate"]);
    assert!(refused_send_options("run `mail send operator --subject x --kind question`", &accepted).is_empty());
}

// covers: UC-operator-channel-13
#[test]
fn no_brief_tells_an_agent_to_pass_mail_send_an_option_it_refuses() {
    let accepted = accepted_send_options();
    let mut briefs = vec![];
    files_under(&repo().join("spira/runes"), &["md", "rune"], &mut briefs);
    assert!(!briefs.is_empty());
    for b in briefs {
        let bad = refused_send_options(&fs::read_to_string(&b).unwrap_or_default(), &accepted);
        assert!(bad.is_empty(), "{} passes mail send {bad:?}", b.display());
    }
}

// covers: UC-operator-channel-13
#[test]
fn no_caller_of_the_retired_ask_script_remains() {
    assert!(names_the_retired_ask_script("bash cockpit/ask.sh add \"q\""), "the matcher must fire on a planted caller");
    assert!(!names_the_retired_ask_script("SPIRA_ASK_TIMEOUT=60"));
    assert!(!repo().join("cockpit/ask.sh").exists());
    let mut files = vec![];
    for d in ["spira", "cockpit"] {
        files_under(&repo().join(d), &["sh", "md", "conf"], &mut files);
    }
    assert!(!files.is_empty());
    let callers: Vec<String> = files.iter().filter(|f| names_the_retired_ask_script(&fs::read_to_string(f).unwrap_or_default())).map(|f| f.display().to_string()).collect();
    assert!(callers.is_empty(), "{callers:?}");
}
