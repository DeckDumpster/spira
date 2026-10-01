//! Maildir mechanics: the directory layout (`tmp/`, `new/`, `cur/`), atomic delivery
//! (mute-aware, sp-9hwim), message-id minting, and the unread/flag rules `list`, `count`,
//! `unread-age`, `done` and `tidy` all share. Every function here does one filesystem
//! operation; the commands in `cmds.rs` compose them.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::env::Env;

/// `_mailbox_valid`: a mailbox name must be non-empty and not look like an option that
/// landed in the mailbox slot by a caller's typo (`mail.sh list --unread` silently creating
/// and reporting on a mailbox literally named "--unread").
pub fn mailbox_valid(mailbox: &str) -> Result<(), String> {
    if mailbox.is_empty() {
        return Err("mailbox required".to_string());
    }
    if mailbox.starts_with('-') {
        return Err(format!(
            "{mailbox}: looks like an option in the mailbox slot, not a mailbox name — refusing"
        ));
    }
    Ok(())
}

pub fn mail_dir(env: &Env, mailbox: &str) -> PathBuf {
    env.mail_root.join(mailbox)
}

/// `_mailbox_exists`: a read verb must never create the mailbox it reads — reporting zero
/// mail from a mailbox that does not exist would be indistinguishable from a real empty
/// inbox. Only `send` and the explicit `ensure` command create one (via [`mail_ensure`]).
pub fn mailbox_exists(dir: &Path) -> bool {
    dir.join("new").is_dir() && dir.join("cur").is_dir()
}

/// `_reply_mailbox`'s own directory check: just the mailbox directory's existence, not the
/// full `new/`+`cur/` [`mailbox_exists`] contract (mail.sh: `[ -d "$(_mail_dir "$localpart")" ]`).
pub fn mail_dir_exists_dir(mail_root: &Path, mailbox: &str) -> bool {
    mail_root.join(mailbox).is_dir()
}

pub fn mail_ensure(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir.join("tmp"))?;
    fs::create_dir_all(dir.join("new"))?;
    fs::create_dir_all(dir.join("cur"))?;
    Ok(())
}

/// `<secs>.<counter-salted-nanos>.<pid>` — unique per process and per call; the exact shape
/// of mail.sh's own `$(date +%s).$RANDOM.$$` is not load-bearing, only that two calls (even
/// concurrent, even in the same process) never collide (G-11).
pub fn mint_msgid() -> String {
    // The process-wide atomic counter is what actually guarantees uniqueness (even two
    // calls in the same process within the same clock tick); the nanosecond component only
    // spreads the middle field the way `$RANDOM` did, it is not load-bearing.
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}.{}.{}", now.as_secs(), n, std::process::id())
}

/// Moves a written message out of `tmp/` into the mailbox proper. Muted
/// (`SPIRA_MAIL_MUTE=1/true`, sp-9hwim, design runtime-is-a-release #5) delivers straight
/// into `cur/` already flagged Seen — the message is recorded (a reader listing `cur/`
/// still finds it) but wakes nobody. Unmuted is unchanged: `new/`.
pub fn mail_deliver(dir: &Path, msgid: &str, mute: bool) -> std::io::Result<PathBuf> {
    let from = dir.join("tmp").join(msgid);
    let to = if mute { dir.join("cur").join(format!("{msgid}:2,S")) } else { dir.join("new").join(msgid) };
    fs::rename(&from, &to)?;
    Ok(to)
}

/// `_is_unread`: `new/` messages are always unread. `cur/` messages carry the Seen flag
/// (`:2,...S...`) when read; everything else in `cur/` (e.g. a message `done` or `read`
/// moved there — `read` itself does not set S) still reads as unread by this rule.
pub fn is_unread(path: &Path) -> bool {
    let s = path.to_string_lossy();
    if s.contains("/new/") {
        return true;
    }
    let base = path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
    match base.split_once(":2,") {
        Some((_, flags)) => !flags.contains('S'),
        None => true,
    }
}

pub fn mtime_secs(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Adds `flag` to a Maildir filename's flag suffix (alphabetically sorted, mail.sh's own
/// `fold -w1 | sort | tr -d '\n'`), or `None` if the flag is already set — no rename needed.
/// Shared by `done` and `sendmail`'s `mark_replied`, both of which only ever add `R`.
pub fn add_flag(filename: &str, flag: char) -> Option<String> {
    match filename.split_once(":2,") {
        Some((base, flags)) => {
            if flags.contains(flag) {
                None
            } else {
                let mut chars: Vec<char> = flags.chars().collect();
                chars.push(flag);
                chars.sort_unstable();
                let flags: String = chars.into_iter().collect();
                Some(format!("{base}:2,{flags}"))
            }
        }
        None => Some(format!("{filename}:2,{flag}")),
    }
}

/// Directory entries, sorted by filename — the ordering a bash glob (`for f in "$d"/*`)
/// produces, which `list`/`read`/`tidy`'s dedup all rely on implicitly.
pub fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_valid_rejects_empty_and_option_looking_names() {
        assert!(mailbox_valid("operator").is_ok());
        assert!(mailbox_valid("").is_err());
        assert!(mailbox_valid("--unread").is_err());
    }

    #[test]
    fn ensure_creates_all_three_maildir_subdirs() {
        let d = testkit::TempDir::new("mail-ensure");
        let dir = d.path().join("operator");
        mail_ensure(&dir).unwrap();
        assert!(dir.join("tmp").is_dir());
        assert!(dir.join("new").is_dir());
        assert!(dir.join("cur").is_dir());
        assert!(mailbox_exists(&dir));
    }

    #[test]
    fn mailbox_exists_is_false_until_ensured() {
        let d = testkit::TempDir::new("mail-exists");
        assert!(!mailbox_exists(&d.path().join("nope")));
    }

    #[test]
    fn deliver_lands_in_new_when_unmuted_and_cur_seen_when_muted() {
        let d = testkit::TempDir::new("mail-deliver");
        let dir = d.path().join("box");
        mail_ensure(&dir).unwrap();
        fs::write(dir.join("tmp/one"), "msg").unwrap();
        let dest = mail_deliver(&dir, "one", false).unwrap();
        assert!(dest.starts_with(dir.join("new")));
        assert!(dest.is_file());

        fs::write(dir.join("tmp/two"), "msg").unwrap();
        let dest2 = mail_deliver(&dir, "two", true).unwrap();
        assert_eq!(dest2.file_name().unwrap().to_str().unwrap(), "two:2,S");
        assert!(dest2.starts_with(dir.join("cur")));
    }

    #[test]
    fn mint_msgid_is_unique_across_many_calls() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            assert!(seen.insert(mint_msgid()));
        }
    }

    #[test]
    fn is_unread_rules() {
        assert!(is_unread(Path::new("/m/box/new/123")));
        assert!(is_unread(Path::new("/m/box/cur/123"))); // no flags at all
        assert!(is_unread(Path::new("/m/box/cur/123:2,R"))); // R but no S
        assert!(!is_unread(Path::new("/m/box/cur/123:2,S")));
        assert!(!is_unread(Path::new("/m/box/cur/123:2,RS")));
    }

    #[test]
    fn add_flag_sorts_and_is_idempotent() {
        assert_eq!(add_flag("123", 'R').unwrap(), "123:2,R");
        assert_eq!(add_flag("123:2,S", 'R').unwrap(), "123:2,RS");
        assert_eq!(add_flag("123:2,R", 'R'), None);
        assert_eq!(add_flag("123:2,RS", 'R'), None);
    }
}
