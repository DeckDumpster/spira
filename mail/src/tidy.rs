//! `tidy`: archive old, answered and closed mail from a mailbox (the operator inbox, in
//! production). A message is kept only if (1) its bead id names an open ask-labelled bead,
//! (2) it is unread and younger than the fresh window, or (3) it carries `X-Spira-Urgent`
//! and is younger than 7 days; rules (2) and (3) never keep mail whose bead is closed.
//! Older copies of a repeated subject are always archived.
//! FAIL CLOSED throughout (law-a-control-that-cannot-check-must-refuse): an unreadable bead
//! store, or a mailbox that was never created, moves nothing rather than guessing.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::bead::{self, Bd};
use crate::maildir;
use crate::message;

/// 7 days — fixed, not configurable (mail.sh's own constant).
pub const URGENT_MAX_S: u64 = 604_800;

#[derive(Debug, Default)]
pub struct TidyReport {
    pub archived: usize,
    pub kept: usize,
}

/// One message as the keep rule reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Seen {
    pub path: std::path::PathBuf,
    pub mtime: u64,
    pub subject: String,
    /// `X-Spira-Bead`; empty when the mail has none.
    pub bead: String,
    pub unread: bool,
    pub urgent: bool,
}

impl Seen {
    /// Whether rule (2) or (3) would keep it, before the closed-bead exception.
    pub fn fresh(&self, now: u64, fresh_s: u64) -> bool {
        let age = now.saturating_sub(self.mtime);
        (self.unread && age < fresh_s) || (self.urgent && age < URGENT_MAX_S)
    }
}

/// `<mail_root>/<mailbox>/{new,cur}`, newest first. Refuses a mailbox that was never created.
pub fn scan(mail_root: &Path, mailbox: &str) -> Result<Vec<Seen>, String> {
    let inbox_dir = mail_root.join(mailbox);
    if !(inbox_dir.join("cur").is_dir() && inbox_dir.join("new").is_dir()) {
        return Err(format!("{mailbox}: mailbox not found"));
    }
    let mut files: Vec<(u64, std::path::PathBuf)> = Vec::new();
    for sub in ["new", "cur"] {
        for p in maildir::sorted_entries(&inbox_dir.join(sub)) {
            files.push((maildir::mtime_secs(&p), p));
        }
    }
    // `sort -rn` on "<mtime>\t<path>" lines: newest first. Rust's sort_by is stable, so
    // same-mtime files keep the new/-then-cur, filename-sorted order collected above.
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    Ok(files
        .into_iter()
        .map(|(mtime, path)| {
            let text = fs::read_to_string(&path).unwrap_or_default();
            // The mail's bead is its header's, never an id scraped from the body's prose.
            let bead = message::header_line_sed(&text, "X-Spira-Bead");
            let urgent = !message::header_line_sed(&text, "X-Spira-Urgent").is_empty();
            Seen { unread: maildir::is_unread(&path), subject: message::header_line_sed(&text, "Subject"), bead, urgent, mtime, path }
        })
        .collect())
}

/// The keep rule over `seen` (newest first), one verdict per message: kept iff it is the first
/// copy of its subject AND (its bead is an open ask OR (it is fresh AND its bead is not known
/// closed)). `closed` is asked only for a fresh first copy that names a bead.
pub fn keeps(seen: &[Seen], open_asks: &dyn Fn(&str) -> bool, now: u64, fresh_s: u64, closed: &mut dyn FnMut(&str) -> bool) -> Vec<bool> {
    let mut subjects: HashSet<&str> = HashSet::new();
    seen.iter()
        .map(|m| {
            if !subjects.insert(m.subject.as_str()) {
                return false;
            }
            let open_ask = !m.bead.is_empty() && open_asks(&m.bead);
            open_ask || (m.fresh(now, fresh_s) && !(!m.bead.is_empty() && closed(&m.bead)))
        })
        .collect()
}

