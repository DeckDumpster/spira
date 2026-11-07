//! A dwarf's mailbox: `<mail>/dwarf-<bead>/{new,cur,tmp}`, open while its session runs.

use std::path::{Path, PathBuf};

/// Mail left by `bead.sh amend --settle` survives the mailbox's removal.
const KEPT: &str = "amend";

pub fn dir(mail: &str, bead: &str) -> PathBuf {
    Path::new(mail).join(format!("dwarf-{bead}"))
}

/// A mailbox needs a mail root; an empty one means mail is unconfigured and nothing is made.
pub fn open(mail: &str, bead: &str) {
    if mail.is_empty() {
        return;
    }
    for sub in ["new", "cur", "tmp"] {
        let _ = std::fs::create_dir_all(dir(mail, bead).join(sub));
    }
}

pub fn close(mail: &str, bead: &str) {
    let box_dir = dir(mail, bead);
    if let Ok(rd) = std::fs::read_dir(&box_dir) {
        for e in rd.filter_map(|e| e.ok()).filter(|e| e.file_name() != KEPT) {
            let _ = std::fs::remove_dir_all(e.path()).or_else(|_| std::fs::remove_file(e.path()));
        }
    }
    let _ = std::fs::remove_dir(&box_dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: UC-operator-channel-12
    #[test]
    fn open_creates_new_cur_and_tmp() {
        let t = testkit::TempDir::new("mailbox-open");
        let mail = t.path().to_str().unwrap();
        open(mail, "sp-x");
        for sub in ["new", "cur", "tmp"] {
            assert!(dir(mail, "sp-x").join(sub).is_dir(), "{sub}");
        }
    }

    // covers: UC-operator-channel-12
    #[test]
    fn close_removes_the_mailbox_and_its_messages() {
        let t = testkit::TempDir::new("mailbox-close");
        let mail = t.path().to_str().unwrap();
        open(mail, "sp-x");
        std::fs::write(dir(mail, "sp-x").join("new/m1"), "hi").unwrap();
        close(mail, "sp-x");
        assert!(!dir(mail, "sp-x").exists());
    }

    // covers: UC-operator-channel-12
    #[test]
    fn close_keeps_amend_and_so_the_directory() {
        let t = testkit::TempDir::new("mailbox-close-amend");
        let mail = t.path().to_str().unwrap();
        open(mail, "sp-x");
        std::fs::create_dir_all(dir(mail, "sp-x").join("amend")).unwrap();
        close(mail, "sp-x");
        assert!(dir(mail, "sp-x").join("amend").is_dir());
        assert!(!dir(mail, "sp-x").join("new").exists());
    }

    // covers: UC-operator-channel-12
    #[test]
    fn close_of_a_mailbox_never_opened_is_quiet() {
        let t = testkit::TempDir::new("mailbox-close-absent");
        close(t.path().to_str().unwrap(), "sp-x");
    }
}
