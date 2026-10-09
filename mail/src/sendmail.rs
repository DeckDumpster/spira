//! `sendmail`: RFC 5322 mail on stdin. Closes the tracking bead a reply answers (via
//! `In-Reply-To`), routes the reply to the sender's mailbox (or `concierge` for a chamber
//! persona, or when there is nobody to route to), and delivers the raw message unchanged.

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::bead::{self, Bd};
use crate::lc::{self, Lc, Lift};
use crate::maildir;
use crate::message;

/// The first paragraph of the body: mail.sh's own awk skips the header/body blank line,
/// then any further leading blank lines, then collects lines up to the next blank line.
pub fn first_paragraph(raw: &str) -> String {
    let (_, body) = message::split_headers_body(raw);
    let mut lines = Vec::new();
    let mut started = false;
    for line in body.lines() {
        if line.trim().is_empty() {
            if started {
                break;
            }
            continue;
        }
        started = true;
        lines.push(line);
    }
    lines.join("\n")
}

/// Saves the message under `run_dir/spira-sendmail` before delivery; the caller removes it on
/// success, so what remains is exactly the messages that failed to send.
pub fn spool_message(run_dir: &Path, raw: &str) -> Option<PathBuf> {
    let dir = run_dir.join("spira-sendmail");
    fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{}.eml", maildir::mint_msgid()));
    fs::write(&path, raw).ok()?;
    Some(path)
}

fn strip_angle_brackets(s: &str) -> String {
    s.chars().filter(|c| *c != '<' && *c != '>').collect()
}

/// `_find_message_by_id`: scans every mailbox's `new/` (mailbox-name order), then every
/// mailbox's `cur/` (same order) — mail.sh's own two-pass glob (`"$SPIRA_MAIL"/*/new`, then
/// `"$SPIRA_MAIL"/*/cur`).
pub fn find_message_by_id(mail_root: &Path, msgid: &str) -> Option<PathBuf> {
    let msgid = msgid.strip_prefix('<').unwrap_or(msgid);
    let msgid = msgid.strip_suffix('>').unwrap_or(msgid);
    if msgid.is_empty() {
        return None;
    }
    let mut mailboxes: Vec<String> = fs::read_dir(mail_root)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    mailboxes.sort();

    for sub in ["new", "cur"] {
        for mbox in &mailboxes {
            for f in maildir::sorted_entries(&mail_root.join(mbox).join(sub)) {
                let text = fs::read_to_string(&f).unwrap_or_default();
                let fid = message::header_ci_before_blank(&text, "message-id");
                if strip_angle_brackets(&fid) == msgid {
                    return Some(f);
                }
            }
        }
    }
    None
}

/// `_reply_mailbox`: the sender's own mailbox, unless the local part names a chamber
/// persona (transient — nobody would read it), or no such mailbox exists — either way,
/// `concierge`.
pub fn reply_mailbox(from: &str, home: &Path, mail_root: &Path) -> String {
    let angled = Regex::new(r"<([^@>]+)@").unwrap();
    let bare = Regex::new(r"^([^@\s]+)@").unwrap();
    let localpart = angled
        .captures(from)
        .or_else(|| bare.captures(from))
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();

    if !localpart.is_empty() && home.join("chamber").join(format!("{localpart}.md")).is_file() {
        return "concierge".to_string();
    }
    if !localpart.is_empty() && maildir::mail_dir_exists_dir(mail_root, &localpart) {
        return localpart;
    }
    "concierge".to_string()
}

pub struct SendmailOutcome {
    pub dest_mailbox: String,
    pub delivered_path: PathBuf,
}

