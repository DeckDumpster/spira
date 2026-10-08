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
use crate::lc::{self, Lc, Lift};
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
    pub class: &'a str,
    pub bead: &'a str,
    pub urgent: bool,
    pub digest: bool,
    pub dry_run: bool,
}

#[derive(Debug)]
pub struct SendOutcome {
    pub delivered_path: PathBuf,
    pub x_bead: Option<String>,
    pub steps: Vec<String>,
}

pub const ESCALATION_CLASSES: [&str; 3] = ["permissions", "policy", "destructive"];

/// Why an aeon's decision ask for the operator does not qualify, or None if it does.
/// A class is supported only when declared, one of `ESCALATION_CLASSES`, and argued in a
/// non-empty `## Class basis` section (law-escalate-decisions-not-problems).
pub fn class_refusal(class: &str, default: &str, body: &str) -> Option<String> {
    if default.trim().is_empty() {
        return Some("no --default stated".to_string());
    }
    if class.is_empty() {
        return Some("no --class declared".to_string());
    }
    if !ESCALATION_CLASSES.contains(&class) {
        return Some(format!("class '{class}' is not an escalation class"));
    }
    if kinds::section_empty("Class basis", body) {
        return Some(format!("class '{class}' declared but the body has no '## Class basis' section supporting it"));
    }
    None
}

/// Why a mail for the operator does not belong in the operator mailbox, or None if it does.
pub fn operator_refusal(kind: &str, default: &str, class: &str, body: &str) -> Option<String> {
    if kind != "question" && kind != "decision" {
        return Some(format!("kind '{kind}' is not a decision ask"));
    }
    if default.trim().is_empty() {
        return Some("no --default carried".to_string());
    }
    class_refusal(class, default, body)
}

const PROBE_MARKERS: [&str; 4] = ["probe", "deleteme", "plumbing test", "test ping"];

/// The probe marker an operator-bound message carries in its subject or first body line.
pub fn probe_marker(subject: &str, body: &str) -> Option<&'static str> {
    let first = body.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let hay = format!("{}\n{}", subject.to_lowercase(), first.to_lowercase());
    PROBE_MARKERS.iter().copied().find(|m| hay.contains(m))
}

