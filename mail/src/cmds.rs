//! The command surface: `send`, `template`, `list`, `read`, `count`, `unread-age`, `done`,
//! `ensure`, `sweep-dismissed`. `sendmail` and `tidy` are big enough to own their own
//! modules (`sendmail.rs`, `tidy.rs`); everything else lives here. Every function takes its
//! inputs explicitly (no reads of `std::env` below `main.rs`) so each is a plain unit test.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::bead::{self, Bd};
use crate::env::Env;
use crate::kinds;
use crate::lint::{self, LintInput};
use crate::maildir;
use crate::message;
use crate::repeat;
use crate::rfc822;
use crate::sendmail::find_message_by_id;

/// A caller whose write end never closes gets a refusal near `budget_ms`, not a hang
/// (sop-mail-send-loom-splice-hang) — mail.sh's `timeout "$budget_s" cat`.
pub fn read_body_deadline(budget_ms: u64) -> Result<String, String> {
    let budget_ms = budget_ms.max(100);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    match rx.recv_timeout(Duration::from_millis(budget_ms)) {
        Ok(buf) => Ok(String::from_utf8_lossy(&buf).into_owned()),
        Err(_) => Err(format!(
            "stdin read exceeded {budget_ms}ms deadline (SPIRA_LOOM_BUDGET_MS) — refusing to hang; ensure the body is piped and stdin is closed"
        )),
    }
}

pub struct SendArgs<'a> {
    pub mailbox: &'a str,
    pub from: Option<&'a str>,
    pub subject: &'a str,
    pub kind: &'a str,
    pub default: &'a str,
    pub bead: &'a str,
    pub urgent: bool,
    pub digest: bool,
}

pub struct SendOutcome {
    pub delivered_path: PathBuf,
    pub x_bead: Option<String>,
}

pub fn send(bd: &dyn Bd, env: &Env, args: &SendArgs, body: String) -> Result<SendOutcome, String> {
    let mut mailbox = args.mailbox.to_string();
    maildir::mailbox_valid(&mailbox)?;

    if let Some(bid) = mailbox.strip_prefix("aeon:") {
        let bid = bid.to_string();
        mailbox = format!("aeon-{bid}");
        if !env.mail_root.join(&mailbox).join("new").is_dir() {
            return Err(format!("aeon:{bid}: no live mailbox — bead is not claimed by a live aeon"));
        }
    }

    let from = args.from.map(str::to_string).or_else(|| env.mail_from.clone()).unwrap_or_default();

    let is_operator = mailbox == "operator";
    let mut repeat_guard = if is_operator {
        Some(repeat::repeat_check(&env.run_dir, &mailbox, args.subject, env.repeat_window_s, env.lock_timeout_ms, env.repeat_considered.as_deref())?)
    } else {
        None
    };

    if env.lint_considered.is_none() {
        let input = LintInput { from: &from, subject: args.subject, kind: args.kind, default: args.default, urgent: args.urgent, body: &body, digest: args.digest };
        if let Err(e) = lint::lint_check(&input, &env.kinds_dir, env.id_prefix_for_regex()) {
            if is_operator {
                repeat::repeat_release(repeat_guard.take().unwrap());
            }
            return Err(e);
        }
    }

    let db_configured = !env.db.is_empty();
    let mut x_bead = args.bead.to_string();

    if (args.kind == "question" || args.kind == "decision") && db_configured {
        if let Some(dec_bead) = bead::create_tracking_bead(bd, true, args.subject, &body, &env.ask_label) {
            if !args.bead.is_empty() {
                if env.allow_blocking {
                    if bead::dep_add(bd, args.bead, &dec_bead, "blocks") {
                        eprintln!("mail: blocking edge wired — {} blocks on {}", args.bead, dec_bead);
                    }
                } else {
                    eprintln!("mail: blocking edge refused — wiring relates_to instead (override: SPIRA_MAIL_ALLOW_BLOCKING=1)");
                    bead::dep_add(bd, args.bead, &dec_bead, "relates-to");
                }
            }
            x_bead = dec_bead;
        }
    }

    let render_id = if !args.bead.is_empty() {
        args.bead.to_string()
    } else {
        let haystack = format!("{}\n{}\n", args.subject, body);
        lint::bead_id_regex(env.id_prefix_for_regex()).find(&haystack).map(|m| m.as_str().to_string()).unwrap_or_default()
    };
    let final_body = if render_id.is_empty() {
        body
    } else {
        format!("{}\n\n{}", bead::render_bead_block(bd, db_configured, &render_id), body)
    };

    let dir = maildir::mail_dir(env, &mailbox);
    maildir::mail_ensure(&dir).map_err(|e| format!("cannot create mailbox {mailbox}: {e}"))?;
    let msgid = maildir::mint_msgid();

    let mut msg = String::new();
    msg.push_str(&format!("From: {from}\n"));
    msg.push_str(&format!("Subject: {}\n", args.subject));
    if !args.kind.is_empty() {
        msg.push_str(&format!("X-Spira-Kind: {}\n", args.kind));
    }
    if !args.default.is_empty() {
        msg.push_str(&format!("X-Spira-Default: {}\n", args.default));
    }
    if args.urgent {
        msg.push_str("X-Spira-Urgent: yes\n");
    }
    if args.digest {
        msg.push_str("X-Spira-Digest: yes\n");
    }
    if !x_bead.is_empty() {
        msg.push_str(&format!("X-Spira-Bead: {x_bead}\n"));
    }
    if let Some(lc) = &env.lint_considered {
        msg.push_str(&format!("X-Spira-Lint-Override: {lc}\n"));
    }
    if let Some(rco) = &env.repeat_considered {
        msg.push_str(&format!("X-Spira-Repeat-Override: {rco}\n"));
    }
    msg.push_str(&format!("Date: {}\n", rfc822::now()));
    msg.push_str(&format!("Message-ID: <{msgid}@spira>\n"));
    msg.push('\n');
    msg.push_str(&final_body);
    msg.push('\n');

    fs::write(dir.join("tmp").join(&msgid), &msg).map_err(|e| format!("cannot write message: {e}"))?;
    let delivered_path = maildir::mail_deliver(&dir, &msgid, env.mute).map_err(|e| format!("cannot deliver message: {e}"))?;

    if is_operator {
        repeat::repeat_stamp(repeat_guard.take().unwrap());

        if (args.kind == "question" || args.kind == "decision") && env.bead_id.is_some() {
            let marker = env.run_dir.join(format!("{}.operator-wait", env.bead_id.as_ref().unwrap()));
            let _ = fs::write(marker, env.session_epoch.clone().unwrap_or_default());
        }
        if (args.kind == "question" || args.kind == "decision") && !x_bead.is_empty() {
            let _ = index_record(&env.index_file, &mailbox, &format!("{msgid}@spira"), &x_bead, args.kind, args.default);
        }
    }

    Ok(SendOutcome { delivered_path, x_bead: if x_bead.is_empty() { None } else { Some(x_bead) } })
}