/// Moves each of `paths` into `<mail_root>/archive/cur`, creating the archive mailbox first. A
/// source already gone is a no-op; a path outside `mail_root` is refused. Returns how many moved.
pub fn archive(mail_root: &Path, paths: &[std::path::PathBuf]) -> Result<usize, String> {
    let archive_dir = mail_root.join("archive");
    maildir::mail_ensure(&archive_dir).map_err(|e| format!("cannot create archive mailbox: {e}"))?;
    let mut moved = 0;
    let mut failed = vec![];
    for path in paths {
        if !path.starts_with(mail_root) || path.starts_with(&archive_dir) {
            return Err(format!("{} is not a message under {} — refusing", path.display(), mail_root.display()));
        }
        let Some(name) = path.file_name() else { continue };
        if !path.exists() {
            continue;
        }
        match fs::rename(path, archive_dir.join("cur").join(name)) {
            Ok(()) => moved += 1,
            Err(e) => failed.push(format!("{}: {e}", path.display())),
        }
    }
    if failed.is_empty() {
        Ok(moved)
    } else {
        Err(format!("archived {moved}, failed {}: {}", failed.len(), failed.join("; ")))
    }
}

/// The open ask ids, with the positive control an empty answer needs: `Err` when the store is
/// not configured, the query fails, or an empty answer fails the control.
pub fn open_asks(bd: &dyn Bd, db_configured: bool, ask_label: &str) -> Result<HashSet<String>, String> {
    if !db_configured {
        return Err("bead store not configured — refusing to move any mail".to_string());
    }
    let ask_ids: HashSet<String> = bead::open_ask_ids(bd, ask_label)?.into_iter().collect();
    if ask_ids.is_empty() {
        if let Err(out) = bead::probe_store(bd) {
            return Err(format!("positive control failed {} — bead store unreadable; refusing to move any mail", bead::bd_failure_detail(&out)));
        }
    }
    Ok(ask_ids)
}

