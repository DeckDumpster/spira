//! The hook path end to end: a recorded PostToolUse JSON on stdin of the real `habits` binary,
//! in a throwaway git repo. rustfmt is the real tool; shellcheck and rust-code-analysis-cli are
//! stubs that print their real output shapes (gcc lines; the `spaces` JSON tree).

use std::path::Path;
use std::process::{Command, Stdio};

use testkit::TempDir;

const HABITS: &str = r#"
[[habit]]
name = "rustfmt"
glob = "**/*.rs"
run = "rustfmt --check --edition 2021 {file}"
budget_ms = 5000

[[habit]]
name = "complexity"
glob = "**/*.rs"
run = "rust-code-analysis-cli -m -O json -p {file}"
threshold = "cognitive > 20 per function"
budget_ms = 5000

[[habit]]
name = "shellcheck"
glob = "**/*.sh"
run = "shellcheck -f gcc {file}"
budget_ms = 5000
"#;

// Cognitive = the count of `if ` lines inside each `fn`; one space per fn, the real `-m -O json` shape.
const RCA_STUB: &str = r#"#!/bin/sh
if [ "$1" = "-f" ]; then exit 0; fi
for last; do :; done
awk 'function flush() { if (name != "") { printf "%s{\"name\":\"%s\",\"start_line\":%d,\"end_line\":%d,\"kind\":\"function\",\"metrics\":{\"cognitive\":{\"sum\":%d}},\"spaces\":[]}", sep, name, start, NR, n; sep = "," } }
  /^fn / { flush(); name = $2; sub(/\(.*/, "", name); start = NR; n = 0 }
  /if / { n++ }
  END { flush(); print "]}" }
  BEGIN { printf "{\"name\":\"f\",\"start_line\":1,\"end_line\":1,\"kind\":\"unit\",\"metrics\":{\"cognitive\":{\"sum\":0}},\"spaces\":[" }' "$last"
"#;

const SHELLCHECK_STUB: &str = r#"#!/bin/sh
for last; do :; done
grep -n 'rm -rf \$' "$last" | while IFS=: read -r n _; do
  echo "$last:$n:8: note: Double quote to prevent globbing and word splitting. [SC2086]"
done
"#;

struct Repo {
    dir: TempDir,
    bin: TempDir,
    tmp: TempDir,
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).stdout(Stdio::null()).status().unwrap().success();
    assert!(ok, "git {args:?}");
}

impl Repo {
    fn new(habits: Option<&str>) -> Repo {
        let r = Repo { dir: TempDir::new("habits-repo"), bin: TempDir::new("habits-bin"), tmp: TempDir::new("habits-tmp") };
        git(&r.dir, &["init", "-q"]);
        if let Some(h) = habits {
            std::fs::create_dir_all(r.dir.join(".spira")).unwrap();
            std::fs::write(r.dir.join(".spira/habits.toml"), h).unwrap();
        }
        testkit::write_exe(r.bin.join("rust-code-analysis-cli"), RCA_STUB);
        testkit::write_exe(r.bin.join("shellcheck"), SHELLCHECK_STUB);
        r
    }

    fn write(&self, rel: &str, body: &str) {
        std::fs::write(self.dir.join(rel), body).unwrap();
    }

    fn commit(&self) {
        git(&self.dir, &["add", "-A"]);
        git(&self.dir, &["commit", "-q", "-m", "base"]);
    }

    fn edit_hook(&self, rel: &str, event: &str) -> (i32, String) {
        let input = format!(
            r#"{{"session_id":"s1","cwd":"{}","hook_event_name":"{event}","tool_name":"Edit","tool_input":{{"file_path":"{}/{rel}","old_string":"a","new_string":"b"}},"tool_response":{{"success":true}}}}"#,
            self.dir.display(),
            self.dir.display()
        );
        let path = format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap_or_default());
        let mut child = Command::new(env!("CARGO_BIN_EXE_habits"))
            .env_clear()
            .env("PATH", path)
            .env("HOME", std::env::var("HOME").unwrap_or_default())
            .env("TMPDIR", self.tmp.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), input.as_bytes()).unwrap();
        let o = finish(child);
        (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned())
    }

    fn post(&self, rel: &str) -> (i32, String) {
        self.edit_hook(rel, "PostToolUse")
    }
}

fn finish(child: std::process::Child) -> std::process::Output {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    rx.recv_timeout(std::time::Duration::from_secs(60)).expect("habits never exited").unwrap()
}

fn context(out: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("not JSON ({e}): {out:?}"));
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    v["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_string()
}

const CLEAN_RS: &str = "fn a() {\n    let x = 1;\n}\n\nfn b() {\n    let   y=2;\n}\n";

