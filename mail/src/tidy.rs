//! `tidy`: archive old, answered and closed mail from a mailbox (the operator inbox, in
//! production). A message is kept only if (1) its bead id names an open ask-labelled bead,
//! (2) it is unread and younger than the fresh window, or (3) it carries `X-Spira-Urgent`
//! and is younger than 7 days. Older copies of a repeated subject are always archived.
//! FAIL CLOSED throughout (law-a-control-that-cannot-check-must-refuse): an unreadable bead
//! store, or a mailbox that was never created, moves nothing rather than guessing.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::bead::{self, Bd};
use crate::lint::bead_id_regex;
use crate::maildir;
use crate::message;

const URGENT_MAX_S: u64 = 604_800; // 7 days — fixed, not configurable (mail.sh's own constant)

#[derive(Debug, Default)]
pub struct TidyReport {
    pub archived: usize,
    pub kept: usize,
}

pub fn tidy(
    bd: &dyn Bd,
    db_configured: bool,
    mail_root: &Path,
    mailbox: &str,
    ask_label: &str,
    fresh_s: u64,
    id_prefix: &str,
    dry_run: bool,
) -> Result<TidyReport, String> {
    if !db_configured {
        return Err("bead store not configured — refusing to move any mail".to_string());
    }

    let ask_ids: HashSet<String> = bead::open_ask_ids(bd, ask_label)?.into_iter().collect();
    if ask_ids.is_empty() {
        if let Err(out) = bead::probe_store(bd) {
            return Err(format!("positive control failed {} — bead store unreadable; refusing to move any mail", bead::bd_failure_detail(&out)));
        }
    }

    let inbox_dir = mail_root.join(mailbox);
    if !(inbox_dir.join("cur").is_dir() && inbox_dir.join("new").is_dir()) {
        return Err(format!("{mailbox}: mailbox not found"));
    }

    let archive_dir = mail_root.join("archive");
    maildir::mail_ensure(&archive_dir).map_err(|e| format!("cannot create archive mailbox: {e}"))?;

    let mut files: Vec<(u64, std::path::PathBuf)> = Vec::new();
    for sub in ["new", "cur"] {
        for p in maildir::sorted_entries(&inbox_dir.join(sub)) {
            files.push((maildir::mtime_secs(&p), p));
        }
    }
    // `sort -rn` on "<mtime>\t<path>" lines: newest first. Rust's sort_by is stable, so
    // same-mtime files keep the new/-then-cur, filename-sorted order collected above.
    files.sort_by(|a, b| b.0.cmp(&a.0));

    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let bead_re = bead_id_regex(id_prefix);

    let mut seen_subjects: HashSet<String> = HashSet::new();
    let mut report = TidyReport::default();

    for (mtime, path) in files {
        let text = fs::read_to_string(&path).unwrap_or_default();
        let subj = message::header_line_sed(&text, "Subject");

        let keep = if seen_subjects.contains(&subj) {
            false
        } else {
            seen_subjects.insert(subj);

            let mut bead_id = message::header_line_sed(&text, "X-Spira-Bead");
            if bead_id.is_empty() {
                let (_, body) = message::split_headers_body(&text);
                bead_id = bead_re.find(body).map(|m| m.as_str().to_string()).unwrap_or_default();
            }
            let age = now.saturating_sub(mtime);
            let urgent_hdr = message::header_line_sed(&text, "X-Spira-Urgent");

            (!bead_id.is_empty() && ask_ids.contains(&bead_id))
                || (maildir::is_unread(&path) && age < fresh_s)
                || (!urgent_hdr.is_empty() && age < URGENT_MAX_S)
        };

        if keep {
            report.kept += 1;
        } else {
            report.archived += 1;
            if !dry_run {
                if let Some(name) = path.file_name() {
                    let _ = fs::rename(&path, archive_dir.join("cur").join(name));
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
        let err = tidy(&bd, false, d.path(), "operator", "asks", 3600, "sp", false).unwrap_err();
        assert!(err.contains("not configured"), "{err}");
    }

    #[test]
    fn refuses_when_the_mailbox_was_never_created() {
        let d = testkit::TempDir::new("mail-tidy");
        let bd = FakeBd::new(vec![BdOut::ok("[]"), BdOut::ok(r#"[{"id":"probe"}]"#)]);
        let err = tidy(&bd, true, d.path(), "operator", "asks", 3600, "sp", false).unwrap_err();
        assert!(err.contains("mailbox not found"), "{err}");
    }

    #[test]
    fn refuses_when_the_query_fails() {
        let d = testkit::TempDir::new("mail-tidy");
        setup(d.path(), "operator");
        let bd = FakeBd::new(vec![BdOut::fail(1, "boom")]);
        let err = tidy(&bd, true, d.path(), "operator", "asks", 3600, "sp", false).unwrap_err();
        assert!(err.contains("refusing"), "{err}");
    }

    #[test]
    fn refuses_when_empty_result_fails_the_positive_control() {
        let d = testkit::TempDir::new("mail-tidy");
        setup(d.path(), "operator");
        let bd = FakeBd::new(vec![BdOut::ok("[]"), BdOut::fail(1, "down")]);
        let err = tidy(&bd, true, d.path(), "operator", "asks", 3600, "sp", false).unwrap_err();
        assert!(err.contains("positive control"), "{err}");
    }

    #[test]
    fn positive_control_failure_names_bds_exit_code_and_stderr() {
        let d = testkit::TempDir::new("mail-tidy");
        setup(d.path(), "operator");
        let bd = FakeBd::new(vec![BdOut::ok("[]"), BdOut::fail(3, "Error 1045: Access denied for user 'spira_lc'")]);
        let err = tidy(&bd, true, d.path(), "operator", "asks", 3600, "sp", false).unwrap_err();
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
        let report = tidy(&bd, true, d.path(), "operator", "asks", 3600, "sp", false).unwrap();
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
        let report = tidy(&bd, true, d.path(), "operator", "asks", 3600, "sp", false).unwrap();
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
        let report = tidy(&bd, true, d.path(), "operator", "asks", 3600, "sp", false).unwrap();
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
        let report = tidy(&bd, true, d.path(), "operator", "asks", 3600, "sp", false).unwrap();
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
        let report = tidy(&bd, true, d.path(), "operator", "asks", 3600, "sp", true).unwrap();
        assert_eq!(report.archived, 1);
        assert!(d.path().join("operator/new/old").exists());
        let _ = Duration::from_secs(0);
    }
}
