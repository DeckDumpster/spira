//! `work review` through the real binary: the work item comes from the socket, the diff from a
//! real git repo, and the model runner is a stub agent at the `SPIRA_AGENT` seam the dwarf
//! launch path uses.

use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

fn socket_answering(sock: &Path, item: &'static str) {
    let l = UnixListener::bind(sock).unwrap();
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};
        for s in l.incoming().flatten() {
            let mut line = String::new();
            let _ = BufReader::new(s.try_clone().unwrap()).read_line(&mut line);
            let _ = write!(&s, "{{\"exit_code\":0,\"len\":{}}}\n{item}", item.len());
        }
    });
}

fn fixture(tag: &str, agent_body: &str) -> (testkit::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let d = testkit::TempDir::new(tag);
    let repo = d.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("src/parser.rs"), "pub fn parse() {}\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    git(&repo, &["branch", "local/main"]);
    std::fs::write(repo.join("src/parser2.rs"), "pub fn parse_again() {}\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "sp-abc12: a second parser"]);
    let agent = d.join("agent");
    testkit::write_exe(&agent, agent_body);
    let sock = d.join("sock");
    socket_answering(&sock, "Add the widget parser.\n");
    std::fs::create_dir_all(d.join("conf.d")).unwrap();
    let toml = spira_config::process::fixture_toml(&d, &[("SPIRA_LC_SOCKET", sock.to_str().unwrap()), ("SPIRA_AGENT", agent.to_str().unwrap()), ("SPIRA_RUN", d.join("run").to_str().unwrap())]);
    (d, repo, toml)
}

fn review(d: &Path, repo: &Path, toml: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_work"))
        .arg("review")
        .args(args)
        .current_dir(repo)
        .env_clear()
        .env("HOME", d)
        .env("PATH", "/usr/bin:/bin")
        .env("SPIRA_WORK_BEAD_ID", "sp-abc12")
        .env("SPIRA_HOME", d)
        .env("SPIRA_TOML", toml)
        .output()
        .unwrap()
}

const FINDER: &str = "#!/bin/sh\ninput=$(cat)\necho \"$input\" | grep -q 'Add the widget parser' || { echo 'no work item' >&2; exit 1; }\necho \"$*\" | grep -q 'claude-sonnet-5-5' || { echo 'wrong model' >&2; exit 1; }\nnew=$(echo \"$input\" | sed -n 's|^+++ b/\\(.*\\)|\\1|p')\nif [ -f src/parser.rs ]; then printf '{\"type\":\"result\",\"result\":\"- %s:1 — duplicates the existing module src/parser.rs; extend it instead\"}\\n' \"$new\"; fi\n";

const LAW_CALLER: &str = "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{\"command\":\"work law 2.1\"}}]}}'\nprintf '%s\\n' '{\"type\":\"result\",\"result\":\"- a.rs:1 — fix it\"}'\n";

#[test]
fn a_second_implementation_of_an_existing_module_names_the_existing_one() {
    let (d, repo, toml) = fixture("work-review-dup", FINDER);
    let o = review(&d, &repo, &toml, &[]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("src/parser2.rs:1") && out.contains("src/parser.rs"), "{out}");
}

#[test]
fn a_reviewer_that_outlives_the_timeout_is_stopped_and_it_says_so() {
    let (d, repo, toml) = fixture("work-review-slow", "#!/bin/sh\ncat >/dev/null\nsleep 30\n");
    let o = review(&d, &repo, &toml, &["--timeout", "1"]);
    assert_eq!(o.status.code(), Some(124));
    assert!(String::from_utf8_lossy(&o.stderr).contains("timed out after 1s"));
}

#[test]
fn help_is_answered_without_a_binding_or_a_socket() {
    let (d, repo, toml) = fixture("work-review-help", FINDER);
    let o = Command::new(env!("CARGO_BIN_EXE_work")).args(["review", "--help"]).current_dir(&repo).env_clear().env("SPIRA_TOML", &toml).env("HOME", &d).output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stdout).contains("work review"));
}

#[test]
fn a_review_keeps_its_transcript_and_a_summary_while_the_builder_sees_only_the_verdict() {
    let (d, repo, toml) = fixture("work-review-keep", LAW_CALLER);
    let o = review(&d, &repo, &toml, &[]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "- a.rs:1 — fix it");
    let t = std::fs::read_to_string(d.join("run/sp-abc12.review-1.jsonl")).unwrap();
    assert!(t.contains("work law 2.1"), "{t}");
    let sum: serde_json::Value = serde_json::from_str(std::fs::read_to_string(d.join("run/review-summary.jsonl")).unwrap().lines().next().unwrap()).unwrap();
    assert_eq!((sum["work"].as_str(), sum["law_calls"].as_u64(), sum["law_args"][0].as_str(), sum["tool_calls"]["Bash"].as_u64(), sum["findings"].as_u64()), (Some("sp-abc12"), Some(1), Some("2.1"), Some(1), Some(1)));
    review(&d, &repo, &toml, &[]);
    assert!(d.join("run/sp-abc12.review-2.jsonl").exists());
}

#[test]
fn a_timed_out_or_failed_review_still_leaves_its_transcript() {
    let (d, repo, toml) = fixture("work-review-partial", "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Read\",\"input\":{}}]}}'\nsleep 30\n");
    assert_eq!(review(&d, &repo, &toml, &["--timeout", "1"]).status.code(), Some(124));
    assert!(std::fs::read_to_string(d.join("run/sp-abc12.review-1.jsonl")).unwrap().contains("Read"));
    let (d, repo, toml) = fixture("work-review-failed", "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"type\":\"system\"}'\nexit 1\n");
    assert_eq!(review(&d, &repo, &toml, &[]).status.code(), Some(3));
    let sum = std::fs::read_to_string(d.join("run/review-summary.jsonl")).unwrap();
    assert!(sum.contains("\"law_calls\":0"), "{sum}");
}

#[test]
fn a_bare_reviewer_name_resolves_in_the_release_bin_though_the_path_lacks_it() {
    let (d, repo, _) = fixture("work-review-bare", FINDER);
    let rel = d.join("release");
    std::fs::create_dir_all(rel.join("bin")).unwrap();
    std::fs::rename(d.join("agent"), rel.join("bin/nqmjx7-claude")).unwrap();
    let sock = d.join("sock");
    let toml = spira_config::process::fixture_toml(&d, &[("SPIRA_LC_SOCKET", sock.to_str().unwrap()), ("SPIRA_AGENT", "nqmjx7-claude")]);
    let o = Command::new(env!("CARGO_BIN_EXE_work"))
        .arg("review")
        .current_dir(&repo)
        .env_clear()
        .env("HOME", &d)
        .env("PATH", "/usr/bin:/bin")
        .env("SPIRA_WORK_BEAD_ID", "sp-abc12")
        .env("SPIRA_HOME", &d)
        .env("SPIRA_RELEASE", &rel)
        .env("SPIRA_TOML", toml)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stdout).contains("src/parser2.rs:1"));
}

#[test]
fn the_system_prompt_the_reviewer_is_launched_with_starts_from_work_law() {
    let agent = "#!/bin/sh\ncat >/dev/null\nwhile [ $# -gt 0 ]; do [ \"$1\" = --system-prompt ] && { printf '%s\\n' \"$2\" | grep -q 'run `work law` and note the sections' && printf '%s\\n' '{\"type\":\"result\",\"result\":\"- x:1 — law first step present\"}'; exit 0; }; shift; done\n";
    let (d, repo, toml) = fixture("work-review-law", agent);
    let o = review(&d, &repo, &toml, &[]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stdout).contains("law first step present"));
}