#[test]
fn a_rustfmt_violation_on_a_touched_line_is_named() {
    let r = Repo::new(Some(HABITS));
    r.write("a.rs", CLEAN_RS);
    r.commit();
    r.write("a.rs", &CLEAN_RS.replace("let x = 1;", "let x=1;"));
    let (code, out) = r.post("a.rs");
    assert_eq!(code, 0);
    let ctx = context(&out);
    assert!(ctx.contains("rustfmt") && ctx.contains("line 2") && ctx.contains("let x=1;"), "{ctx}");
    assert!(!ctx.contains("y=2"), "the untouched pre-existing violation must not be reported: {ctx}");
}

#[test]
fn an_untouched_pre_existing_violation_says_nothing() {
    let r = Repo::new(Some(HABITS));
    r.write("a.rs", CLEAN_RS);
    r.commit();
    r.write("a.rs", &CLEAN_RS.replace("let x = 1;", "let x = 2;"));
    let (code, out) = r.post("a.rs");
    assert_eq!((code, out.trim()), (0, ""));
}

#[test]
fn a_shellcheck_finding_on_a_touched_line_is_named() {
    let r = Repo::new(Some(HABITS));
    r.write("p.sh", "#!/bin/sh\necho ok\nrm -rf \"$1\"\n");
    r.commit();
    r.write("p.sh", "#!/bin/sh\necho ok\nrm -rf \"$1\"\nrm -rf $2\n");
    let (code, out) = r.post("p.sh");
    assert_eq!(code, 0);
    let ctx = context(&out);
    assert!(ctx.contains("shellcheck") && ctx.contains("SC2086") && ctx.contains("line 4"), "{ctx}");
}

fn heavy(ifs: usize) -> String {
    format!("fn heavy() {{\n{}}}\n", "    if x {}\n".repeat(ifs))
}

#[test]
fn a_function_pushed_over_the_limit_is_named_once() {
    let r = Repo::new(Some(HABITS));
    r.write("c.rs", &heavy(18));
    r.commit();
    r.write("c.rs", &heavy(19));
    assert_eq!(r.post("c.rs").1.trim(), "", "18 to 19 stays under the limit");
    r.write("c.rs", &heavy(23));
    let ctx = context(&r.post("c.rs").1);
    assert!(ctx.contains("complexity") && ctx.contains("heavy") && ctx.contains("18") && ctx.contains("23"), "{ctx}");
    r.write("c.rs", &heavy(24));
    assert!(!context_or_empty(&r.post("c.rs").1).contains("complexity"), "already told this session");
}

fn context_or_empty(out: &str) -> String {
    if out.trim().is_empty() { String::new() } else { context(out) }
}

#[test]
fn a_function_already_over_the_limit_is_not_reported_for_an_edit_inside_it() {
    let r = Repo::new(Some(HABITS));
    r.write("c.rs", &heavy(30));
    r.commit();
    r.write("c.rs", &heavy(31));
    assert_eq!(r.post("c.rs").1.trim(), "");
}

#[test]
fn a_new_function_over_the_limit_is_named() {
    let r = Repo::new(Some(HABITS));
    r.write("c.rs", "fn small() {}\n");
    r.commit();
    r.write("c.rs", &format!("fn small() {{}}\n{}", heavy(25)));
    let ctx = context(&r.post("c.rs").1);
    assert!(ctx.contains("new fn heavy") && ctx.contains("25"), "{ctx}");
}

#[test]
fn a_file_the_parser_errors_on_gets_no_complexity_finding() {
    let r = Repo::new(Some(HABITS));
    testkit::write_exe(r.bin.join("rust-code-analysis-cli"), "#!/bin/sh\nif [ \"$1\" = \"-f\" ]; then echo 'In file x'; echo '{ERROR:65535} from (1, 1) to (1, 2)'; exit 0; fi\necho 'scored anyway' >&2; exit 1\n");
    r.write("c.rs", "fn small() {}\n");
    r.commit();
    r.write("c.rs", &format!("fn small() {{}}\n{}", heavy(25)));
    assert!(!context_or_empty(&r.post("c.rs").1).contains("complexity"));
}

#[test]
fn a_missing_tool_exits_zero_and_says_so_once() {
    let r = Repo::new(Some(&HABITS.replace("shellcheck -f gcc", "no-such-tool-xyz")));
    r.write("p.sh", "echo a\n");
    r.commit();
    r.write("p.sh", "echo b\n");
    let (code, out) = r.post("p.sh");
    assert_eq!(code, 0);
    assert!(context(&out).contains("shellcheck skipped (tool not found)"));
    let (code, out) = r.post("p.sh");
    assert_eq!((code, out.trim()), (0, ""));
}