pub fn send(bd: &dyn Bd, env: &Env, args: &SendArgs, body: String) -> Result<SendOutcome, String> {
    let mut mailbox = args.mailbox.to_string();
    maildir::mailbox_valid(&mailbox)?;
    let mut steps = vec![format!("mailbox {mailbox}: valid")];

    if mailbox == "operator" && !args.dry_run {
        if let Some(m) = probe_marker(args.subject, &body) {
            return Err(format!(
                "refusing a probe to the operator ('{m}' in the subject or opening line) — a real ask in the queue pages a human. To exercise the plumbing, add --dry-run (work ask --dry-run): every step runs and nothing is filed or delivered."
            ));
        }
    }

    let mut body = body;
    let mut rerouted = false;
    if mailbox == "operator" && !args.digest && env.operator_considered.is_none() {
        if let Some(why) = operator_refusal(args.kind, args.default, args.class, &body) {
            eprintln!(
                "mail: routed to the concierge, not the operator — {why}. The operator mailbox admits only question asks with a class (permissions, policy, destructive-on-production-data), a default and a '## Class basis' section; everything else is the concierge's judgement, and carries no ask label."
            );
            body = format!("(Routed here from an operator mail: {why}.)\n\n{body}");
            mailbox = "concierge".to_string();
            rerouted = true;
        }
    }
    steps.push(match mailbox.as_str() {
        "concierge" if rerouted => "route: concierge (rerouted: ask does not qualify for the operator)".to_string(),
        m => format!("route: {m}"),
    });

    if let Some(bid) = mailbox.strip_prefix("aeon:") {
        let bid = bid.to_string();
        mailbox = format!("aeon-{bid}");
        if !env.mail_root.join(&mailbox).join("new").is_dir() {
            return Err(format!("aeon:{bid}: no live mailbox — bead is not claimed by a live aeon"));
        }
    }

    let from = args.from.map(str::to_string).or_else(|| env.mail_from.clone()).unwrap_or_default();

    let is_operator = mailbox == "operator";
    if is_operator && !args.dry_run && !env.db.is_empty() && !env.ask_label.is_empty() {
        match bead::open_duplicate_ask(bd, &env.ask_label, args.subject, args.bead) {
            Ok(Some(open)) => return Err(format!("duplicate ask refused — {open} is already open for it; answer or amend that one")),
            Ok(None) => {}
            Err(e) => eprintln!("mail: duplicate check skipped: {e}"),
        }
    }
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

    steps.push(if env.lint_considered.is_none() { "lint: passed".to_string() } else { "lint: overridden".to_string() });

    let db_configured = !env.db.is_empty();
    let asks = args.kind == "question" || args.kind == "decision";
    // The bead a question is ABOUT (the asking session's own, under `work ask`), carried as
    // X-Spira-Work-Bead so whoever answers can lift its `ask` hold (sp-v62vn follow-up).
    let work_bead = if asks { args.bead } else { "" };
    // A rerouted question files no tracking bead, so X-Spira-Bead would name the WORK bead —
    // and a reply's sendmail closes whatever X-Spira-Bead names. The work bead rides
    // X-Spira-Work-Bead alone.
    let mut x_bead = if rerouted && asks { String::new() } else { args.bead.to_string() };

    if args.dry_run {
        if asks && db_configured && !rerouted && mailbox != "concierge" {
            if env.ask_label.is_empty() {
                return Err("ask label does not resolve — refusing to file an ask under a guessed one".to_string());
            }
            steps.push(format!("tracking bead: would be filed under label {}", env.ask_label));
        }
        steps.push(format!("delivery: would write into {}", maildir::mail_dir(env, &mailbox).display()));
        steps.push("dry run: no bead filed, no mail delivered".to_string());
        if is_operator {
            repeat::repeat_release(repeat_guard.take().unwrap());
        }
        return Ok(SendOutcome { delivered_path: PathBuf::new(), x_bead: None, steps });
    }

    if (args.kind == "question" || args.kind == "decision") && db_configured && !rerouted && mailbox != "concierge" {
        if env.ask_label.is_empty() {
            if is_operator {
                repeat::repeat_release(repeat_guard.take().unwrap());
            }
            return Err("ask label does not resolve — refusing to file an ask under a guessed one".to_string());
        }
        if let Some(dec_bead) = bead::create_tracking_bead(bd, true, args.subject, &body, &env.ask_label, work_bead) {
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
            if !args.default.is_empty() {
                if let Err(e) = bead::note(bd, &dec_bead, &format!("Default: {}", args.default)) {
                    eprintln!("mail: could not record the default on {dec_bead}: {e}");
                }
            }
            x_bead = dec_bead;
        }
    }

    let render_id = args.bead.to_string();
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
    if !work_bead.is_empty() {
        msg.push_str(&format!("X-Spira-Work-Bead: {work_bead}\n"));
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

        if (args.kind == "question" || args.kind == "decision") && !x_bead.is_empty() {
            let _ = index_record(&env.index_file, &mailbox, &format!("{msgid}@spira"), &x_bead, args.kind, args.default, work_bead);
        }
    }

    Ok(SendOutcome { delivered_path, x_bead: if x_bead.is_empty() { None } else { Some(x_bead) }, steps })
}

