//! `sendmail`: RFC 5322 mail on stdin. Routes the reply to the sender's mailbox (or
//! `concierge` for a chamber persona, or when there is nobody to route to) and delivers the
//! raw message unchanged. Its bead side effects belong to `reconcile`.

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

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

/// Runs the `sendmail` pipeline against raw RFC 5322 text already read from stdin: routes the
/// reply and delivers it. Nothing here touches the bead store; `reconcile::pass` applies what
/// a delivered reply means to the beads it answers.
pub fn sendmail(home: &Path, mail_root: &Path, mute: bool, raw: &str) -> Result<SendmailOutcome, String> {
    let in_reply_to = strip_angle_brackets(&message::header_ci_before_blank(raw, "in-reply-to"));
    let mut dest_mailbox = "concierge".to_string();
    let mut orig_file: Option<PathBuf> = None;

    if !in_reply_to.is_empty() {
        if let Some(path) = find_message_by_id(mail_root, &in_reply_to) {
            let text = fs::read_to_string(&path).unwrap_or_default();
            let orig_from = message::header_ci_before_blank(&text, "from");
            dest_mailbox = reply_mailbox(&orig_from, home, mail_root);
            orig_file = Some(path);
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

    Ok(SendmailOutcome { dest_mailbox, delivered_path })
}

/// Replying to a message is having seen it: the original moves out of `new/` into `cur/`
/// carrying `S` and `R` in one rename. Flagging it in place left it in `new/` unread, and a
/// client's own seen-rename of the old name then failed against it.
pub fn mark_replied(path: &Path) {
    if !path.is_file() {
        return;
    }
    let Some(base) = path.file_name().and_then(|f| f.to_str()) else { return };
    let Some(mbox) = path.parent().and_then(|sub| sub.parent()) else { return };
    let named = maildir::add_flag(base, 'R').unwrap_or_else(|| base.to_string());
    let name = maildir::add_flag(&named, 'S').unwrap_or(named);
    let _ = fs::rename(path, mbox.join("cur").join(name));
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn reply_routes_to_the_senders_mailbox_when_one_exists() {
        let d = testkit::TempDir::new("mail-sendmail");
        let home = d.path().join("home");
        fs::create_dir_all(home.join("chamber")).unwrap();
        maildir::mail_ensure(&d.path().join("gate")).unwrap();
        let msgid = send_plain(d.path(), "uc17-orig1", "Gate <gate@spira>", "routing test");
        let out = sendmail(&home, d.path(), false, &reply_raw(Some(&msgid))).unwrap();
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
        let out = sendmail(&home, d.path(), false, &reply_raw(Some(&msgid))).unwrap();
        assert_eq!(out.dest_mailbox, "concierge");
    }

    #[test]
    fn reply_with_no_sender_mailbox_and_no_persona_routes_to_concierge() {
        let d = testkit::TempDir::new("mail-sendmail");
        let home = d.path().join("home");
        fs::create_dir_all(home.join("chamber")).unwrap();
        let msgid = send_plain(d.path(), "uc17-orig3", "Landing gate <nobox@spira>", "routing test");
        let out = sendmail(&home, d.path(), false, &reply_raw(Some(&msgid))).unwrap();
        assert_eq!(out.dest_mailbox, "concierge");
    }

    #[test]
    fn reply_with_no_in_reply_to_routes_to_concierge() {
        let d = testkit::TempDir::new("mail-sendmail");
        let home = d.path().join("home");
        fs::create_dir_all(home.join("chamber")).unwrap();
        let out = sendmail(&home, d.path(), false, &reply_raw(None)).unwrap();
        assert_eq!(out.dest_mailbox, "concierge");
    }

    #[test]
    fn a_reply_marks_the_original_seen_and_replied_in_cur_and_touches_no_bead() {
        let d = testkit::TempDir::new("mail-sendmail");
        let home = d.path().join("home");
        fs::create_dir_all(home.join("chamber")).unwrap();
        let msgid = send_plain(d.path(), "gate", "Gate <gate@spira>", "Enable feature?");
        let raw = format!("From: Operator <operator@spira>\nSubject: Re: Enable feature?\nIn-Reply-To: <{msgid}@spira>\n\nApprove it.\n");
        sendmail(&home, d.path(), false, &raw).unwrap();
        let names = |sub: &str| -> Vec<String> { fs::read_dir(d.path().join("gate").join(sub)).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect() };
        assert_eq!(names("new").len(), 1, "the original left new/: {:?}", names("new"));
        assert!(names("new").iter().all(|n| !n.contains(":2,")), "the delivered reply stays unflagged in new/");
        assert_eq!(names("cur"), vec![format!("{msgid}:2,RS")]);
    }

    #[test]
    fn a_reply_to_an_already_seen_original_keeps_it_in_cur_with_s_and_gains_r() {
        let d = testkit::TempDir::new("mail-sendmail");
        let home = d.path().join("home");
        fs::create_dir_all(home.join("chamber")).unwrap();
        let msgid = send_plain(d.path(), "gate", "Gate <gate@spira>", "Enable feature?");
        let gate = d.path().join("gate");
        fs::rename(gate.join("new").join(&msgid), gate.join("cur").join(format!("{msgid}:2,S"))).unwrap();
        let raw = format!("From: Operator <operator@spira>\nSubject: Re: x\nIn-Reply-To: <{msgid}@spira>\n\nOk.\n");
        sendmail(&home, d.path(), false, &raw).unwrap();
        assert!(gate.join("cur").join(format!("{msgid}:2,RS")).is_file());
        assert_eq!(fs::read_dir(gate.join("new")).unwrap().count(), 1);
    }

    #[test]
    fn mute_sendmail_delivers_straight_into_cur_already_seen() {
        let d = testkit::TempDir::new("mail-sendmail");
        let home = d.path().join("home");
        fs::create_dir_all(home.join("chamber")).unwrap();
        let raw = "From: Someone <s@s>\nSubject: raw muted\n\nbody\n";
        let out = sendmail(&home, d.path(), true, raw).unwrap();
        assert_eq!(out.dest_mailbox, "concierge");
        assert!(out.delivered_path.to_string_lossy().contains("/cur/"));
        assert!(out.delivered_path.to_string_lossy().ends_with(":2,S"));
    }
}