#[test]
fn a_habit_over_its_budget_is_skipped_with_exit_zero() {
    let r = Repo::new(None);
    std::fs::create_dir_all(r.dir.join(".spira")).unwrap();
    let slow = HABITS.replace("run = \"shellcheck -f gcc {file}\"\nbudget_ms = 5000", "run = \"sleep 5\"\nbudget_ms = 100");
    std::fs::write(r.dir.join(".spira/habits.toml"), slow).unwrap();
    r.write("p.sh", "echo a\n");
    r.commit();
    r.write("p.sh", "echo b\n");
    let (code, out) = r.post("p.sh");
    assert_eq!(code, 0);
    assert!(context(&out).contains("shellcheck skipped (over its 100 ms budget)"));
}

#[test]
fn a_repo_without_habits_toml_gets_nothing() {
    let r = Repo::new(None);
    r.write("a.rs", CLEAN_RS);
    r.commit();
    r.write("a.rs", "fn   a(){}\n");
    assert_eq!(r.post("a.rs"), (0, String::new()));
}

#[test]
fn garbage_on_stdin_still_exits_zero() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_habits")).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), b"not json").unwrap();
    let o = child.wait_with_output().unwrap();
    assert_eq!((o.status.code(), o.stdout.len()), (Some(0), 0));
}

#[test]
fn a_malformed_habits_toml_says_so_once_and_exits_zero() {
    let r = Repo::new(Some("[[habit]]\nname = 1\n"));
    r.write("a.rs", "fn a() {}\n");
    r.commit();
    r.write("a.rs", "fn   a(){}\n");
    let (code, out) = r.post("a.rs");
    assert_eq!(code, 0);
    assert!(context(&out).contains("habits.toml is unreadable"));
    assert_eq!(r.post("a.rs").1.trim(), "");
}

#[test]
fn stop_runs_the_whole_crate_check_once_on_changed_lines_only() {
    let stop = format!("{HABITS}\n[[stop]]\nname = \"clippy\"\nrun = \"clip {{crate}}\"\nbudget_ms = 5000\n");
    let r = Repo::new(Some(&stop));
    std::fs::create_dir_all(r.dir.join("k/src")).unwrap();
    r.write("k/Cargo.toml", "[package]\nname = \"kay\"\nversion = \"0.1.0\"\n");
    r.write("k/src/lib.rs", "pub fn a() {}\npub fn b() {}\n");
    r.commit();
    r.write("k/src/lib.rs", "pub fn a() {}\npub fn b() {}\npub fn c() {}\n");
    testkit::write_exe(
        r.bin.join("clip"),
        "#!/bin/sh\necho \"k/src/lib.rs:1:1: warning: old and untouched\"\necho \"k/src/lib.rs:3:1: warning: fresh on the new line\"\n",
    );
    let (code, out) = r.edit_hook("k/src/lib.rs", "Stop");
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    let msg = v["systemMessage"].as_str().unwrap();
    assert!(msg.contains("fresh on the new line") && !msg.contains("untouched"), "{msg}");
    assert_eq!(r.edit_hook("k/src/lib.rs", "Stop").1.trim(), "", "once per session");
}

#[test]
fn metrics_count_findings_and_the_resolution_of_the_previous_run() {
    use spira_otel::metrics::MetricsSink;
    let r = Repo::new(Some(HABITS));
    r.write("a.rs", CLEAN_RS);
    r.commit();
    let sink_srv = testkit::otlp::Sink::start();
    let sink = MetricsSink::new(Some(&sink_srv.addr), "spira-habits", std::time::Duration::from_secs(2));
    let hook = |rel: &str| {
        let input = format!(r#"{{"session_id":"m1","cwd":"{0}","hook_event_name":"PostToolUse","tool_input":{{"file_path":"{0}/{rel}"}}}}"#, r.dir.display());
        let path = format!("{}:{}", r.bin.display(), std::env::var("PATH").unwrap_or_default());
        let _env = testkit::env(&[("PATH", Some(&path))]);
        habits::dispatch(&input, &r.tmp, &sink)
    };
    r.write("a.rs", &CLEAN_RS.replace("let x = 1;", "let x=1;"));
    assert!(hook("a.rs").is_some());
    sink_srv.expect(&["habit_findings_total", "habit_resolved_total", "habit_runtime_ms", "rustfmt", &r.dir.file_name().unwrap().to_string_lossy()], std::time::Duration::from_secs(5));
    r.write("a.rs", &CLEAN_RS.replace("let x = 1;", "let x = 3;"));
    assert!(hook("a.rs").is_none(), "fixed: nothing to say");
    let body = sink_srv.expect(&["habit_resolved_total"], std::time::Duration::from_secs(5));
    assert!(body.matches("habit_resolved_total").count() >= 2);
}