fn index_record(index_file: &Path, mailbox: &str, msgid: &str, bead: &str, kind: &str, default: &str) -> std::io::Result<()> {
    if let Some(parent) = index_file.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(index_file)?;
    writeln!(f, "{}\t{}\t{}\t{}\t{}", mailbox, msgid, bead, kind, default.replace('\t', " "))
}

pub fn template(kinds_dir: &Path, kind: &str) -> Result<String, String> {
    if kind.is_empty() {
        return Err("kind required".to_string());
    }
    kinds::load(kinds_dir, kind).map(|k| k.template).ok_or_else(|| format!("unknown kind: {kind}"))
}

/// Shared by `list`/`read`/`count`/`unread-age`/`done`: a read verb must never create the
/// mailbox it reads (mail.sh's own `_mailbox_exists`).
fn require_mailbox(env: &Env, mailbox: &str) -> Result<PathBuf, String> {
    maildir::mailbox_valid(mailbox)?;
    let dir = maildir::mail_dir(env, mailbox);
    if maildir::mailbox_exists(&dir) {
        Ok(dir)
    } else {
        Err(format!("{mailbox}: no such mailbox"))
    }
}

pub fn list(env: &Env, mailbox: &str, unread_only: bool) -> Result<String, String> {
    let dir = require_mailbox(env, mailbox)?;
    let mut subs = vec!["new"];
    if !unread_only {
        subs.push("cur");
    }
    let mut lines = Vec::new();
    for sub in subs {
        for f in maildir::sorted_entries(&dir.join(sub)) {
            let text = fs::read_to_string(&f).unwrap_or_default();
            let from = non_empty_or_dash(message::header_line_sed(&text, "From"));
            let subject = non_empty_or_dash(message::header_line_sed(&text, "Subject"));
            let date = non_empty_or_dash(message::header_line_sed(&text, "Date"));
            lines.push(format!("{date}  [{sub}]  From: {from}  Subject: {subject}"));
        }
    }
    Ok(lines.join("\n"))
}

fn non_empty_or_dash(s: String) -> String {
    if s.is_empty() {
        "-".to_string()
    } else {
        s
    }
}

pub fn read_cmd(env: &Env, mailbox: &str, msg: Option<&str>) -> Result<String, String> {
    let dir = require_mailbox(env, mailbox)?;

    let path = if let Some(msg) = msg {
        let candidates = [dir.join("new").join(msg), dir.join("cur").join(msg)];
        candidates.into_iter().find(|c| c.is_file()).ok_or_else(|| format!("{msg}: not found"))?
    } else {
        let mut oldest: Option<(u64, PathBuf)> = None;
        for c in maildir::sorted_entries(&dir.join("new")) {
            let mt = maildir::mtime_secs(&c);
            if oldest.as_ref().map(|(m, _)| mt < *m).unwrap_or(true) {
                oldest = Some((mt, c));
            }
        }
        oldest.map(|(_, p)| p).ok_or_else(|| format!("no unread mail in {mailbox}"))?
    };

    let content = fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if path.starts_with(dir.join("new")) {
        if let Some(name) = path.file_name() {
            let _ = fs::rename(&path, dir.join("cur").join(name));
        }
    }
    Ok(content)
}

