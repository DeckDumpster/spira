//! The PreToolUse hook: before a decision command runs, show the session the statutes that govern
//! it. The first call of each verb class in a window is refused with their full text; the retry,
//! made with the law read, passes and still carries the text as context.

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

fn classes_of(seg: &Segment, out: &mut Vec<String>, overrides: &mut Vec<String>) {
    let a = &seg.argv;
    let Some(first) = a.first() else { return };
    let arg = |n: usize| a.get(n).map(String::as_str).unwrap_or("");
    let tool = base(first);
    if let Some((_, v)) = seg.env.iter().find(|(k, _)| k == OVERRIDE) {
        if !v.trim().is_empty() {
            overrides.push(v.trim().to_string());
        }
    }
    if matches!(tool, "bash" | "sh") {
        if let Some(i) = a.iter().position(|t| t == "-c") {
            if let Some(inner) = a.get(i + 1) {
                for s in split_segments(inner) {
                    classes_of(&s, out, overrides);
                }
            }
        }
    }
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
    let (mut classes, mut overrides) = (Vec::new(), Vec::new());
    for s in split_segments(command) {
        classes_of(&s, &mut classes, &mut overrides);
    }
    (classes, overrides)
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
    pub env_override: Option<&'a str>,
    pub now: u64,
    pub state_dir: &'a Path,
    pub log: &'a Path,
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

pub fn decide(i: &Inputs, shelf: &Result<Vec<Statute>, String>, vocab: &Vocab) -> Outcome {
    let (classes, overrides) = classify(i.command);
    if classes.is_empty() {
        return Outcome::Pass;
    }
    let why = overrides.first().map(String::as_str).or(i.env_override.map(str::trim).filter(|w| !w.is_empty()));
    if let Some(why) = why {
        log_override(i, &classes, why);
        return Outcome::Pass;
    }
    let shelf = match shelf {
        Ok(s) => s,
        Err(e) => return Outcome::Block(format!("statute-check: {e}. The law cannot be shown, so the decision is not made unread. Override: {OVERRIDE}=\"<slugs read, and why this complies>\".")),
    };
    let query = format!("{} {}", i.command, i.description);
    let hits = rank(shelf, vocab, &query, SHOWN);
    if hits.is_empty() {
        return Outcome::Pass;
    }
    let text = render(&hits);
    let unseen: Vec<&String> = classes
        .iter()
        .filter(|c| std::fs::read_to_string(state_file(i, c)).ok().and_then(|s| s.trim().parse::<u64>().ok()).is_none_or(|t| i.now.saturating_sub(t) >= WINDOW_SECS))
        .collect();
    if unseen.is_empty() {
        return Outcome::Context(format!("Statutes governing this decision ({}), shown again:\n\n{text}", classes.join(", ")));
    }
    for c in &unseen {
        let f = state_file(i, c);
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(f, i.now.to_string());
    }
    Outcome::Block(format!(
        "statute-check: {} is a decision. The statutes that govern it are below, in full. Read them, then retry the same command within {} minutes and it will go through; if one forbids it, do what it says instead.\n\n{text}\nTo proceed with a reason on record instead: {OVERRIDE}=\"<slugs read, and why this complies>\" as a prefix on the command.",
        unseen.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", "),
        WINDOW_SECS / 60
    ))
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
}