#[allow(clippy::too_many_arguments)]
pub fn tidy(
    bd: &dyn Bd,
    db_configured: bool,
    mail_root: &Path,
    mailbox: &str,
    ask_label: &str,
    fresh_s: u64,
    dry_run: bool,
) -> Result<TidyReport, String> {
    let ask_ids = open_asks(bd, db_configured, ask_label)?;
    let seen = scan(mail_root, mailbox)?;
    let archive_dir = mail_root.join("archive");
    maildir::mail_ensure(&archive_dir).map_err(|e| format!("cannot create archive mailbox: {e}"))?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let verdicts = keeps(&seen, &|id| ask_ids.contains(id), now, fresh_s, &mut |id| bead::bead_is_closed(bd, id));
    let mut report = TidyReport::default();
    for (m, keep) in seen.iter().zip(verdicts) {
        if keep {
            report.kept += 1;
        } else {
            report.archived += 1;
            if !dry_run {
                if let Some(name) = m.path.file_name() {
                    let _ = fs::rename(&m.path, archive_dir.join("cur").join(name));
                }
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead::fake::FakeBd;
    use crate::bead::BdOut;
    use std::fs;
    use std::time::Duration;

    fn seed_message(dir: &Path, sub: &str, name: &str, subject: &str, bead_hdr: Option<&str>, urgent: bool, age_s: u64) {
        let mut text = format!("From: Bot <bot@spira>\nSubject: {subject}\n");
        if let Some(b) = bead_hdr {
            text.push_str(&format!("X-Spira-Bead: {b}\n"));
        }
        if urgent {
            text.push_str("X-Spira-Urgent: yes\n");
        }
        text.push_str("\nbody\n");
        let path = dir.join(sub).join(name);
        fs::write(&path, text).unwrap();
        let t = filetime_secs_ago(age_s);
        let _ = filetime_set(&path, t);
    }

    // Minimal mtime-setting without pulling in a filetime crate: utimensat via libc.
    fn filetime_secs_ago(age_s: u64) -> i64 {
        (SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64) - age_s as i64
    }
    fn filetime_set(path: &Path, secs: i64) -> std::io::Result<()> {
        use std::ffi::CString;
        let c = CString::new(path.as_os_str().to_str().unwrap()).unwrap();
        let times = [libc::timespec { tv_sec: secs, tv_nsec: 0 }, libc::timespec { tv_sec: secs, tv_nsec: 0 }];
        let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0) };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    fn setup(root: &Path, mailbox: &str) {
        maildir::mail_ensure(&root.join(mailbox)).unwrap();
    }

    #[test]
    fn refuses_without_a_configured_store() {
        let d = testkit::TempDir::new("mail-tidy");
        let bd = FakeBd::new(vec![]);
        let err = tidy(&bd, false, d.path(), "operator", "asks", 3600, false).unwrap_err();
        assert!(err.contains("not configured"), "{err}");
    }

    #[test]
    fn refuses_when_the_mailbox_was_never_created() {
        let d = testkit::TempDir::new("mail-tidy");
        let bd = FakeBd::new(vec![BdOut::ok("[]"), BdOut::ok(r#"[{"id":"probe"}]"#)]);
        let err = tidy(&bd, true, d.path(), "operator", "asks", 3600, false).unwrap_err();
        assert!(err.contains("mailbox not found"), "{err}");
    }

    #[test]
    fn refuses_when_the_query_fails() {
        let d = testkit::TempDir::new("mail-tidy");
        setup(d.path(), "operator");
        let bd = FakeBd::new(vec![BdOut::fail(1, "boom")]);
        let err = tidy(&bd, true, d.path(), "operator", "asks", 3600, false).unwrap_err();
        assert!(err.contains("refusing"), "{err}");
    }

    #[test]
    fn refuses_when_empty_result_fails_the_positive_control() {
        let d = testkit::TempDir::new("mail-tidy");
        setup(d.path(), "operator");
        let bd = FakeBd::new(vec![BdOut::ok("[]"), BdOut::fail(1, "down")]);
        let err = tidy(&bd, true, d.path(), "operator", "asks", 3600, false).unwrap_err();
        assert!(err.contains("positive control"), "{err}");
    }

    #[test]
    fn positive_control_failure_names_bds_exit_code_and_stderr() {
        let d = testkit::TempDir::new("mail-tidy");
        setup(d.path(), "operator");
        let bd = FakeBd::new(vec![BdOut::ok("[]"), BdOut::fail(3, "Error 1045: Access denied for user 'spira_lc'")]);
        let err = tidy(&bd, true, d.path(), "operator", "asks", 3600, false).unwrap_err();
        assert!(err.contains("bd exit 3"), "{err}");
        assert!(err.contains("Access denied for user 'spira_lc'"), "{err}");
    }

    #[test]
    fn open_ask_is_kept_closed_ask_is_archived() {
        let d = testkit::TempDir::new("mail-tidy");
        setup(d.path(), "operator");
        seed_message(&d.path().join("operator"), "new", "a", "Open question", Some("sp-open1"), false, 10);
        seed_message(&d.path().join("operator"), "new", "b", "Closed question", Some("sp-closed1"), false, 7200);
        let bd = FakeBd::new(vec![BdOut::ok(r#"[{"id":"sp-open1"}]"#)]);
        let report = tidy(&bd, true, d.path(), "operator", "asks", 3600, false).unwrap();
        assert_eq!((report.archived, report.kept), (1, 1));
        assert!(d.path().join("operator/new/a").exists());
        assert!(!d.path().join("operator/new/b").exists());
        assert!(d.path().join("archive/cur/b").exists());
    }

    #[test]
    fn fresh_unread_is_kept_even_without_a_bead() {
        let d = testkit::TempDir::new("mail-tidy");
        setup(d.path(), "operator");
        seed_message(&d.path().join("operator"), "new", "fresh", "Fresh note", None, false, 5);
        let bd = FakeBd::new(vec![BdOut::ok("[]"), BdOut::ok(r#"[{"id":"probe"}]"#)]);
        let report = tidy(&bd, true, d.path(), "operator", "asks", 3600, false).unwrap();
        assert_eq!((report.archived, report.kept), (0, 1));
    }

    #[test]
    fn fresh_unread_mail_of_a_closed_bead_is_archived_open_one_is_kept() {
        let d = testkit::TempDir::new("mail-tidy");
        setup(d.path(), "operator");
        seed_message(&d.path().join("operator"), "new", "c", "Closed one", Some("sp-closed1"), false, 5);
        seed_message(&d.path().join("operator"), "new", "u", "Urgent closed", Some("sp-closed2"), true, 3);
        seed_message(&d.path().join("operator"), "new", "o", "Open one", Some("sp-open1"), false, 10);
        let bd = FakeBd::new(vec![
            BdOut::ok(r#"[{"id":"sp-open1"}]"#),
            BdOut::ok(r#"[{"id":"sp-closed2","status":"closed"}]"#),
            BdOut::ok(r#"[{"id":"sp-closed1","status":"closed"}]"#),
        ]);
        let report = tidy(&bd, true, d.path(), "operator", "asks", 3600, false).unwrap();
        assert_eq!((report.archived, report.kept), (2, 1));
        assert!(d.path().join("operator/new/o").exists());
        assert!(d.path().join("archive/cur/c").exists());
        assert!(d.path().join("archive/cur/u").exists());
    }

    /// A bead id in the body is prose, not the mail's subject bead: a fresh unread note that
    /// mentions a closed bead is not that bead's mail, and is kept.
    #[test]
    fn a_bead_named_only_in_the_body_is_not_the_mails_bead() {
        let d = testkit::TempDir::new("mail-tidy");
        setup(d.path(), "operator");
        let path = d.path().join("operator/new/n");
        fs::write(&path, "Subject: Disk is filling\n\nUnrelated to sp-closed1, which closed last week.\n").unwrap();
        let bd = FakeBd::new(vec![BdOut::ok("[]"), BdOut::ok(r#"[{"id":"probe"}]"#), BdOut::ok(r#"[{"id":"sp-closed1","status":"closed"}]"#)]);
        let report = tidy(&bd, true, d.path(), "operator", "asks", 3600, false).unwrap();
        assert_eq!((report.archived, report.kept), (0, 1), "the note is kept");
        assert!(path.exists());
    }

    #[test]
    fn fresh_unread_mail_of_an_unresolvable_bead_is_kept() {
        let d = testkit::TempDir::new("mail-tidy");
        setup(d.path(), "operator");
        seed_message(&d.path().join("operator"), "new", "x", "Unknown", Some("sp-gone1"), false, 5);
        let bd = FakeBd::new(vec![BdOut::ok("[]"), BdOut::ok(r#"[{"id":"probe"}]"#), BdOut::fail(1, "no such bead")]);
        let report = tidy(&bd, true, d.path(), "operator", "asks", 3600, false).unwrap();
        assert_eq!((report.archived, report.kept), (0, 1));
    }

    #[test]
    fn urgent_retention_at_both_edges_of_the_seven_day_window() {
        let d = testkit::TempDir::new("mail-tidy");
        setup(d.path(), "operator");
        // Both read (old, so only the urgent rule can save either).
        let inside = URGENT_MAX_S - 60;
        let outside = URGENT_MAX_S + 60;
        seed_message(&d.path().join("operator"), "cur", "in:2,S", "Urgent still fresh", None, true, inside);
        seed_message(&d.path().join("operator"), "cur", "out:2,S", "Urgent expired", None, true, outside);
        let bd = FakeBd::new(vec![BdOut::ok("[]"), BdOut::ok(r#"[{"id":"probe"}]"#)]);
        let report = tidy(&bd, true, d.path(), "operator", "asks", 3600, false).unwrap();
        assert_eq!((report.archived, report.kept), (1, 1));
        assert!(d.path().join("operator/cur/in:2,S").exists());
        assert!(!d.path().join("operator/cur/out:2,S").exists());
    }

    #[test]
    fn only_the_newest_copy_of_a_repeated_subject_is_kept() {
        let d = testkit::TempDir::new("mail-tidy");
        setup(d.path(), "operator");
        seed_message(&d.path().join("operator"), "new", "e1", "Daily report", None, false, 14400);
        seed_message(&d.path().join("operator"), "new", "e2", "Daily report", None, false, 7200);
        seed_message(&d.path().join("operator"), "new", "e3", "Daily report", None, false, 10);
        let bd = FakeBd::new(vec![BdOut::ok("[]"), BdOut::ok(r#"[{"id":"probe"}]"#)]);
        let report = tidy(&bd, true, d.path(), "operator", "asks", 3600, false).unwrap();
        assert_eq!((report.archived, report.kept), (2, 1));
        assert!(d.path().join("operator/new/e3").exists());
        assert!(!d.path().join("operator/new/e1").exists());
        assert!(!d.path().join("operator/new/e2").exists());
    }

    #[test]
    fn dry_run_reports_without_moving() {
        let d = testkit::TempDir::new("mail-tidy");
        setup(d.path(), "operator");
        seed_message(&d.path().join("operator"), "new", "old", "Dry run test", None, false, 7200);
        let bd = FakeBd::new(vec![BdOut::ok("[]"), BdOut::ok(r#"[{"id":"probe"}]"#)]);
        let report = tidy(&bd, true, d.path(), "operator", "asks", 3600, true).unwrap();
        assert_eq!(report.archived, 1);
        assert!(d.path().join("operator/new/old").exists());
        let _ = Duration::from_secs(0);
    }
}