/// Runs the whole `sendmail` pipeline against raw RFC 5322 text already read from stdin.
///
/// An answer to a question about a work bead (the original carries `X-Spira-Work-Bead`)
/// also lifts that bead's `ask` hold — `spira-lc reply <work-bead> <message-id>` — once the
/// answer is delivered, so the bead becomes claimable again (sp-v62vn follow-up). When no
/// tracking bead carried the verdict to the work bead (a question the concierge was routed),
/// the answer is noted on the work bead itself, so the session that next claims it can read it.
pub fn sendmail(bd: &dyn Bd, lc: &dyn Lc, db_configured: bool, home: &Path, mail_root: &Path, mute: bool, raw: &str) -> Result<SendmailOutcome, String> {
    let in_reply_to = strip_angle_brackets(&message::header_ci_before_blank(raw, "in-reply-to"));
    let mut first_para = first_paragraph(raw);

    let mut dest_mailbox = "concierge".to_string();
    let mut orig_file: Option<PathBuf> = None;
    let mut orig_bead = String::new();
    let mut orig_kind = String::new();
    let mut work_bead = String::new();
    let mut orig_subject = String::new();

    if !in_reply_to.is_empty() {
        if let Some(path) = find_message_by_id(mail_root, &in_reply_to) {
            let text = fs::read_to_string(&path).unwrap_or_default();
            orig_bead = message::header_ci_before_blank(&text, "x-spira-bead");
            orig_kind = message::header_ci_before_blank(&text, "x-spira-kind");
            work_bead = message::header_ci_before_blank(&text, "x-spira-work-bead");
            orig_subject = message::header_ci_before_blank(&text, "subject");
            let orig_from = message::header_ci_before_blank(&text, "from");
            dest_mailbox = reply_mailbox(&orig_from, home, mail_root);
            orig_file = Some(path);
        }
    }

    if first_para.is_empty() {
        first_para = format!("answered by mail <{in_reply_to}> \"{orig_subject}\" (reply body empty)");
    }

    if !orig_bead.is_empty() {
        let warnings = bead::sendmail_close_bead(bd, db_configured, &orig_bead, &orig_kind, &first_para)?;
        for w in warnings {
            eprintln!("{w}");
        }
    }

    if let Some(path) = &orig_file {
        mark_replied(path);
    }

    let dir = mail_root.join(&dest_mailbox);
    maildir::mail_ensure(&dir).map_err(|e| format!("cannot ensure mailbox {dest_mailbox}: {e}"))?;
    let msgid = maildir::mint_msgid();
    fs::write(dir.join("tmp").join(&msgid), raw).map_err(|e| format!("cannot write message: {e}"))?;
    let delivered_path = maildir::mail_deliver(&dir, &msgid, mute).map_err(|e| format!("cannot deliver message: {e}"))?;

    if !work_bead.is_empty() {
        let actor = lc::actor_of(&message::header_ci_before_blank(raw, "from"), "operator");
        if orig_bead.is_empty() && db_configured {
            if let Err(e) = bead::note(bd, &work_bead, &format!("Answer from {actor} to \"{orig_subject}\": {first_para}")) {
                eprintln!("mail: could not note the answer on {work_bead}: {e}");
            }
        }
        let own_id = strip_angle_brackets(&message::header_ci_before_blank(raw, "message-id"));
        let answer_id = if own_id.is_empty() { format!("{msgid}@spira") } else { own_id };
        if let Some(w) = lc::lift_ask(lc, &work_bead, Lift::Reply { message_id: &answer_id }, &actor) {
            eprintln!("{w}");
        }
    }

    if !orig_bead.is_empty() && db_configured && (orig_kind == "question" || orig_kind == "decision") {
        let actor = lc::actor_of(&message::header_ci_before_blank(raw, "from"), "operator");
        let own_id = strip_angle_brackets(&message::header_ci_before_blank(raw, "message-id"));
        let answer_id = if own_id.is_empty() { format!("{msgid}@spira") } else { own_id };
        if let Some(w) = lc::answer_ask(lc, &orig_bead, &first_para, &actor, "mail", &answer_id) {
            eprintln!("{w}");
        }
    }

    Ok(SendmailOutcome { dest_mailbox, delivered_path })
}

