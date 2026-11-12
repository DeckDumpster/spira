//! The PreToolUse hook: before a decision command runs, show the session the statutes that govern
//! it. The first call of each verb class in a window is refused with their full text; the retry,
//! made with the law read, passes and still carries the text as context.

use crate::doors::{Contact, Registry};
use crate::{rank, render, Statute, Vocab};
use std::path::Path;

pub const WINDOW_SECS: u64 = 600;
pub const OVERRIDE: &str = "STATUTE_CHECK_CONSIDERED";
const SHOWN: usize = 4;

const ROUND_VERBS: &[&str] = &["open", "certify", "land", "eject", "abandon"];
const WRAPPERS: &[&str] = &["env", "sudo", "nohup", "command", "exec", "time"];
const WRITERS: &[&str] = &["rm", "mv", "cp", "touch", "tee", "truncate", "ln", "install", "dd", "unlink", "shred"];

struct Segment {
    env: Vec<(String, String)>,
    argv: Vec<String>,
}

fn split_segments(cmd: &str) -> Vec<Segment> {
    let mut segs: Vec<Vec<String>> = vec![Vec::new()];
    let mut cur = String::new();
    let mut live = false;
    let chars: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    let flush = |segs: &mut Vec<Vec<String>>, cur: &mut String, live: &mut bool| {
        if *live {
            segs.last_mut().unwrap().push(std::mem::take(cur));
        }
        *live = false;
    };
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\'' => {
                live = true;
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    cur.push(chars[i]);
                    i += 1;
                }
            }
            '"' => {
                live = true;
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                    }
                    cur.push(chars[i]);
                    i += 1;
                }
            }
            '\\' if i + 1 < chars.len() => {
                live = true;
                i += 1;
                cur.push(chars[i]);
            }
            c if c.is_whitespace() && c != '\n' => flush(&mut segs, &mut cur, &mut live),
            '\n' | ';' | '|' | '&' => {
                flush(&mut segs, &mut cur, &mut live);
                while i + 1 < chars.len() && matches!(chars[i + 1], '|' | '&') && c != '\n' && c != ';' {
                    i += 1;
                }
                segs.push(Vec::new());
            }
            '>' => {
                flush(&mut segs, &mut cur, &mut live);
                segs.last_mut().unwrap().push(">".into());
                while i + 1 < chars.len() && chars[i + 1] == '>' {
                    i += 1;
                }
            }
            c => {
                live = true;
                cur.push(c);
            }
        }
        i += 1;
    }
    flush(&mut segs, &mut cur, &mut live);
    segs.into_iter()
        .filter(|s| !s.is_empty())
        .map(|mut toks| {
            let mut env = Vec::new();
            while let Some(first) = toks.first() {
                let is_assign = first.split_once('=').is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
                if is_assign {
                    let (k, v) = toks.remove(0).split_once('=').map(|(k, v)| (k.to_string(), v.to_string())).unwrap();
                    env.push((k, v));
                } else if WRAPPERS.contains(&base(first)) {
                    toks.remove(0);
                } else {
                    break;
                }
            }
            Segment { env, argv: toks }
        })
        .collect()
}