/// One TSV line: mailbox, message id, tracking bead, kind, default, and — sixth, possibly
/// empty — the work bead the question is about (read by `sweep_dismissed` to lift its hold;
/// a five-field line from before the column existed still parses).
fn index_record(index_file: &Path, mailbox: &str, msgid: &str, bead: &str, kind: &str, default: &str, work_bead: &str) -> std::io::Result<()> {
    if let Some(parent) = index_file.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(index_file)?;
    writeln!(f, "{}\t{}\t{}\t{}\t{}\t{}", mailbox, msgid, bead, kind, default.replace('\t', " "), work_bead)
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

/// A dismissal is the operator's answer — "the default" — so a dismissed question about a
/// work bead (the index's sixth field) also lifts that bead's `ask` hold, with the dismissed
/// question's own message id as the reply (sp-v62vn follow-up).
pub fn sweep_dismissed(bd: &dyn Bd, lc: &dyn Lc, db_configured: bool, mail_root: &Path, index_file: &Path, mailbox: &str, operator_actor: &str) -> Result<SweepOutcome, String> {
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
        // The index's beads are asks: bd's status is their state (spira_config::nonwork, sp-mve9i).
        if spira_config::nonwork::is_closed(spira_config::nonwork::Kind::Ask, &status) {
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
            if let Some(w) = lc::lift_ask(lc, fields.get(5).copied().unwrap_or(""), Lift::Reply { message_id: msgid }, operator_actor) {
                eprintln!("{w}");
            }
        } else {
            eprintln!("sweep-dismissed: failed to close {bead_id}");
        }
    }
    Ok(SweepOutcome::Report(report))
}

#[cfg(test)]
mod class_tests {
    use super::class_refusal;

    const BASIS: &str = "## Question\nq\n\n## Class basis\nneeds a credential I lack\n";

    #[test]
    fn a_declared_supported_class_qualifies() {
        for c in ["permissions", "policy", "destructive"] {
            assert_eq!(class_refusal(c, "take the safe path", BASIS), None);
        }
    }

    #[test]
    fn no_class_an_architecture_class_or_no_basis_is_refused() {
        assert!(class_refusal("", "d", BASIS).is_some());
        assert!(class_refusal("architecture", "d", BASIS).is_some());
        assert!(class_refusal("policy", "d", "## Question\nq\n").is_some());
        assert!(class_refusal("policy", "d", "## Class basis\n\n").is_some());
    }

    #[test]
    fn a_missing_default_is_refused_whatever_the_class() {
        assert!(class_refusal("policy", "  ", BASIS).is_some());
    }
}

#[cfg(test)]
mod sweep_tests {
    use super::*;
    use crate::lc::fake::FakeLc;
    use crate::bead::fake::FakeBd;
    use crate::bead::BdOut;

