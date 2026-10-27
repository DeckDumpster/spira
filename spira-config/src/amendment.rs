//! Acknowledged amendments to in-flight work: a record per amendment under the holder's
//! mailbox, which the PreToolUse hook, `work ack`, `work submit` and `bead.sh amend` all read.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub const DEFAULT_ACK_SECS: i64 = 600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Pending,
    Accepted,
    Declined(String),
    Enforced,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub id: String,
    pub bead: String,
    pub amender: String,
    pub text: String,
    pub created: i64,
    pub deadline: i64,
    pub state: State,
}

pub fn dir(mail: &str, bead: &str) -> PathBuf {
    Path::new(mail).join(format!("dwarf-{bead}")).join("amend")
}

fn file(mail: &str, bead: &str, id: &str) -> PathBuf {
    dir(mail, bead).join(format!("{id}.json"))
}

pub fn is_amend_id(s: &str) -> bool {
    s.strip_prefix("am-").is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_hexdigit()))
}

/// `10m`, `90s`, `2h`, or bare seconds.
pub fn parse_duration(s: &str) -> Option<i64> {
    let s = s.trim();
    let (num, mult) = match s.chars().last()? {
        's' => (&s[..s.len() - 1], 1),
        'm' => (&s[..s.len() - 1], 60),
        'h' => (&s[..s.len() - 1], 3600),
        c if c.is_ascii_digit() => (s, 1),
        _ => return None,
    };
    let n: i64 = num.parse().ok()?;
    (n > 0).then_some(n * mult)
}

fn state_json(st: &State) -> (&'static str, String) {
    match st {
        State::Pending => ("pending", String::new()),
        State::Accepted => ("accepted", String::new()),
        State::Declined(why) => ("declined", why.clone()),
        State::Enforced => ("enforced", String::new()),
    }
}

fn to_json(r: &Record) -> Value {
    let (state, reason) = state_json(&r.state);
    json!({"id": r.id, "bead": r.bead, "amender": r.amender, "text": r.text,
           "created": r.created, "deadline": r.deadline, "state": state, "reason": reason})
}

fn from_json(v: &Value) -> Option<Record> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let state = match v.get("state")?.as_str()? {
        "pending" => State::Pending,
        "accepted" => State::Accepted,
        "declined" => State::Declined(s("reason").unwrap_or_default()),
        "enforced" => State::Enforced,
        _ => return None,
    };
    Some(Record {
        id: s("id")?,
        bead: s("bead")?,
        amender: s("amender")?,
        text: s("text")?,
        created: v.get("created")?.as_i64()?,
        deadline: v.get("deadline")?.as_i64()?,
        state,
    })
}

fn write(mail: &str, r: &Record) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let d = dir(mail, &r.bead);
    std::fs::create_dir_all(&d).map_err(|e| format!("creating {}: {e}", d.display()))?;
    let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755));
    let path = file(mail, &r.bead, &r.id);
    let tmp = d.join(format!(".{}.tmp", r.id));
    std::fs::write(&tmp, to_json(r).to_string()).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644));
    std::fs::rename(&tmp, &path).map_err(|e| format!("renaming into {}: {e}", path.display()))
}

pub fn create(mail: &str, bead: &str, amender: &str, text: &str, now: i64, ack_secs: i64) -> Result<Record, String> {
    let mut n = (now as u64) ^ ((std::process::id() as u64) << 20);
    let id = loop {
        let id = format!("am-{:x}", n & 0xff_ffff_ffff);
        if !file(mail, bead, &id).exists() {
            break id;
        }
        n = n.wrapping_add(1);
    };
    let r = Record { id, bead: bead.into(), amender: amender.into(), text: text.into(), created: now, deadline: now + ack_secs, state: State::Pending };
    write(mail, &r)?;
    Ok(r)
}

pub fn load(mail: &str, bead: &str, id: &str) -> Option<Record> {
    let text = std::fs::read_to_string(file(mail, bead, id)).ok()?;
    from_json(&serde_json::from_str(&text).ok()?)
}

/// The record named `id` on whichever bead holds it.
pub fn find(mail: &str, id: &str) -> Option<Record> {
    if !is_amend_id(id) {
        return None;
    }
    std::fs::read_dir(mail).ok()?.filter_map(|e| e.ok()).find_map(|e| {
        let name = e.file_name().into_string().ok()?;
        load(mail, name.strip_prefix("dwarf-")?, id)
    })
}

pub fn all(mail: &str, bead: &str) -> Vec<Record> {
    let Ok(rd) = std::fs::read_dir(dir(mail, bead)) else { return vec![] };
    let mut v: Vec<Record> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|t| from_json(&serde_json::from_str(&t).ok()?))
        .collect();
    v.sort_by(|a, b| (a.created, &a.id).cmp(&(b.created, &b.id)));
    v
}

pub fn unanswered(mail: &str, bead: &str) -> Vec<Record> {
    all(mail, bead).into_iter().filter(|r| r.state == State::Pending).collect()
}

/// Sets the state of a pending record; anything already answered is refused.
pub fn answer(mail: &str, bead: &str, id: &str, to: State) -> Result<Record, String> {
    let mut r = load(mail, bead, id).ok_or_else(|| format!("no amendment {id} on {bead}"))?;
    if r.state != State::Pending {
        return Err(format!("amendment {id} is already {}", state_json(&r.state).0));
    }
    r.state = to;
    write(mail, &r)?;
    Ok(r)
}

