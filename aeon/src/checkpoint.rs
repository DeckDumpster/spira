//! Checkpointing a session across a world stop: the session id recorded at launch, the
//! decision to resume it or fall back cold, the turn boundary a stop waits for, and the
//! message that tells a resumed model the world was down.
//!
//! The facts are `session` (`<uuid> <fayth> <persona hash> <worktree>`) and `checkpointed`
//! (`<epoch>`), appended to the lifecycle event log; a checkpoint is pending while the last
//! `checkpointed` fact is newer than the last `session` fact.

use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

pub const SESSION: &str = "session";
pub const CHECKPOINTED: &str = "checkpointed";

/// A stopped world that is never started again releases its checkpoints after this long.
pub const HOLD_SECS: i64 = 7 * 24 * 3600;
/// How long a stop waits for a turn boundary; below `slay`'s 60s before KILL.
pub const BOUNDARY_WAIT_SECS: u64 = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    pub kind: String,
    pub cause: String,
}

/// `spira-lc facts` output (an array of `{event_type, new_value}`, oldest first).
pub fn parse_facts(json: &str) -> Vec<Fact> {
    let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(json.trim()) else { return Vec::new() };
    rows.iter()
        .filter_map(|r| {
            let s = |k: &str| r.get(k).and_then(Value::as_str).map(str::to_string);
            Some(Fact { kind: s("event_type")?, cause: s("new_value").unwrap_or_default() })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFact {
    pub id: String,
    pub fayth: String,
    pub persona: String,
    pub worktree: String,
}

impl SessionFact {
    pub fn cause(&self) -> String {
        format!("{} {} {} {}", self.id, self.fayth, self.persona, self.worktree)
    }

    pub fn parse(cause: &str) -> Option<Self> {
        let mut p = cause.splitn(4, ' ');
        let (id, fayth, persona) = (p.next()?, p.next()?, p.next()?);
        if !is_uuid(id) {
            return None;
        }
        Some(Self { id: id.into(), fayth: fayth.into(), persona: persona.into(), worktree: p.next().unwrap_or("").into() })
    }
}

pub fn is_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5 && [8, 4, 4, 4, 12].iter().zip(&parts).all(|(n, p)| p.len() == *n && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A random (v4) session id, as `claude --session-id` requires.
pub fn new_session_id() -> String {
    use std::io::Read;
    let mut b = [0u8; 16];
    let read = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b));
    if read.is_err() {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        b = (t ^ ((std::process::id() as u128) << 64)).to_le_bytes();
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// A short fingerprint of the persona template a session ran under.
pub fn persona_hash(template: &[u8]) -> String {
    Sha256::digest(template).iter().take(6).map(|x| format!("{x:02x}")).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Fresh,
    Resume { session: SessionFact, stopped_at: Option<i64> },
    Cold { reason: String },
}

/// What this claim does with a checkpoint left by an earlier stop.
pub fn plan(facts: &[Fact], fayth: &str, persona: &str, worktree_present: bool, transcript_present: impl Fn(&str) -> bool) -> Plan {
    let last = |kind: &str| facts.iter().rposition(|f| f.kind == kind);
    let Some(ck) = last(CHECKPOINTED) else { return Plan::Fresh };
    let ses = last(SESSION);
    if ses.is_some_and(|s| s > ck) {
        return Plan::Fresh;
    }
    let stopped_at = facts[ck].cause.trim().parse().ok();
    let cold = |r: &str| Plan::Cold { reason: r.to_string() };
    let Some(session) = ses.and_then(|s| SessionFact::parse(&facts[s].cause)) else {
        return cold("the checkpoint names no recorded session");
    };
    if session.fayth != fayth {
        return cold(&format!("the session ran as {} and this is {fayth}", session.fayth));
    }
    if session.persona != persona {
        return cold("the persona changed since the session was recorded (a new release)");
    }
    if !worktree_present {
        return cold("the kept worktree is gone");
    }
    if !transcript_present(&session.id) {
        return cold("the session transcript is gone (expired)");
    }
    Plan::Resume { session, stopped_at }
}

/// The claude config dir's `projects/*/<id>.jsonl`, wherever claude encoded the cwd.
pub fn transcript_exists(config_dir: &Path, id: &str) -> bool {
    let Ok(rd) = std::fs::read_dir(config_dir.join("projects")) else { return false };
    rd.flatten().any(|d| d.path().join(format!("{id}.jsonl")).is_file())
}

pub fn config_dir(env: &std::collections::BTreeMap<String, String>) -> PathBuf {
    match env.get("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(env.get("HOME").cloned().unwrap_or_default()).join(".claude"),
    }
}

/// True unless a tool call is in flight: the last whole message in the stream-json trace is
/// an assistant turn that asked for a tool and has had no result yet.
pub fn at_boundary(trace_tail: &str) -> bool {
    for line in trace_tail.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { continue };
        match v.get("type").and_then(Value::as_str) {
            Some("stream_event") | None => continue,
            Some("assistant") => {
                let blocks = v.pointer("/message/content").and_then(Value::as_array);
                return !blocks.is_some_and(|b| b.iter().any(|c| c.get("type").and_then(Value::as_str) == Some("tool_use")));
            }
            Some(_) => return true,
        }
    }
    true
}

/// The last 64 KiB of the trace, cut to whole lines.
pub fn trace_tail(log: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(log) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let from = len.saturating_sub(64 * 1024);
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(from)).is_err() || f.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let s = String::from_utf8_lossy(&buf).into_owned();
    if from > 0 {
        s.split_once('\n').map(|(_, rest)| rest.to_string()).unwrap_or_default()
    } else {
        s
    }
}

pub fn resume_message(stopped_at: Option<i64>, now: i64, branch: &str) -> String {
    let down = match stopped_at {
        Some(t) if now >= t => format!(" for {}", duration(now - t)),
        _ => String::new(),
    };
    format!(
        "The world was stopped while you worked{down}, and has started again. This is your own session, resumed: \
         your worktree and branch {branch} are as you left them, and nothing was charged for the stop. \
         Anything you were running when it stopped did not finish — check before you rely on it, then carry on from where you were.\n"
    )
}

fn duration(secs: i64) -> String {
    match secs {
        s if s < 90 => format!("{s}s"),
        s if s < 90 * 60 => format!("{}m", s / 60),
        s if s < 48 * 3600 => format!("{}h {}m", s / 3600, s % 3600 / 60),
        s => format!("{}d {}h", s / 86400, s % 86400 / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0b9d6f3a-1c2e-4f5a-8b7c-9d0e1f2a3b4c";

    fn session(fayth: &str, persona: &str) -> Fact {
        Fact { kind: SESSION.into(), cause: SessionFact { id: ID.into(), fayth: fayth.into(), persona: persona.into(), worktree: "/w/sp-a".into() }.cause() }
    }

    fn ck(at: i64) -> Fact {
        Fact { kind: CHECKPOINTED.into(), cause: at.to_string() }
    }

    #[test]
    fn a_session_id_is_a_v4_uuid_and_differs_each_time() {
        let (a, b) = (new_session_id(), new_session_id());
        assert!(is_uuid(&a) && a.as_bytes()[14] == b'4' && a != b, "{a} {b}");
    }

    #[test]
    fn the_session_fact_round_trips_and_keeps_a_worktree_with_spaces() {
        let f = SessionFact { id: ID.into(), fayth: "builder".into(), persona: "abc123".into(), worktree: "/a b/c".into() };
        assert_eq!(SessionFact::parse(&f.cause()), Some(f));
        assert_eq!(SessionFact::parse("not-a-uuid builder x /w"), None);
    }

    #[test]
    fn facts_parse_from_the_lc_listing() {
        let j = format!(r#"[{{"issue_id":"sp-a","event_type":"session","new_value":"{ID} b p /w","actor":"x"}},{{"issue_id":"sp-a","event_type":"checkpointed","new_value":"100"}}]"#);
        let f = parse_facts(&j);
        assert_eq!((f[0].kind.as_str(), f[1].cause.as_str()), (SESSION, "100"));
        assert!(parse_facts("garbage").is_empty());
    }

    fn decide(facts: &[Fact], fayth: &str, persona: &str, wt: bool, tr: bool) -> Plan {
        plan(facts, fayth, persona, wt, |_| tr)
    }

    #[test]
    fn a_pending_checkpoint_resumes_its_session() {
        let facts = [session("builder", "p1"), ck(100)];
        assert_eq!(
            decide(&facts, "builder", "p1", true, true),
            Plan::Resume { session: SessionFact::parse(&facts[0].cause).unwrap(), stopped_at: Some(100) }
        );
    }

    #[test]
    fn no_checkpoint_or_an_already_consumed_one_is_a_fresh_claim() {
        assert_eq!(decide(&[], "builder", "p1", true, true), Plan::Fresh);
        assert_eq!(decide(&[session("builder", "p1")], "builder", "p1", true, true), Plan::Fresh);
        assert_eq!(decide(&[session("builder", "p1"), ck(100), session("builder", "p1")], "builder", "p1", true, true), Plan::Fresh);
    }

    #[test]
    fn every_unresumable_session_falls_back_cold_and_says_why() {
        let facts = [session("builder", "p1"), ck(100)];
        for (fayth, persona, wt, tr, why) in [
            ("ops", "p1", true, true, "ran as builder"),
            ("builder", "p2", true, true, "persona changed"),
            ("builder", "p1", false, true, "worktree"),
            ("builder", "p1", true, false, "transcript"),
        ] {
            match decide(&facts, fayth, persona, wt, tr) {
                Plan::Cold { reason } => assert!(reason.contains(why), "{reason}"),
                other => panic!("{other:?}"),
            }
        }
        assert!(matches!(decide(&[ck(100)], "builder", "p1", true, true), Plan::Cold { .. }));
    }

    #[test]
    fn the_transcript_is_found_under_any_encoded_cwd() {
        let d = testkit::TempDir::new("ckpt-transcript");
        std::fs::create_dir_all(d.join("projects/-w-sp-a")).unwrap();
        assert!(!transcript_exists(&d, ID));
        std::fs::write(d.join("projects/-w-sp-a").join(format!("{ID}.jsonl")), "{}\n").unwrap();
        assert!(transcript_exists(&d, ID));
    }

    #[test]
    fn a_stop_waits_only_while_a_tool_is_in_flight() {
        let tool = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"x"},{"type":"tool_use","id":"t","name":"Bash","input":{}}]}}"#;
        let result = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t"}]}}"#;
        let text = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"done"}]}}"#;
        let partial = r#"{"type":"stream_event","event":{}}"#;
        assert!(!at_boundary(&format!("{tool}\n{partial}\n")), "tool_use with no result is mid-tool");
        assert!(at_boundary(&format!("{tool}\n{result}\n")));
        assert!(at_boundary(&format!("{tool}\n{result}\n{text}\n{partial}\n")));
        assert!(at_boundary(""));
    }

    #[test]
    fn the_resume_message_says_the_world_was_down_and_for_how_long() {
        let m = resume_message(Some(1000), 1000 + 3 * 3600 + 120, "spira/sp-a");
        assert!(m.contains("world was stopped") && m.contains("3h 2m") && m.contains("spira/sp-a"), "{m}");
        assert!(resume_message(None, 5, "b").contains("worked, and has started"));
    }
}