pub fn count(env: &Env, mailbox: &str) -> Result<usize, String> {
    let dir = require_mailbox(env, mailbox)?;
    Ok(maildir::sorted_entries(&dir.join("new")).len())
}

pub fn unread_age(env: &Env, mailbox: &str) -> Result<Option<u64>, String> {
    let dir = require_mailbox(env, mailbox)?;
    let mut oldest: Option<u64> = None;
    for c in maildir::sorted_entries(&dir.join("new")) {
        let mt = maildir::mtime_secs(&c);
        oldest = Some(oldest.map_or(mt, |o| o.min(mt)));
    }
    Ok(oldest.map(|o| {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        now.saturating_sub(o)
    }))
}

pub fn done(env: &Env, mailbox: &str, msgid: &str, note: Option<&str>) -> Result<(), String> {
    if msgid.is_empty() {
        return Err("message id required".to_string());
    }
    let dir = require_mailbox(env, mailbox)?;

    let mut found = [dir.join("new").join(msgid), dir.join("cur").join(msgid)].into_iter().find(|c| c.is_file());
    if found.is_none() {
        let prefix = format!("{msgid}:");
        found = maildir::sorted_entries(&dir.join("cur"))
            .into_iter()
            .find(|p| p.file_name().map(|f| f.to_string_lossy().starts_with(&prefix)).unwrap_or(false));
    }
    let mut path = found.ok_or_else(|| format!("{msgid}: not found in {mailbox}"))?;

    if path.starts_with(dir.join("new")) {
        if let Some(name) = path.file_name() {
            let dest = dir.join("cur").join(name);
            fs::rename(&path, &dest).map_err(|e| format!("cannot move to cur: {e}"))?;
            path = dest;
        }
    }

    if let Some(base) = path.file_name().and_then(|f| f.to_str()) {
        if let Some(new_name) = maildir::add_flag(base, 'R') {
            let dest = path.parent().unwrap().join(new_name);
            fs::rename(&path, &dest).map_err(|e| format!("cannot set R flag: {e}"))?;
            path = dest;
        }
    }

    if let Some(note) = note {
        if !note.is_empty() {
            let mut f = OpenOptions::new().append(true).open(&path).map_err(|e| format!("cannot append note: {e}"))?;
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
            writeln!(f, "\n-- done: {}\n{}", rfc822::format_done(now), note).map_err(|e| format!("cannot append note: {e}"))?;
        }
    }
    Ok(())
}

pub fn ensure(env: &Env, mailbox: &str) -> Result<(), String> {
    maildir::mailbox_valid(mailbox)?;
    maildir::mail_ensure(&maildir::mail_dir(env, mailbox)).map_err(|e| e.to_string())
}

#[derive(Debug, Default)]
pub struct SweepReport {
    pub dismissed: usize,
    pub kept: usize,
}

/// `[]`/empty result: nothing to dismiss (not an error). `None`: the index doesn't exist
/// yet, also not an error.
pub enum SweepOutcome {
    NothingToDismiss,
    Report(SweepReport),
}

pub fn sweep_dismissed(bd: &dyn Bd, db_configured: bool, mail_root: &Path, index_file: &Path, mailbox: &str, operator_actor: &str) -> Result<SweepOutcome, String> {
    if !db_configured {
        return Err("bead store not configured — refusing to dismiss anything".to_string());
    }
    if !index_file.is_file() {
        return Ok(SweepOutcome::NothingToDismiss);
    }
    let text = fs::read_to_string(index_file).map_err(|_| "index unreadable — refusing to dismiss anything".to_string())?;

    let mut report = SweepReport::default();
    for line in text.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 5 {
            continue;
        }
        let (idx_mailbox, msgid, bead_id, _kind, default) = (fields[0], fields[1], fields[2], fields[3], fields[4]);
        if msgid.is_empty() || idx_mailbox != mailbox {
            continue;
        }

        let status = match bead::bead_status(bd, true, bead_id) {
            Some(s) => s,
            None => continue,
        };
        if status == "closed" {
            continue;
        }
        if find_message_by_id(mail_root, msgid).is_some() {
            report.kept += 1;
            continue;
        }
        if bead::bead_replied(bd, true, bead_id, operator_actor) {
            report.kept += 1;
            continue;
        }
        let reason = format!("dismissed by operator (mail deleted) — default taken: {}", if default.is_empty() { "<none given>" } else { default });
        if bead::close(bd, bead_id, &reason).is_ok() {
            report.dismissed += 1;
        } else {
            eprintln!("sweep-dismissed: failed to close {bead_id}");
        }
    }
    Ok(SweepOutcome::Report(report))
}
