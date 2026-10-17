// The hold sweep's IO seam: reads held beads from the lifecycle store and delivered questions
// from the mailboxes, and takes each step through `spira-lc` and `mail`. The rule that picks
// the step is reconciler_engine::holds; nothing here decides.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use reconciler_engine::holds::{Action, AskMail, Env, Exit, Hold, Kind};
use serde_json::Value;

/// Mailboxes a question can be delivered to: an out-of-class ask is rerouted to the Concierge.
const ASK_MAILBOXES: [&str; 2] = ["operator", "concierge"];
const HISTORY_TAIL: usize = 8;

pub struct Live {
    pub lc_bin: String,
    pub mail_sh: String,
    pub mail_root: PathBuf,
    pub actor: String,
    pub now: u64,
}

fn field<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    v.get(k).filter(|x| !x.is_null())
}

fn text(v: &Value, k: &str) -> Option<String> {
    match field(v, k)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn epoch(v: &Value, k: &str) -> Option<u64> {
    text(v, k)?.trim().parse::<f64>().ok().filter(|f| *f >= 0.0).map(|f| f as u64)
}

pub fn bead_ids(list_json: &str) -> Result<Vec<String>, String> {
    let v: Value = serde_json::from_str(list_json.trim()).map_err(|e| format!("spira-lc list: not JSON: {e}"))?;
    let rows = v.as_array().ok_or("spira-lc list: not a JSON array")?;
    Ok(rows.iter().filter_map(|r| text(r, "bead_id")).collect())
}

fn same_kind(s: &str, k: Kind) -> bool {
    s.eq_ignore_ascii_case(k.as_str())
}

/// What the event log says about the live hold of `kind`: when and by whom it was placed, and
/// its reason text. A hold lifted since (or never recorded) is `None`.
pub fn live_hold_from_history(history_json: &str, kind: Kind) -> Result<Option<Hold>, String> {
    let v: Value = serde_json::from_str(history_json.trim()).map_err(|e| format!("spira-lc history: not JSON: {e}"))?;
    let events = v.as_array().ok_or("spira-lc history: not a JSON array")?;
    let mut live: Option<Hold> = None;
    for e in events {
        if text(e, "applied").is_some_and(|a| a == "0" || a == "false") {
            continue;
        }
        let evidence = match field(e, "evidence") {
            Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
            Some(other) => other.clone(),
            None => Value::Null,
        };
        let name = text(e, "event").unwrap_or_default();
        let body = evidence.get(&name).cloned().unwrap_or(Value::Null);
        let hit = text(&body, "kind").is_some_and(|k| same_kind(&k, kind));
        match name.as_str() {
            "Hold" if hit => {
                live = Some(Hold {
                    bead: text(e, "lc_key").unwrap_or_default(),
                    kind,
                    since: epoch(e, "at"),
                    detail: text(&body, "detail"),
                    actor: text(e, "actor"),
                });
            }
            "Unhold" if hit => live = None,
            "Reply" | "AskWithdrawn" if kind == Kind::Ask => live = None,
            _ => {}
        }
    }
    Ok(live)
}

/// Who places the snooze that ends a session which moved nothing.
const NO_PROGRESS_ACTOR: &str = "aeon";

/// The claims on a bead that ended in a no-progress snooze since its last Submit or answer,
/// oldest first. A session the machinery could not judge leaves no snooze, so it is not here.
/// Empty while an ask on the bead is live.
pub fn exits_from_history(history_json: &str) -> Result<Vec<Exit>, String> {
    let v: Value = serde_json::from_str(history_json.trim()).map_err(|e| format!("spira-lc history: not JSON: {e}"))?;
    let events = v.as_array().ok_or("spira-lc history: not a JSON array")?;
    let (mut exits, mut ask_live) = (Vec::new(), false);
    for e in events {
        if text(e, "applied").is_some_and(|a| a == "0" || a == "false") {
            continue;
        }
        let evidence = match field(e, "evidence") {
            Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
            Some(other) => other.clone(),
            None => Value::Null,
        };
        let name = text(e, "event").unwrap_or_default();
        let body = evidence.get(&name).cloned().unwrap_or(Value::Null);
        let kind = text(&body, "kind").unwrap_or_default();
        match name.as_str() {
            "Submit" => exits.clear(),
            "Reply" | "AskWithdrawn" => {
                exits.clear();
                ask_live = false;
            }
            "Hold" if same_kind(&kind, Kind::Ask) => ask_live = true,
            "Unhold" if same_kind(&kind, Kind::Ask) => ask_live = false,
            "Hold" if same_kind(&kind, Kind::Wait) && text(e, "actor").is_some_and(|a| a == NO_PROGRESS_ACTOR) => {
                let detail = text(&body, "detail").unwrap_or_default();
                if spira_config::lc_state::snooze_until(&detail).is_some() && !spira_config::lc_state::is_checkpoint(&detail) {
                    exits.push(Exit { ended_at: epoch(e, "at").unwrap_or(0), note: detail });
                }
            }
            _ => {}
        }
    }
    Ok(if ask_live { Vec::new() } else { exits })
}

/// True when the last applied event left the bead in a terminal state: the machine refuses
/// every Unhold there, so a hold it still lists is not the sweep's to lift.
pub fn ends_terminal(history_json: &str) -> Result<bool, String> {
    let v: Value = serde_json::from_str(history_json.trim()).map_err(|e| format!("spira-lc history: not JSON: {e}"))?;
    let events = v.as_array().ok_or("spira-lc history: not a JSON array")?;
    let last = events.iter().rev().find(|e| !text(e, "applied").is_some_and(|a| a == "0" || a == "false"));
    Ok(last.and_then(|e| text(e, "to_state")).is_some_and(|s| TERMINAL_STATES.iter().any(|t| s.eq_ignore_ascii_case(t))))
}

const TERMINAL_STATES: [&str; 4] = ["LANDED", "SUPERSEDED", "DROPPED", "DONE"];

/// The newest delivered question per work bead in `mailboxes`. `Err` when no mailbox could be
/// read at all: that is "cannot tell", never "no questions".
pub fn scan_asks(mail_root: &Path, mailboxes: &[&str]) -> Result<BTreeMap<String, AskMail>, String> {
    let mut out: BTreeMap<String, AskMail> = BTreeMap::new();
    let mut readable = 0;
    for mailbox in mailboxes {
        for sub in ["new", "cur"] {
            let Ok(dir) = fs::read_dir(mail_root.join(mailbox).join(sub)) else { continue };
            readable += 1;
            for entry in dir.flatten() {
                let Some((bead, kind)) = question_headers(&entry.path()) else { continue };
                if kind != "question" && kind != "decision" {
                    continue;
                }
                let at = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                if out.get(&bead).is_none_or(|m| at > m.delivered_at) {
                    out.insert(bead, AskMail { delivered_at: at, mailbox: (*mailbox).to_string() });
                }
            }
        }
    }
    if readable == 0 {
        return Err(format!("no mailbox under {} could be read", mail_root.display()));
    }
    Ok(out)
}

fn question_headers(path: &Path) -> Option<(String, String)> {
    let f = fs::File::open(path).ok()?;
    let (mut bead, mut kind) = (None, String::new());
    for line in BufReader::new(f).lines() {
        let line = line.ok()?;
        if line.trim().is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("X-Spira-Work-Bead:") {
            bead = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("X-Spira-Kind:") {
            kind = v.trim().to_string();
        }
    }
    Some((bead.filter(|b| !b.is_empty())?, kind))
}

fn lc(bin: &str, args: &[&str]) -> Result<String, String> {
    let out = spira_config::bounded::bounded(bin)
        .args(args)
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("{bin}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{bin} {}: exit {}: {}", args.join(" "), out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn lift_wait_result(unhold: Result<String, String>, until: impl std::fmt::Display) -> Result<(), String> {
    match unhold {
        Ok(_) => Ok(()),
        Err(e) if e.contains("Terminal") => Ok(()),
        Err(e) => Err(format!("{e} (snooze expired at {until})")),
    }
}

fn history_tail(history_json: &str) -> String {
    let Ok(Value::Array(events)) = serde_json::from_str::<Value>(history_json.trim()) else { return "(history unreadable)\n".into() };
    let start = events.len().saturating_sub(HISTORY_TAIL);
    let mut out = String::new();
    for e in &events[start..] {
        out.push_str(&format!(
            "  #{} {} {}->{} applied={} actor={} refusal={}\n",
            text(e, "seq").unwrap_or_default(),
            text(e, "event").unwrap_or_default(),
            text(e, "from_state").unwrap_or_default(),
            text(e, "to_state").unwrap_or_default(),
            text(e, "applied").unwrap_or_default(),
            text(e, "actor").unwrap_or_default(),
            text(e, "refusal").unwrap_or_else(|| "-".into()),
        ));
    }
    out
}

impl Live {
    fn send(&self, mailbox: &str, subject: &str, body: &str) -> Result<(), String> {
        let mut child = spira_config::bounded::bounded(&self.mail_sh)
            .args(["send", mailbox, "--from", "Reconciler <reconciler@spira>", "--subject", subject, "--kind", "note"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{}: {e}", self.mail_sh))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(body.as_bytes());
        }
        let out = child.wait_with_output().map_err(|e| format!("{}: {e}", self.mail_sh))?;
        if !out.status.success() {
            return Err(format!("{} send {mailbox}: exit {}: {}", self.mail_sh, out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(())
    }

    fn send_question(&self, bead: &str, subject: &str, body: &str) -> Result<(), String> {
        let mut child = spira_config::bounded::bounded(&self.mail_sh)
            .args(["send", "concierge", "--from", "Reconciler <reconciler@spira>", "--subject", subject, "--kind", "question", "--default", "drop or split the bead; it cannot progress with unchanged inputs", "--bead", bead])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{}: {e}", self.mail_sh))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(body.as_bytes());
        }
        let out = child.wait_with_output().map_err(|e| format!("{}: {e}", self.mail_sh))?;
        if !out.status.success() {
            return Err(format!("{} send concierge: exit {}: {}", self.mail_sh, out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(())
    }

    fn mailbox_exists(&self, mailbox: &str) -> bool {
        self.mail_root.join(mailbox).join("new").is_dir()
    }
}

impl Env for Live {
    fn holds(&mut self) -> Result<Vec<Hold>, String> {
        let mut out = Vec::new();
        for kind in Kind::ALL {
            let ids = bead_ids(&lc(&self.lc_bin, &["list", "--hold", kind.as_str(), "--live", "--snapshot"])?)?;
            for id in ids {
                let history = lc(&self.lc_bin, &["history", &id, "--machine", "bead"])?;
                if ends_terminal(&history)? {
                    continue;
                }
                let placed = live_hold_from_history(&history, kind)?;
                out.push(placed.map(|h| Hold { bead: id.clone(), ..h }).unwrap_or(Hold {
                    bead: id,
                    kind,
                    since: None,
                    detail: None,
                    actor: None,
                }));
            }
        }
        Ok(out)
    }

    fn asks(&mut self) -> Result<BTreeMap<String, AskMail>, String> {
        scan_asks(&self.mail_root, &ASK_MAILBOXES)
    }

    fn exits(&mut self, bead: &str) -> Result<Vec<Exit>, String> {
        exits_from_history(&lc(&self.lc_bin, &["history", bead, "--machine", "bead"])?)
    }

    fn act(&mut self, hold: &Hold, action: &Action) -> Result<(), String> {
        let bead = hold.bead.as_str();
        let reason = hold.detail.as_deref().unwrap_or("(no reason recorded)");
        let age_h = |t: Option<u64>, now: u64| t.map(|s| format!("{}h", now.saturating_sub(s) / 3600)).unwrap_or_else(|| "unknown".into());
        let now = self.now;
        match action {
            Action::TripBreaker { exits, window_secs } => {
                let evidence: String = exits
                    .iter()
                    .map(|x| format!("  epoch {} no-progress (unjudged-no-progress): {}\n", x.ended_at, x.note))
                    .collect();
                let last = exits.last().map(|x| x.note.as_str()).unwrap_or("");
                let n = exits.len();
                let question = format!("{n} claims ended without progress inside {}m; last: {last}", window_secs / 60);
                lc(&self.lc_bin, &["hold", bead, "ask", &question, &self.actor])?;
                self.send_question(
                    bead,
                    &format!("NO-PROGRESS BREAKER: {bead} had {n} claims end without progress inside {}m", window_secs / 60),
                    &format!(
                        "bead     {bead}\n\nEach claim ended without a Submit and nothing it did moved the branch:\n{evidence}\nThe last aeon's final word: {last}\n\nThe lifecycle has stopped re-summoning it. Only your answer lifts the hold: change an input the aeon was blocked on, split or drop the bead.\n"
                    ),
                )
            }
            Action::LiftWait { until } => lift_wait_result(lc(&self.lc_bin, &["unhold", bead, "wait", &self.actor]), *until),
            Action::RaiseAsk => self.send(
                "concierge",
                &format!("HOLD SWEEP: {bead} is held on an ask that was never delivered"),
                &format!(
                    "bead     {bead}\nheld for {}\nquestion {reason}\n\nNo delivered question exists in the operator or concierge mailbox for this bead, so nobody has been asked. Deliver it (work ask) or withdraw it (spira-lc withdraw-ask). The sweep does not lift an ask.\n",
                    age_h(hold.since, now)
                ),
            ),
            Action::ResurfaceAsk { delivered_at, mailbox } => self.send(
                mailbox,
                &format!("HOLD SWEEP: {bead} has waited {}h for an answer", now.saturating_sub(*delivered_at) / 3600),
                &format!("bead     {bead}\nquestion {reason}\n\nThe question was delivered {}h ago and the bead is still held on it. This is the one reminder; the sweep does not lift an ask.\n", now.saturating_sub(*delivered_at) / 3600),
            ),
            Action::DiagnosePoison => {
                let history = lc(&self.lc_bin, &["history", bead, "--machine", "bead"]).map(|h| history_tail(&h)).unwrap_or_else(|e| format!("(history unreadable: {e})\n"));
                let show = lc(&self.lc_bin, &["show", bead]).unwrap_or_else(|e| format!("(show unreadable: {e})"));
                self.send(
                    "concierge",
                    &format!("HOLD SWEEP: {bead} poisoned for {} with no new tip — diagnose", age_h(hold.since, now)),
                    &format!(
                        "bead     {bead}\npoisoned by {}\nreason   {reason}\n\nlast {HISTORY_TAIL} lifecycle events:\n{history}\ncurrent row:\n{show}\n\nUnpoison it (spira-lc unhold {bead} poison) or drop it. The sweep never lifts a poison.\n",
                        hold.actor.as_deref().unwrap_or("unknown")
                    ),
                )
            }
            Action::RemindManual => {
                let placer = hold.actor.as_deref().filter(|a| self.mailbox_exists(a)).unwrap_or("concierge");
                self.send(
                    placer,
                    &format!("HOLD SWEEP: manual hold on {bead} is {} old", age_h(hold.since, now)),
                    &format!(
                        "bead     {bead}\nplaced by {}\nreason   {reason}\n\nCheck whether that condition still holds; if it does not, lift it (unhold.sh {bead}).\n",
                        hold.actor.as_deref().unwrap_or("unknown")
                    ),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HISTORY: &str = r#"[
      {"seq":1,"event":"Claim","applied":1,"actor":"aeon","at":100,"evidence":{"Claim":{}}},
      {"seq":2,"event":"Hold","applied":1,"actor":"sentinel","at":200,"evidence":{"Hold":{"kind":"Poison","cause":"attempts-exhausted","detail":"3 attempts"}}},
      {"seq":3,"event":"Hold","applied":0,"actor":"x","at":250,"evidence":{"Hold":{"kind":"Operator","cause":"manual-hold"}}},
      {"seq":4,"event":"Hold","applied":1,"actor":"concierge","at":300,"evidence":"{\"Hold\":{\"kind\":\"Operator\",\"cause\":\"manual-hold\",\"detail\":\"until publishing resumes\"}}"}
    ]"#;

    #[test]
    fn the_live_hold_is_read_from_its_own_event() {
        let p = live_hold_from_history(HISTORY, Kind::Poison).unwrap().unwrap();
        assert_eq!((p.since, p.actor.as_deref(), p.detail.as_deref()), (Some(200), Some("sentinel"), Some("3 attempts")));
        let m = live_hold_from_history(HISTORY, Kind::Operator).unwrap().unwrap();
        assert_eq!((m.since, m.actor.as_deref(), m.detail.as_deref()), (Some(300), Some("concierge"), Some("until publishing resumes")));
    }

    #[test]
    fn a_refused_hold_event_is_not_a_hold_and_a_kind_never_placed_is_none() {
        assert_eq!(live_hold_from_history(HISTORY, Kind::Wait).unwrap(), None);
        assert_eq!(live_hold_from_history(HISTORY, Kind::Ask).unwrap(), None);
    }

    #[test]
    fn a_dropped_bead_still_holding_wait_is_terminal_and_a_live_one_is_not() {
        let dropped = r#"[
          {"seq":1,"event":"Hold","applied":1,"to_state":"READY","evidence":{"Hold":{"kind":"Wait"}}},
          {"seq":2,"event":"Reclassify","applied":1,"to_state":"DROPPED","evidence":{}},
          {"seq":3,"event":"Unhold","applied":0,"to_state":"DROPPED","refusal":"Terminal","evidence":{}}
        ]"#;
        assert!(ends_terminal(dropped).unwrap());
        assert!(!ends_terminal(HISTORY).unwrap());
        assert!(!ends_terminal(r#"[{"seq":1,"event":"Hold","applied":1,"to_state":"READY","evidence":{}}]"#).unwrap());
    }

    #[test]
    fn an_unhold_or_a_reply_ends_the_hold() {
        let h = r#"[
          {"seq":1,"event":"Hold","applied":1,"actor":"a","at":1,"evidence":{"Hold":{"kind":"Poison"}}},
          {"seq":2,"event":"Unhold","applied":1,"actor":"a","at":2,"evidence":{"Unhold":{"kind":"Poison"}}},
          {"seq":3,"event":"Hold","applied":1,"actor":"a","at":3,"evidence":{"Hold":{"kind":"Ask","detail":"q"}}},
          {"seq":4,"event":"Reply","applied":1,"actor":"a","at":4,"evidence":{"Reply":{"message_id":"m"}}}
        ]"#;
        assert_eq!(live_hold_from_history(h, Kind::Poison).unwrap(), None);
        assert_eq!(live_hold_from_history(h, Kind::Ask).unwrap(), None);
    }

    #[test]
    fn a_wait_on_a_finished_bead_is_not_a_failure_to_lift() {
        let refused = Err("spira-lc unhold sp-a wait reconciler: exit 3: refused: Terminal".to_string());
        assert_eq!(lift_wait_result(refused, 5), Ok(()));
        assert_eq!(lift_wait_result(Ok(String::new()), 5), Ok(()));
        let other = lift_wait_result(Err("refused: StaleVersion".to_string()), 5).unwrap_err();
        assert!(other.contains("StaleVersion") && other.contains("snooze expired at 5"));
    }

    #[test]
    fn unparseable_history_is_an_error_not_no_hold() {
        assert!(live_hold_from_history("garbage", Kind::Poison).is_err());
        assert!(bead_ids("{}").is_err());
        assert_eq!(bead_ids(r#"[{"bead_id":"sp-1"},{"bead_id":"sp-2"}]"#).unwrap(), vec!["sp-1", "sp-2"]);
    }

    fn deliver(root: &Path, mailbox: &str, sub: &str, name: &str, headers: &str) {
        let dir = root.join(mailbox).join(sub);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(name), format!("{headers}\nBody\n")).unwrap();
    }

    #[test]
    fn a_delivered_question_is_found_by_its_work_bead_in_either_mailbox() {
        let dir = testkit::TempDir::new("hold-sweep-asks");
        deliver(&dir, "operator", "cur", "m1", "Subject: q\nX-Spira-Kind: question\nX-Spira-Work-Bead: sp-a\n");
        deliver(&dir, "concierge", "new", "m2", "Subject: q\nX-Spira-Kind: decision\nX-Spira-Work-Bead: sp-b\n");
        let asks = scan_asks(&dir, &ASK_MAILBOXES).unwrap();
        assert_eq!(asks.get("sp-a").map(|m| m.mailbox.as_str()), Some("operator"));
        assert_eq!(asks.get("sp-b").map(|m| m.mailbox.as_str()), Some("concierge"));
    }

    #[test]
    fn a_note_about_a_bead_is_not_a_delivered_question() {
        let dir = testkit::TempDir::new("hold-sweep-notes");
        deliver(&dir, "concierge", "new", "m1", "Subject: n\nX-Spira-Kind: note\nX-Spira-Work-Bead: sp-a\n");
        deliver(&dir, "concierge", "new", "m2", "Subject: q\nX-Spira-Kind: question\n");
        assert!(scan_asks(&dir, &ASK_MAILBOXES).unwrap().is_empty());
    }

    #[test]
    fn no_readable_mailbox_is_cannot_tell() {
        let dir = testkit::TempDir::new("hold-sweep-nomail");
        assert!(scan_asks(&dir, &ASK_MAILBOXES).is_err());
    }

    fn ev(seq: usize, event: &str, actor: &str, at: u64, body: &str) -> String {
        format!(r#"{{"seq":{seq},"event":"{event}","applied":1,"actor":"{actor}","at":{at},"evidence":{{"{event}":{body}}}}}"#)
    }

    /// One claim that ends in the no-progress snooze the aeon places, as sp-6rejya's history recorded them.
    fn no_progress_cycle(events: &mut Vec<String>, at: u64) {
        let n = events.len();
        events.push(ev(n + 1, "Claim", "aeon", at, "{}"));
        events.push(ev(n + 2, "Release", "aeon", at + 60, "null"));
        let hold = format!(r#"{{"kind":"Wait","cause":"unlanded-blocker","detail":"snooze-until:{} no-progress: needs the Concierge"}}"#, at + 1800);
        events.push(ev(n + 3, "Hold", "aeon", at + 61, &hold));
        events.push(ev(n + 4, "Unhold", "reconciler", at + 1900, r#"{"kind":"Wait"}"#));
    }

    fn breaker() -> reconciler_engine::holds::Breaker {
        reconciler_engine::holds::Breaker::new(4, 180).unwrap()
    }

    fn trips(events: &[String]) -> bool {
        let history = format!("[{}]", events.join(","));
        breaker().tripped(&exits_from_history(&history).unwrap()).is_some()
    }

    #[test]
    fn replaying_a_history_of_no_progress_claims_trips_on_the_fourth_and_not_before() {
        let mut events = Vec::new();
        let mut first_trip = None;
        for cycle in 0..8u64 {
            no_progress_cycle(&mut events, 1_000_000 + cycle * 2000);
            if first_trip.is_none() && trips(&events) {
                first_trip = Some(cycle + 1);
            }
        }
        assert_eq!(first_trip, Some(4));
    }

    #[test]
    fn once_the_ask_is_placed_the_same_history_trips_no_more() {
        let mut events = Vec::new();
        for cycle in 0..4u64 {
            no_progress_cycle(&mut events, 1_000_000 + cycle * 2000);
        }
        assert!(trips(&events));
        events.push(ev(events.len() + 1, "Hold", "reconciler", 1_010_000, r#"{"kind":"Ask","cause":"operator-question","detail":"q"}"#));
        assert!(!trips(&events));
        events.push(ev(events.len() + 1, "Reply", "concierge", 1_011_000, r#"{"message_id":"m"}"#));
        assert!(!trips(&events), "an answer clears the count it answered");
    }

    #[test]
    fn a_harness_fault_exit_is_not_a_no_progress_claim() {
        let mut events = Vec::new();
        for cycle in 0..3u64 {
            no_progress_cycle(&mut events, 1_000_000 + cycle * 2000);
        }
        let n = events.len();
        events.push(ev(n + 1, "Claim", "aeon", 1_006_000, "{}"));
        events.push(ev(n + 2, "Release", "aeon", 1_006_060, "null"));
        assert!(!trips(&events));
        no_progress_cycle(&mut events, 1_007_000);
        assert!(trips(&events), "positive control: a fourth no-progress exit does trip");
    }

    #[test]
    fn four_claims_spread_over_more_than_the_window_do_not_trip() {
        let mut events = Vec::new();
        for cycle in 0..4u64 {
            no_progress_cycle(&mut events, 1_000_000 + cycle * 4000);
        }
        assert!(!trips(&events));
    }

    #[test]
    fn a_submit_ends_the_count() {
        let mut events = Vec::new();
        for cycle in 0..3u64 {
            no_progress_cycle(&mut events, 1_000_000 + cycle * 2000);
        }
        events.push(ev(events.len() + 1, "Submit", "aeon", 1_006_000, r#"{"tip":"t"}"#));
        no_progress_cycle(&mut events, 1_007_000);
        assert!(!trips(&events));
    }

    #[test]
    fn the_sweep_lists_only_live_beads_so_a_terminal_row_is_never_unheld() {
        let dir = testkit::TempDir::new("hold-sweep-live");
        let log = dir.join("calls");
        let bin = dir.join("fake-lc");
        testkit::write_exe(&bin, &format!("#!/bin/sh\necho \"$@\" >> {}\necho '[]'\n", log.display()));
        let mut live = Live { lc_bin: bin.display().to_string(), mail_sh: String::new(), mail_root: dir.to_path_buf(), actor: "reconciler".into(), now: 0 };
        assert!(live.holds().unwrap().is_empty());
        let calls = fs::read_to_string(&log).unwrap();
        assert_eq!(calls.lines().count(), Kind::ALL.len());
        assert!(calls.lines().all(|l| l.starts_with("list --hold ") && l.ends_with(" --live --snapshot")), "{calls}");
    }
}