/// `_mark_replied`: adds the Maildir `R` flag in place (directory unchanged — a message
/// found in `new/` keeps living in `new/`, just renamed). A no-op if `R` is already set.
pub fn mark_replied(path: &Path) {
    if !path.is_file() {
        return;
    }
    let base = match path.file_name().and_then(|f| f.to_str()) {
        Some(b) => b,
        None => return,
    };
    if let Some(new_name) = maildir::add_flag(base, 'R') {
        if let Some(dir) = path.parent() {
            let _ = fs::rename(path, dir.join(new_name));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead::fake::FakeBd;
    use crate::bead::BdOut;
    use crate::lc::fake::FakeLc;

    fn send_plain(mail_root: &Path, mailbox: &str, from: &str, subject: &str) -> String {
        maildir::mail_ensure(&mail_root.join(mailbox)).unwrap();
        let msgid = maildir::mint_msgid();
        let raw = format!("From: {from}\nSubject: {subject}\nMessage-ID: <{msgid}@spira>\n\nbody\n");
        fs::write(mail_root.join(mailbox).join("tmp").join(&msgid), raw).unwrap();
        maildir::mail_deliver(&mail_root.join(mailbox), &msgid, false).unwrap();
        msgid
    }

    fn reply_raw(in_reply_to: Option<&str>) -> String {
        let mut s = "From: Operator <operator@spira>\nSubject: Re: routing test\n".to_string();
        if let Some(id) = in_reply_to {
            s.push_str(&format!("In-Reply-To: <{id}@spira>\n"));
        }
        s.push_str("\nNoted.\n");
        s
    }

    #[test]
    fn first_paragraph_stops_at_the_next_blank_line() {
        let raw = "From: A\nSubject: S\n\n\nFirst line.\nSecond line.\n\nIgnored third paragraph.\n";
        assert_eq!(first_paragraph(raw), "First line.\nSecond line.");
    }

    #[test]
    fn a_spooled_message_is_readable_back() {
        let d = testkit::TempDir::new("mail-spool");
        let p = spool_message(d.path(), "hello").unwrap();
        assert_eq!(fs::read_to_string(p).unwrap(), "hello");
    }

    #[test]
    fn a_crlf_reply_body_is_found() {
        let raw = "From: A\r\nSubject: S\r\n\r\nApprove it.\r\nMore.\r\n\r\nIgnored.\r\n";
        assert_eq!(first_paragraph(raw), "Approve it.\nMore.");
    }

    #[test]
    fn an_empty_reply_body_still_closes_the_bead_with_a_traceable_reason() {
        let d = testkit::TempDir::new("mail-sendmail-empty");
        let home = d.path().join("home");
        fs::create_dir_all(home.join("chamber")).unwrap();
        let q = deliver_question(d.path(), "operator", "X-Spira-Bead: sp-dec1\n");
        let bd = FakeBd::new(vec![BdOut::ok(""), BdOut::ok("[]")]);
        let raw = format!("From: Operator <operator@spira>\nSubject: Re: May I?\nIn-Reply-To: <{q}@spira>\n\n\n");
        sendmail(&bd, &FakeLc::new(0), true, &home, d.path(), false, &raw).unwrap();
        let reason = bd.calls()[0].1.clone().unwrap();
        assert!(reason.contains(&q) && reason.contains("May I?"), "{reason}");
    }

    #[test]
    fn a_reply_to_a_question_closes_the_ask_on_the_ask_machine_with_his_words() {
        let d = testkit::TempDir::new("mail-sendmail-ask");
        let home = d.path().join("home");
        fs::create_dir_all(home.join("chamber")).unwrap();
        maildir::mail_ensure(&d.path().join("gate")).unwrap();
        let mut sent = Vec::new();
        for (kind, bead) in [("question", "sp-ask1"), ("note", "sp-note1")] {
            let msgid = maildir::mint_msgid();
            let orig = format!("From: Gate <gate@spira>\nSubject: S\nX-Spira-Kind: {kind}\nX-Spira-Bead: {bead}\nMessage-ID: <{msgid}@spira>\n\nbody\n");
            fs::write(d.path().join("gate/tmp").join(&msgid), &orig).unwrap();
            maildir::mail_deliver(&d.path().join("gate"), &msgid, false).unwrap();
            let lc = FakeLc::new(0);
            let bd = FakeBd::new(vec![BdOut::ok(""), BdOut::ok("[]")]);
            let raw = format!("From: Operator <operator@spira>\nSubject: Re: S\nMessage-ID: <ans-{kind}@spira>\nIn-Reply-To: <{msgid}@spira>\n\nApprove it.\n");
            sendmail(&bd, &lc, true, &home, d.path(), false, &raw).unwrap();
            sent.push(lc.calls());
        }
        assert_eq!(
            sent[0],
            vec![vec!["close-ask", "sp-ask1", "--exit", "answered", "--quote", "Approve it.", "--actor", "operator", "--channel", "mail", "--message-id", "ans-question@spira"]]
        );
        assert!(sent[1].is_empty(), "a note is no ask: {:?}", sent[1]);
    }

    #[test]
    fn reply_routes_to_the_senders_mailbox_when_one_exists() {
        let d = testkit::TempDir::new("mail-sendmail");
        let home = d.path().join("home");
        fs::create_dir_all(home.join("chamber")).unwrap();
        maildir::mail_ensure(&d.path().join("gate")).unwrap();
        let msgid = send_plain(d.path(), "uc17-orig1", "Gate <gate@spira>", "routing test");
        let bd = FakeBd::new(vec![]);
        let out = sendmail(&bd, &FakeLc::new(0), false, &home, d.path(), false, &reply_raw(Some(&msgid))).unwrap();
        assert_eq!(out.dest_mailbox, "gate");
    }

    #[test]
    fn reply_to_a_chamber_persona_routes_to_concierge_even_with_a_same_named_mailbox() {
        let d = testkit::TempDir::new("mail-sendmail");
        let home = d.path().join("home");
        fs::create_dir_all(home.join("chamber")).unwrap();
        fs::write(home.join("chamber/builder.md"), "# builder persona\n").unwrap();
        maildir::mail_ensure(&d.path().join("builder")).unwrap();
        let msgid = send_plain(d.path(), "uc17-orig2", "Builder <builder@spira>", "routing test");
        let bd = FakeBd::new(vec![]);
        let out = sendmail(&bd, &FakeLc::new(0), false, &home, d.path(), false, &reply_raw(Some(&msgid))).unwrap();
        assert_eq!(out.dest_mailbox, "concierge");
    }

    #[test]
    fn reply_with_no_sender_mailbox_and_no_persona_routes_to_concierge() {
        let d = testkit::TempDir::new("mail-sendmail");
        let home = d.path().join("home");
        fs::create_dir_all(home.join("chamber")).unwrap();
        let msgid = send_plain(d.path(), "uc17-orig3", "Landing gate <nobox@spira>", "routing test");
        let bd = FakeBd::new(vec![]);
        let out = sendmail(&bd, &FakeLc::new(0), false, &home, d.path(), false, &reply_raw(Some(&msgid))).unwrap();
        assert_eq!(out.dest_mailbox, "concierge");
    }

    #[test]
    fn reply_with_no_in_reply_to_routes_to_concierge() {
        let d = testkit::TempDir::new("mail-sendmail");
        let home = d.path().join("home");
        fs::create_dir_all(home.join("chamber")).unwrap();
        let bd = FakeBd::new(vec![]);
        let out = sendmail(&bd, &FakeLc::new(0), false, &home, d.path(), false, &reply_raw(None)).unwrap();
        assert_eq!(out.dest_mailbox, "concierge");
    }

    #[test]
    fn a_reply_closes_the_tracking_bead_and_marks_the_original_replied() {
        let d = testkit::TempDir::new("mail-sendmail");
        let home = d.path().join("home");
        fs::create_dir_all(home.join("chamber")).unwrap();
        maildir::mail_ensure(&d.path().join("gate")).unwrap();
        let msgid = maildir::mint_msgid();
        let orig = format!(
            "From: Gate <gate@spira>\nSubject: Enable feature?\nX-Spira-Kind: decision\nX-Spira-Bead: sp-dec1\nMessage-ID: <{msgid}@spira>\n\n## Decision\n"
        );
        fs::write(d.path().join("gate/tmp").join(&msgid), &orig).unwrap();
        maildir::mail_deliver(&d.path().join("gate"), &msgid, false).unwrap();

        let bd = FakeBd::new(vec![BdOut::ok(""), BdOut::ok("[]")]);
        let raw = format!("From: Operator <operator@spira>\nSubject: Re: Enable feature?\nIn-Reply-To: <{msgid}@spira>\n\nApprove it.\n");
        sendmail(&bd, &FakeLc::new(0), true, &home, d.path(), false, &raw).unwrap();

        let calls = bd.calls();
        assert_eq!(calls[0].0[0], "close");
        assert_eq!(calls[0].0[1], "sp-dec1");
        assert_eq!(calls[0].1.as_deref(), Some("Approve it."));

        // mark_replied renames in place — it does not move new/ -> cur/ (mail.sh's own
        // behaviour: only `done` does that move; a reply just flags where the message
        // already lives). "gate" is both the original sender's mailbox AND this reply's
        // routed destination, so gate/new/ now holds two files: the original (flagged R)
        // and the reply itself.
        let names: Vec<String> = fs::read_dir(d.path().join("gate/new"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        let name = names.into_iter().find(|n| n.ends_with(":2,R")).expect("the original, flagged replied");
        assert!(name.ends_with(":2,R"), "{name}");
    }

    fn deliver_question(root: &Path, mailbox: &str, headers: &str) -> String {
        maildir::mail_ensure(&root.join(mailbox)).unwrap();
        let msgid = maildir::mint_msgid();
        let orig = format!("From: Builder <builder@spira>\nSubject: May I?\nX-Spira-Kind: question\n{headers}Message-ID: <{msgid}@spira>\n\n## Question\nq\n");
        fs::write(root.join(mailbox).join("tmp").join(&msgid), &orig).unwrap();
        maildir::mail_deliver(&root.join(mailbox), &msgid, false).unwrap();
        msgid
    }

    /// THE GAP THIS CLOSES (sp-v62vn follow-up): `work ask` holds the asking bead, and only a
    /// Reply lifts the hold. The operator's answer to an operator ask closes the tracking
    /// bead AND emits `spira-lc reply <work-bead> <answer's message id>`.
    #[test]
    fn an_answer_to_a_work_beads_question_lifts_its_ask_hold() {
        let d = testkit::TempDir::new("mail-sendmail-lift");
        let home = d.path().join("home");
        fs::create_dir_all(home.join("chamber")).unwrap();
        let q = deliver_question(d.path(), "operator", "X-Spira-Bead: sp-dec1\nX-Spira-Work-Bead: sp-work1\n");
        let bd = FakeBd::new(vec![BdOut::ok(""), BdOut::ok("[]")]);
        let lc = FakeLc::new(0);
        let raw = format!("From: Operator <operator@spira>\nSubject: Re: May I?\nIn-Reply-To: <{q}@spira>\nMessage-ID: <ans-1@spira>\n\nYes.\n");
        sendmail(&bd, &lc, true, &home, d.path(), false, &raw).unwrap();
        assert_eq!(bd.calls()[0].0[..2], ["close".to_string(), "sp-dec1".to_string()]);
        let calls = lc.calls();
        assert_eq!(calls[0], vec!["reply", "sp-work1", "ans-1@spira", "operator"]);
        assert_eq!(calls[1][..2], ["close-ask", "sp-dec1"]);
        assert_eq!(calls.len(), 2);
        assert!(!bd.calls().iter().any(|c| c.0[0] == "note" && c.0[1] == "sp-work1"), "the tracking bead's verdict note covers it: {:?}", bd.calls());
    }

    /// A question routed to the concierge has no tracking bead: the concierge's reply must
    /// not close anything, lifts the hold, and leaves the answer on the work bead itself.
    #[test]
    fn a_reply_to_a_routed_question_notes_the_work_bead_and_closes_nothing() {
        let d = testkit::TempDir::new("mail-sendmail-routed");
        let home = d.path().join("home");
        fs::create_dir_all(home.join("chamber")).unwrap();
        let q = deliver_question(d.path(), "concierge", "X-Spira-Work-Bead: sp-work1\n");
        let bd = FakeBd::new(vec![BdOut::ok("")]);
        let lc = FakeLc::new(0);
        let raw = format!("From: Concierge <concierge@spira>\nSubject: Re: May I?\nIn-Reply-To: <{q}@spira>\n\nGo ahead.\n");
        sendmail(&bd, &lc, true, &home, d.path(), false, &raw).unwrap();
        let calls = bd.calls();
        assert!(!calls.iter().any(|c| c.0[0] == "close"), "{calls:?}");
        assert_eq!(calls[0].0[..2], ["note".to_string(), "sp-work1".to_string()]);
        assert!(calls[0].0[2].contains("Go ahead."), "{calls:?}");
        let lcc = lc.calls();
        assert_eq!(lcc.len(), 1);
        assert_eq!(lcc[0][..2], ["reply".to_string(), "sp-work1".to_string()]);
        assert_eq!(lcc[0][3], "concierge");
    }

    /// No X-Spira-Work-Bead (a note, an old question): nothing to lift.
    #[test]
    fn a_reply_to_mail_with_no_work_bead_lifts_no_hold_and_only_closes_the_ask() {
        let d = testkit::TempDir::new("mail-sendmail-nolift");
        let home = d.path().join("home");
        fs::create_dir_all(home.join("chamber")).unwrap();
        let q = deliver_question(d.path(), "operator", "X-Spira-Bead: sp-dec1\n");
        let bd = FakeBd::new(vec![BdOut::ok(""), BdOut::ok("[]")]);
        let lc = FakeLc::new(0);
        let raw = format!("From: Operator <operator@spira>\nSubject: Re: May I?\nIn-Reply-To: <{q}@spira>\n\nYes.\n");
        sendmail(&bd, &lc, true, &home, d.path(), false, &raw).unwrap();
        let calls = lc.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0][..2], ["close-ask", "sp-dec1"]);
    }

    #[test]
    fn mute_sendmail_delivers_straight_into_cur_already_seen() {
        let d = testkit::TempDir::new("mail-sendmail");
        let home = d.path().join("home");
        fs::create_dir_all(home.join("chamber")).unwrap();
        let bd = FakeBd::new(vec![]);
        let raw = "From: Someone <s@s>\nSubject: raw muted\n\nbody\n";
        let out = sendmail(&bd, &FakeLc::new(0), false, &home, d.path(), true, raw).unwrap();
        assert_eq!(out.dest_mailbox, "concierge");
        assert!(out.delivered_path.to_string_lossy().contains("/cur/"));
        assert!(out.delivered_path.to_string_lossy().ends_with(":2,S"));
    }
}