fn base(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

fn is_pause_path(t: &str) -> bool {
    t.contains("rounds-pace/") && base(t) == "pause"
}

#[derive(Default)]
pub struct Found {
    pub classes: Vec<String>,
    pub overrides: Vec<String>,
    pub used_doors: Vec<String>,
    pub touch_tools: Vec<(String, String)>,
}

fn classes_of(seg: &Segment, doors: Option<&Registry>, found: &mut Found) {
    let a = &seg.argv;
    let Some(first) = a.first() else { return };
    let arg = |n: usize| a.get(n).map(String::as_str).unwrap_or("");
    let tool = base(first);
    if let Some((_, v)) = seg.env.iter().find(|(k, _)| k == OVERRIDE) {
        if !v.trim().is_empty() {
            found.overrides.push(v.trim().to_string());
        }
    }
    if matches!(tool, "bash" | "sh") {
        if let Some(i) = a.iter().position(|t| t == "-c") {
            if let Some(inner) = a.get(i + 1) {
                for s in split_segments(inner) {
                    classes_of(&s, doors, found);
                }
            }
        }
    }
    for contact in doors.map(|d| d.contacts(a)).unwrap_or_default() {
        match contact {
            Contact::Touches(r) => {
                let c = format!("door:{}", r.name);
                if !found.classes.contains(&c) {
                    found.touch_tools.push((c.clone(), tool.to_string()));
                    found.classes.push(c);
                }
            }
            Contact::UsesDoor(r) => found.used_doors.push(r.name.clone()),
        }
    }
    let out = &mut found.classes;
    let mut push = |c: String| {
        if !out.contains(&c) {
            out.push(c);
        }
    };
    match (tool, arg(1)) {
        ("queue", v @ ("rework" | "resubmit" | "absolve")) => push(format!("queue {v}")),
        ("queue", "round") if ROUND_VERBS.contains(&arg(2)) => push(format!("queue round-{}", arg(2))),
        ("spira-lc", v @ ("close" | "hold" | "unhold" | "reply")) => push(format!("spira-lc {v}")),
        ("bead.sh" | "bead", v @ ("file" | "amend")) => push(format!("bead {v}")),
        ("dwarf", "direct") => push("dwarf direct".into()),
        ("mail", "send") => push("mail send".into()),
        ("spira-config", "set") => push("spira-config set".into()),
        ("batcher", "cut") => push("batcher cut".into()),
        ("systemctl", _) if a.iter().any(|t| t == "--user") => {
            if let Some(v) = a.iter().skip(1).find(|t| matches!(t.as_str(), "restart" | "stop" | "start")) {
                push(format!("systemctl {v}"));
            }
        }
        _ => {}
    }
    if matches!(tool, "reply" | "resolve") {
        push(tool.to_string());
    }
    if a.iter().any(|t| is_pause_path(t)) {
        let redirected = a.windows(2).any(|w| w[0] == ">" && is_pause_path(&w[1]));
        let sed_in_place = tool == "sed" && a.iter().any(|t| t.starts_with("-i") || t == "--in-place");
        if redirected || sed_in_place || WRITERS.contains(&tool) {
            push("pause file".into());
        }
    }
}

/// The verb classes `command` contains, and every override reason written as a prefix on it.
pub fn classify(command: &str) -> (Vec<String>, Vec<String>) {
    let f = find(command, None);
    (f.classes, f.overrides)
}

/// [`classify`] with the doors pass: `door:<name>` for each resource touched without its tool,
/// and the doors whose tool the command used.
pub fn find(command: &str, doors: Option<&Registry>) -> Found {
    let mut found = Found::default();
    for s in split_segments(command) {
        classes_of(&s, doors, &mut found);
    }
    found
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Pass,
    Context(String),
    Block(String),
}

pub struct Inputs<'a> {
    pub command: &'a str,
    pub description: &'a str,
    pub session: &'a str,
    pub persona: &'a str,
    pub env_override: Option<&'a str>,
    pub now: u64,
    pub state_dir: &'a Path,
    pub log: &'a Path,
    pub doors_log: &'a Path,
}

fn state_file(i: &Inputs, class: &str) -> std::path::PathBuf {
    let key = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect::<String>();
    i.state_dir.join(key(i.session)).join(key(class))
}

fn log_override(i: &Inputs, classes: &[String], why: &str) {
    use std::io::Write;
    if let Some(d) = i.log.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(i.log) {
        let cmd = i.command.replace('\n', " ");
        let _ = writeln!(f, "{}\t{}\t{}\t{}\t{}", i.now, i.session, classes.join(","), why.replace('\n', " "), cmd);
    }
}

fn log_door(i: &Inputs, resource: &str, tool: &str, event: &str, reason: &str) {
    use std::io::Write;
    if let Some(d) = i.doors_log.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(i.doors_log) {
        let flat = |s: &str| s.replace(['\n', '\t'], " ");
        let _ = writeln!(f, "{}\t{}\t{}\t{}\t{}\t{}", i.now, flat(i.persona), resource, flat(tool), event, flat(reason));
    }
}