pub fn mark_enforced(mail: &str, r: &Record) -> Result<(), String> {
    let mut r = r.clone();
    r.state = State::Enforced;
    write(mail, &r)
}

/// Why `--enforce` may proceed now, or why it may not.
pub fn enforceable(r: &Record, now: i64) -> Result<&'static str, String> {
    match &r.state {
        State::Declined(_) => Ok("declined"),
        State::Pending if now >= r.deadline => Ok("unanswered past its deadline"),
        State::Pending => Err(format!("amendment {} is still inside its ack window ({}s left)", r.id, r.deadline - now)),
        State::Accepted => Err(format!("amendment {} was accepted; there is nothing to enforce", r.id)),
        State::Enforced => Err(format!("amendment {} was already enforced", r.id)),
    }
}

/// The refusal `work submit` / `work done` owe while an amendment is unanswered.
pub fn submit_refusal(mail: &str, bead: &str) -> Option<String> {
    let open = unanswered(mail, bead);
    if open.is_empty() {
        return None;
    }
    let ids = open.iter().map(|r| r.id.as_str()).collect::<Vec<_>>().join(", ");
    Some(format!(
        "refused: {bead} has an unanswered amendment ({ids}); answer it with `work ack <id> --accept` or `work ack <id> --decline \"<why>\"` before you submit"
    ))
}

/// What the hook shows the blocked dwarf.
pub fn block_text(r: &Record) -> String {
    format!(
        "Amendment {id} from {who} must be answered before you do anything else.\n\n{text}\n\nRun `work ack {id} --accept` to take it on, or `work ack {id} --decline \"<why>\"`. Unanswered by deadline (epoch {dl}) or declined, the amender may slay this session and requeue the bead with the amendment merged in.",
        id = r.id,
        who = r.amender,
        text = r.text.trim(),
        dl = r.deadline
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> testkit::TempDir {
        let d = testkit::TempDir::new(tag);
        std::fs::create_dir_all(d.join("dwarf-sp-x1")).unwrap();
        d
    }

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration("10m"), Some(600));
        assert_eq!(parse_duration("90s"), Some(90));
        assert_eq!(parse_duration("2h"), Some(7200));
        assert_eq!(parse_duration("45"), Some(45));
        assert_eq!(parse_duration("0m"), None);
        assert_eq!(parse_duration("m"), None);
        assert_eq!(parse_duration("x"), None);
    }

    #[test]
    fn an_amend_id_is_never_a_bead_id() {
        let d = tmp("amend-id");
        let m = d.to_string_lossy().into_owned();
        let r = create(&m, "sp-x1", "concierge", "t", 1000, 600).unwrap();
        assert!(is_amend_id(&r.id));
        assert!(!r.id.starts_with("sp-"));
    }

    #[test]
    fn pending_blocks_submit_until_answered() {
        let d = tmp("amend-submit");
        let m = d.to_string_lossy().into_owned();
        let r = create(&m, "sp-x1", "concierge", "use 709cfac", 1000, 600).unwrap();
        assert!(submit_refusal(&m, "sp-x1").unwrap().contains(&r.id));
        answer(&m, "sp-x1", &r.id, State::Accepted).unwrap();
        assert!(submit_refusal(&m, "sp-x1").is_none());
    }

    #[test]
    fn a_decline_records_the_reason_and_cannot_be_answered_twice() {
        let d = tmp("amend-decline");
        let m = d.to_string_lossy().into_owned();
        let r = create(&m, "sp-x1", "concierge", "t", 1000, 600).unwrap();
        answer(&m, "sp-x1", &r.id, State::Declined("wrong repo".into())).unwrap();
        assert_eq!(load(&m, "sp-x1", &r.id).unwrap().state, State::Declined("wrong repo".into()));
        assert!(answer(&m, "sp-x1", &r.id, State::Accepted).is_err());
        assert!(submit_refusal(&m, "sp-x1").is_none());
    }

    #[test]
    fn enforce_waits_for_the_deadline_unless_declined() {
        let d = tmp("amend-enforce");
        let m = d.to_string_lossy().into_owned();
        let r = create(&m, "sp-x1", "concierge", "t", 1000, 600).unwrap();
        assert!(enforceable(&r, 1599).is_err());
        assert!(enforceable(&r, 1600).is_ok());
        let d = answer(&m, "sp-x1", &r.id, State::Declined("no".into())).unwrap();
        assert!(enforceable(&d, 1001).is_ok());
        let mut a = d.clone();
        a.state = State::Accepted;
        assert!(enforceable(&a, 9999).is_err());
        mark_enforced(&m, &d).unwrap();
        assert!(enforceable(&load(&m, "sp-x1", &d.id).unwrap(), 9999).is_err());
    }

    #[test]
    fn find_locates_a_record_by_id_alone() {
        let d = tmp("amend-find");
        let m = d.to_string_lossy().into_owned();
        let r = create(&m, "sp-x1", "concierge", "t", 1000, 600).unwrap();
        assert_eq!(find(&m, &r.id).unwrap().bead, "sp-x1");
        assert!(find(&m, "am-deadbeef").is_none());
        assert!(find(&m, "../etc").is_none());
    }
}