    /// An ask already closed in bd is skipped outright: no reply probe, no close (sp-mve9i:
    /// the ask is a non-work bead, so its bd status is the state read here).
    #[test]
    fn a_closed_ask_is_neither_kept_nor_dismissed() {
        let t = testkit::TempDir::new("mail-sweep-closed");
        let idx = t.path().join("index");
        std::fs::write(&idx, "ops\t<m1@x>\tsp-ask1\task\tgo\n").unwrap();
        let bd = FakeBd::new(vec![BdOut::ok(r#"[{"id":"sp-ask1","status":"closed"}]"#)]);
        let Ok(SweepOutcome::Report(r)) = sweep_dismissed(&bd, &FakeLc::new(0), true, t.path(), &idx, "ops", "ryan") else { panic!("no report") };
        assert_eq!((r.dismissed, r.kept), (0, 0));
        assert_eq!(bd.calls().len(), 1, "{:?}", bd.calls());
    }

    /// A dismissed question about a work bead takes its default AND lifts the work bead's
    /// ask hold; a five-field line from before the work-bead column lifts nothing.
    #[test]
    fn a_dismissed_question_lifts_its_work_beads_ask_hold() {
        let t = testkit::TempDir::new("mail-sweep-lift");
        let idx = t.path().join("index");
        std::fs::write(&idx, "ops\tm1@spira\tsp-ask1\tquestion\tgo\tsp-work1\nops\tm2@spira\tsp-ask2\tquestion\tgo\n").unwrap();
        let open = || BdOut::ok(r#"[{"status":"open"}]"#);
        // per line: status, history (no operator reply), close
        let bd = FakeBd::new(vec![open(), BdOut::ok("[]"), BdOut::ok(""), open(), BdOut::ok("[]"), BdOut::ok("")]);
        let lc = FakeLc::new(0);
        let Ok(SweepOutcome::Report(r)) = sweep_dismissed(&bd, &lc, true, t.path(), &idx, "ops", "ryan") else { panic!("no report") };
        assert_eq!(r.dismissed, 2, "{:?}", bd.calls());
        assert_eq!(lc.calls(), vec![vec!["reply", "sp-work1", "m1@spira", "ryan"]]);
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    use crate::bead::fake::FakeBd;
    use crate::bead::BdOut;

    fn env(root: &Path) -> Env {
        Env {
            mail_root: root.join("mail"),
            kinds_dir: root.join("kinds"),
            index_file: root.join("index"),
            mute: false,
            loom_budget_ms: 1000,
            repeat_window_s: 0,
            tidy_fresh_s: 0,
            id_prefix: "sp".into(),
            ask_label: "needs-x".into(),
            db: String::new(),
            bd_bin: String::new(),
            bd_conn_retries: 0,
            operator_actor: "op".into(),
            run_dir: root.join("run"),
            home: root.to_path_buf(),
            mail_from: None,
            lint_considered: Some("test".into()),
            repeat_considered: Some("test".into()),
            allow_blocking: false,
            bead_id: None,
            lock_timeout_ms: 1000,
        }
    }

    fn args<'a>(subject: &'a str, dry_run: bool) -> SendArgs<'a> {
        SendArgs { mailbox: "operator", from: Some("A <a@spira>"), subject, kind: "question", default: "do it", class: "policy", bead: "", urgent: false, digest: false, dry_run }
    }

    fn mailbox_entries(root: &Path) -> usize {
        let new = root.join("mail/operator/new");
        std::fs::read_dir(new).map(|d| d.count()).unwrap_or(0)
    }

    #[test]
    fn a_probe_to_the_operator_is_refused_naming_the_dry_run_exit() {
        let t = testkit::TempDir::new("mail-probe-refused");
        let e = env(t.path());
        for subject in ["plumbing probe 2", "TEST-PROBE4-DELETEME", "probe-direct-mail-2"] {
            let Err(msg) = send(&FakeBd::new(vec![]), &e, &args(subject, false), "body".into()) else { panic!("{subject} delivered") };
            assert!(msg.contains("--dry-run"), "{msg}");
        }
        assert_eq!(mailbox_entries(t.path()), 0);
    }

    #[test]
    fn a_probe_marker_in_the_opening_line_is_refused_too() {
        let t = testkit::TempDir::new("mail-probe-body");
        let r = send(&FakeBd::new(vec![]), &env(t.path()), &args("hello", false), "\nthis is a probe\n".into());
        assert!(r.is_err());
    }

    #[test]
    fn a_dry_run_reports_each_step_and_delivers_nothing() {
        let t = testkit::TempDir::new("mail-probe-dry");
        let bd = FakeBd::new(vec![]);
        let out = send(&bd, &env(t.path()), &args("probe: is the ask channel alive", true), "## Class basis\nit is policy\n".into()).unwrap();
        let all = out.steps.join("\n");
        for want in ["mailbox operator: valid", "route: operator", "lint:", "delivery: would write", "no bead filed"] {
            assert!(all.contains(want), "{want} missing from {all}");
        }
        assert!(bd.calls().is_empty(), "{:?}", bd.calls());
        assert_eq!(mailbox_entries(t.path()), 0);
        assert!(!t.path().join("mail/operator").exists(), "a dry run must not even create the mailbox");
        assert!(out.x_bead.is_none());
    }

    fn root_mailbox_entries(root: &Path, mb: &str) -> usize {
        std::fs::read_dir(root.join("mail").join(mb).join("new")).map(|d| d.count()).unwrap_or(0)
    }

    #[test]
    fn unclassed_or_non_question_operator_mail_is_rerouted_to_the_concierge() {
        let basis = "## Class basis\nit is policy\n";
        let cases = [("fyi", "d", "policy", basis), ("alert", "d", "policy", basis), ("question", "d", "", basis), ("question", "", "policy", basis), ("question", "d", "policy", "no basis")];
        for (n, (kind, default, class, body)) in cases.iter().enumerate() {
            let t = testkit::TempDir::new(&format!("mail-reroute-{n}"));
            let a = SendArgs { kind, default, class, ..args("Something happened", false) };
            send(&FakeBd::new(vec![]), &env(t.path()), &a, (*body).into()).unwrap();
            assert_eq!(root_mailbox_entries(t.path(), "operator"), 0, "{kind}/{default}/{class}");
            assert_eq!(root_mailbox_entries(t.path(), "concierge"), 1, "{kind}/{default}/{class}");
        }
    }

    #[test]
    fn a_classed_question_with_a_default_still_reaches_the_operator() {
        let t = testkit::TempDir::new("mail-classed");
        send(&FakeBd::new(vec![]), &env(t.path()), &args("Rotate the key?", false), "## Class basis\nit is policy\n".into()).unwrap();
        assert_eq!(root_mailbox_entries(t.path(), "operator"), 1);
        assert_eq!(root_mailbox_entries(t.path(), "concierge"), 0);
    }

    #[test]
    fn a_duplicate_ask_is_refused_naming_the_open_one() {
        let t = testkit::TempDir::new("mail-dup");
        let mut e = env(t.path());
        e.db = "/db".into();
        let open = r#"[{"id":"sp-open1","title":"Rotate the key 12?","labels":["needs-x","work-bead:sp-w1"]}]"#;
        let body = "## Class basis\nit is policy\n";
        let same_subject = send(&FakeBd::new(vec![BdOut::ok(open)]), &e, &args("Rotate the key 99?", false), body.into());
        assert!(same_subject.unwrap_err().contains("sp-open1"));
        let same_bead = SendArgs { bead: "sp-w1", ..args("Entirely different words", false) };
        assert!(send(&FakeBd::new(vec![BdOut::ok(open)]), &e, &same_bead, body.into()).unwrap_err().contains("sp-open1"));
        assert_eq!(root_mailbox_entries(t.path(), "operator"), 0);
        let other = FakeBd::new(vec![BdOut::ok(open), BdOut::ok("sp-new1"), BdOut::ok("")]);
        assert!(send(&other, &e, &args("Wholly unrelated?", false), body.into()).is_ok());
    }

    #[test]
    fn a_real_ask_still_goes_through() {
        let t = testkit::TempDir::new("mail-probe-real");
        let out = send(&FakeBd::new(vec![]), &env(t.path()), &args("Close sp-abcd1?", false), "## Class basis\nit is policy\n".into()).unwrap();
        assert!(out.delivered_path.exists());
        assert_eq!(mailbox_entries(t.path()), 1);
    }

    fn send_ask(root: &Path, bd: &FakeBd, default: &str, class: &str, body: &str) -> SendOutcome {
        for d in ["mail/operator/new", "mail/concierge/new", "run"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let args = SendArgs { mailbox: "operator", from: None, subject: "Mute it?", kind: "question", default, class, bead: "", urgent: false, digest: false, dry_run: false };
        let mut e = env(root);
        e.db = "db".into();
        e.bd_bin = "bd".into();
        send(bd, &e, &args, body.to_string()).unwrap()
    }

    #[test]
    fn a_machine_alarm_with_no_class_carries_no_ask_label_and_reaches_the_concierge() {
        let t = testkit::TempDir::new("mail-ask-alarm");
        let bd = FakeBd::new(vec![]);
        let out = send_ask(t.path(), &bd, "mute it", "", "## Question\nrecurred 5 times\n");
        assert!(bd.calls().is_empty(), "no tracking bead, so no ask label: {:?}", bd.calls());
        assert!(out.delivered_path.to_string_lossy().contains("/concierge/"), "{:?}", out.delivered_path);
    }

    #[test]
    fn a_proper_decision_ask_still_carries_the_ask_label() {
        let t = testkit::TempDir::new("mail-ask-proper");
        let bd = FakeBd::new(vec![BdOut::ok("sp-new1\n"), BdOut::ok("")]);
        let out = send_ask(t.path(), &bd, "grant it", "permissions", "## Question\nq\n\n## Class basis\nneeds a credential\n");
        let calls = bd.calls();
        assert!(calls[0].0.iter().any(|a| a.starts_with("needs-x")), "{calls:?}");
        assert!(out.delivered_path.to_string_lossy().contains("/operator/"), "{:?}", out.delivered_path);
    }
}