fn seen_at(i: &Inputs, class: &str) -> Option<u64> {
    std::fs::read_to_string(state_file(i, class)).ok().and_then(|s| s.trim().parse::<u64>().ok()).filter(|t| i.now.saturating_sub(*t) < WINDOW_SECS)
}

pub fn decide(i: &Inputs, shelf: &Result<Vec<Statute>, String>, vocab: &Vocab, doors: Option<&Registry>) -> Outcome {
    let found = find(i.command, doors);
    for name in &found.used_doors {
        let class = format!("door:{name}");
        if seen_at(i, &class).is_some() {
            log_door(i, name, "", "used-door", "");
            let _ = std::fs::remove_file(state_file(i, &class));
        }
    }
    let (classes, overrides) = (found.classes, found.overrides);
    if classes.is_empty() {
        return Outcome::Pass;
    }
    let door_name = |c: &str| c.strip_prefix("door:").map(str::to_string);
    let tool_of = |c: &str| found.touch_tools.iter().find(|(k, _)| k == c).map(|(_, t)| t.as_str()).unwrap_or("");
    let why = overrides.first().map(String::as_str).or(i.env_override.map(str::trim).filter(|w| !w.is_empty()));
    if let Some(why) = why {
        log_override(i, &classes, why);
        for c in &classes {
            if let Some(n) = door_name(c) {
                log_door(i, &n, tool_of(c), "override", why);
            }
        }
        return Outcome::Pass;
    }
    let only_doors = classes.iter().all(|c| c.starts_with("door:"));
    let hits = match shelf {
        Ok(s) => rank(s, vocab, &format!("{} {}", i.command, i.description), SHOWN),
        Err(_) if only_doors => Vec::new(),
        Err(e) => return Outcome::Block(format!("statute-check: {e}. The law cannot be shown, so the decision is not made unread. Override: {OVERRIDE}=\"<slugs read, and why this complies>\".")),
    };
    if hits.is_empty() && !classes.iter().any(|c| c.starts_with("door:")) {
        return Outcome::Pass;
    }
    let text = render(&hits);
    let unseen: Vec<&String> = classes.iter().filter(|c| seen_at(i, c).is_none()).collect();
    if unseen.is_empty() {
        for c in &classes {
            if let Some(n) = door_name(c) {
                log_door(i, &n, tool_of(c), "retried", "");
            }
        }
        if hits.is_empty() {
            return Outcome::Pass;
        }
        return Outcome::Context(format!("Statutes governing this decision ({}), shown again:\n\n{text}", classes.join(", ")));
    }
    for c in &unseen {
        let f = state_file(i, c);
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(f, i.now.to_string());
        if let Some(n) = door_name(c) {
            log_door(i, &n, tool_of(c), "blocked", "");
        }
    }
    let pointers: Vec<String> = unseen.iter().filter_map(|c| door_name(c)).filter_map(|n| doors?.resource.iter().find(|r| r.name == n)).map(|r| r.pointer()).collect();
    let verbs: Vec<&&String> = unseen.iter().filter(|c| !c.starts_with("door:")).collect();
    let mut msg = String::from("statute-check: ");
    for p in &pointers {
        msg.push_str(p);
        msg.push('\n');
    }
    if verbs.is_empty() {
        msg.push_str(&format!("Use the tool, or retry the same command within {} minutes and it will go through.", WINDOW_SECS / 60));
    } else {
        msg.push_str(&format!(
            "{} is a decision. The statutes that govern it are below, in full. Read them, then retry the same command within {} minutes and it will go through; if one forbids it, do what it says instead.",
            verbs.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", "),
            WINDOW_SECS / 60
        ));
    }
    if !hits.is_empty() {
        msg.push_str(&format!("\n\n{text}"));
    }
    msg.push_str(&format!("\nTo proceed with a reason on record instead: {OVERRIDE}=\"<slugs read, and why this complies>\" as a prefix on the command."));
    Outcome::Block(msg)
}

/// The hook's stdout for `outcome`: Claude Code's block decision, or context attached to a pass.
pub fn response(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Pass => String::new(),
        Outcome::Block(reason) => serde_json::json!({"decision": "block", "reason": reason}).to_string(),
        Outcome::Context(ctx) => serde_json::json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "additionalContext": ctx}}).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(cmd: &str) -> Vec<String> {
        classify(cmd).0
    }

    #[test]
    fn decision_verbs_are_classified_through_wrappers_and_chains() {
        assert_eq!(c("queue rework sp-abc"), ["queue rework"]);
        assert_eq!(c("cd /x && FOO=1 /opt/bin/queue round eject r1"), ["queue round-eject"]);
        assert_eq!(c("bash -c 'spira-lc hold sp-1'"), ["spira-lc hold"]);
        assert_eq!(c("systemctl --user restart batcher.service"), ["systemctl restart"]);
        assert_eq!(c("bin/reply sp-1 \"ok\""), ["reply"]);
        assert_eq!(c("batcher cut x | tee log"), ["batcher cut"]);
    }

    #[test]
    fn a_pause_file_is_caught_when_written_moved_or_removed_and_not_when_read() {
        assert_eq!(c("touch run/rounds-pace/r/pause"), ["pause file"]);
        assert_eq!(c("echo x > run/rounds-pace/r/pause"), ["pause file"]);
        assert_eq!(c("rm -f $RUN/rounds-pace/r/pause"), ["pause file"]);
        assert!(c("cat run/rounds-pace/r/pause").is_empty());
        assert!(c("queue status").is_empty());
        assert!(c("systemctl --user status x").is_empty());
        assert!(c("echo queue rework").is_empty());
    }

    #[test]
    fn a_prefix_override_is_read_from_the_command() {
        let (cl, ov) = classify("STATUTE_CHECK_CONSIDERED=\"law-x read; complies\" queue rework sp-1");
        assert_eq!(cl, ["queue rework"]);
        assert_eq!(ov, ["law-x read; complies"]);
    }

    fn d(cmd: &str) -> Vec<String> {
        let reg = crate::doors::parse(include_str!("../../spira/doors.toml")).unwrap();
        find(cmd, Some(&reg)).classes.into_iter().filter(|c| c.starts_with("door:")).collect()
    }

    #[test]
    fn a_command_that_moves_the_landing_ref_is_stopped_whatever_it_is() {
        for cmd in [
            "git update-ref refs/heads/local/main abc123",
            "git -C /r/repo push . HEAD:local/main",
            "git branch -f local/main abc123",
            "queue land-local spira --head abc --members sp-1:abc",
            "cd /r && queue.sh land-local --head abc --members sp-1:abc",
            "bash -c 'git update-ref refs/heads/local/main abc'",
        ] {
            assert_eq!(d(cmd), ["door:landing-ref"], "{cmd}");
        }
        for cmd in [
            "git log local/main",
            "git diff local/main...HEAD",
            "git rebase local/main",
            "git -C /r branch --list local/main",
            "queue hand-land sp-1 spira --tip abc --by concierge --reason x --evidence y --evidence-tip abc --round wait",
        ] {
            assert!(d(cmd).is_empty(), "{cmd}: {:?}", d(cmd));
        }
    }

    #[test]
    fn round_state_is_stopped_when_written_and_not_when_read() {
        assert_eq!(d("mv /r/run/rounds-pace/spira/pause /tmp/x"), ["door:round-state"]);
        assert_eq!(d("rm -f run/rounds-pace/spira/pause"), ["door:round-state"]);
        assert!(d("cat /r/run/rounds-pace/spira/pause").is_empty());
        assert!(d("queue round stop spira --by me --reason r --until u").is_empty());
    }

    #[test]
    fn tracing_or_reading_a_store_process_is_stopped_even_though_it_only_reads() {
        assert_eq!(d("strace -f spira-lc list"), ["door:store-trace"]);
        assert_eq!(d("strace -p 4242"), ["door:store-trace"]);
        assert_eq!(d("cat /proc/123/cmdline"), ["door:store-trace"]);
        assert!(d("strace -f ls").is_empty());
        assert!(d("store-trace.sh --pid 4242").is_empty());
    }

    #[test]
    fn a_raw_store_client_is_stopped_and_the_stores_tool_is_not() {
        assert_eq!(d("bd list"), ["door:store"]);
        assert_eq!(d("sqlite3 /r/state.db .tables"), ["door:store"]);
        assert!(d("sqlite3 :memory: 'select 1'").is_empty());
        assert!(d("spira-lc list --state open").is_empty());
        assert!(d("work show").is_empty());
    }

    #[test]
    fn builders_bead_branches_and_the_other_resources_each_have_their_pointer() {
        assert_eq!(d("systemctl --user stop dwarf-sp-1.service"), ["door:builder"]);
        assert!(d("systemctl --user status dwarf-sp-1.service").is_empty());
        assert!(d("slay.sh --bead sp-1 --keep-work").is_empty());
        assert_eq!(d("git branch -D spira/sp-1"), ["door:bead-branch"]);
        assert_eq!(d("git push origin :spira/sp-1"), ["door:bead-branch"]);
        assert!(d("git log spira/sp-1").is_empty());
        assert!(d("rm /r/run/worktree/sp-1/src/main.rs").is_empty());
        assert_eq!(d("rm -rf /r/run/worktree/sp-1"), ["door:bead-branch"]);
        assert_eq!(d("ln -sfn /r/rel2 /r/spira-releases/current"), ["door:active-release"]);
        assert_eq!(d("rm -rf /r/dwarf-build/abc"), ["door:build-dir"]);
    }

    fn inputs<'a>(dir: &'a Path, command: &'a str, now: u64, env_override: Option<&'a str>) -> Inputs<'a> {
        Inputs {
            command,
            description: "",
            session: "s",
            persona: "concierge",
            env_override,
            now,
            state_dir: dir,
            log: Box::leak(dir.join("o.log").into_boxed_path()),
            doors_log: Box::leak(dir.join("doors.log").into_boxed_path()),
        }
    }

    fn no_statutes() -> Result<Vec<Statute>, String> {
        Ok(Vec::new())
    }

    #[test]
    fn the_block_names_the_tool_with_its_usage_once_and_the_retry_passes() {
        let reg = crate::doors::parse(include_str!("../../spira/doors.toml")).unwrap();
        let dir = testkit::TempDir::new("doors-block");
        let cmd = "git update-ref refs/heads/local/main deadbeef";
        let first = decide(&inputs(&dir, cmd, 1000, None), &no_statutes(), &Vocab::default(), Some(&reg));
        let Outcome::Block(text) = first else { panic!("{first:?}") };
        assert!(text.contains("queue hand-land") && text.contains("--tip <40-hex> --by <actor>"), "{text}");
        assert!(text.contains("This touches landing-ref."), "{text}");
        assert_eq!(decide(&inputs(&dir, cmd, 1010, None), &no_statutes(), &Vocab::default(), Some(&reg)), Outcome::Pass);
        let log = std::fs::read_to_string(dir.join("doors.log")).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines, ["1000\tconcierge\tlanding-ref\tgit\tblocked\t", "1010\tconcierge\tlanding-ref\tgit\tretried\t"]);
        assert!(!log.contains("deadbeef") && !log.contains("update-ref"), "no command arguments in the log: {log}");
    }

    #[test]
    fn using_the_door_after_a_block_is_recorded_and_an_override_is_logged_with_its_reason() {
        let reg = crate::doors::parse(include_str!("../../spira/doors.toml")).unwrap();
        let dir = testkit::TempDir::new("doors-use");
        let touch = "git branch -f local/main abc";
        let run = |cmd, now, ov| decide(&inputs(&dir, cmd, now, ov), &no_statutes(), &Vocab::default(), Some(&reg));
        assert!(matches!(run(touch, 5, None), Outcome::Block(_)));
        assert_eq!(run("queue hand-land sp-1 spira --tip abc", 9, None), Outcome::Pass);
        assert_eq!(run(touch, 20, Some("operator said so")), Outcome::Pass);
        let log = std::fs::read_to_string(dir.join("doors.log")).unwrap();
        let events: Vec<&str> = log.lines().map(|l| l.split('\t').nth(4).unwrap()).collect();
        assert_eq!(events, ["blocked", "used-door", "override"], "{log}");
        assert!(log.lines().last().unwrap().ends_with("operator said so"));
    }
}
