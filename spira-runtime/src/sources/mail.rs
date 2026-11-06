//! The real `mail.arrived` source (`tasks::g::mail_arrived`): one watch on `<mailbox>/new` per
//! registered reader, a catch-up submit per mailbox already holding unread mail, then one submit
//! per wait that saw a delivery. Replaces the watch half of `spira/spira-mail-deliver.sh`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use runtime::host::Submit;
use tokio::sync::mpsc;

use super::watch::Watch;
use crate::tasks::g::mail_arrived::Arrived;

/// How long one wait lasts before the source looks whether its channel closed.
const WAIT: Duration = Duration::from_millis(500);

/// The registered mailboxes in `readers` (`SPIRA_MAIL_READERS`: `<mailbox>=<wake command>`, one
/// per line, `#` comments), each with its directory under `mail_root`, in the order given.
pub fn mailboxes(readers: &str, mail_root: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = vec![];
    for line in readers.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let Some((mailbox, _)) = line.split_once('=') else { continue };
        let mailbox = mailbox.trim();
        if !mailbox.is_empty() && !out.iter().any(|(m, _)| m == mailbox) {
            out.push((mailbox.to_string(), mail_root.join(mailbox)));
        }
    }
    out
}

/// A message's `X-Spira-Kind` header, from the headers before the first blank line.
fn kind_of(text: &str) -> Option<String> {
    text.lines().take_while(|l| !l.trim().is_empty()).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case("x-spira-kind").then(|| v.trim().to_string())
    })
}

/// Whether any of `files` in `new` is a machine `--kind event` mail (the script's settle rule).
fn any_event(new: &Path, files: &[String]) -> bool {
    files.iter().any(|f| std::fs::read_to_string(new.join(f)).ok().and_then(|t| kind_of(&t)).is_some_and(|k| k == "event"))
}

fn unread(new: &Path) -> Vec<String> {
    let mut files: Vec<String> = std::fs::read_dir(new).into_iter().flatten().flatten().filter(|e| e.path().is_file()).filter_map(|e| e.file_name().into_string().ok()).collect();
    files.sort();
    files
}

/// Runs the source over `mailboxes` on `watch` until `tx` closes. A mailbox whose `new/` cannot
/// be watched is said once on stderr and left out; the rest are still watched.
pub fn run(mailboxes: &[(String, PathBuf)], mut watch: Box<dyn Watch>, tx: mpsc::Sender<Submit>) {
    if tx.is_closed() {
        return;
    }
    let mut seq: BTreeMap<String, u64> = BTreeMap::new();
    let mut by_dir: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut send = |mailbox: &str, kind_event: bool| -> bool {
        let n = seq.entry(mailbox.to_string()).or_default();
        *n += 1;
        tx.blocking_send(Arrived { mailbox: mailbox.to_string(), kind_event }.submit(*n)).is_ok()
    };
    for (mailbox, dir) in mailboxes {
        let new = dir.join("new");
        if let Err(e) = watch.add(&new) {
            eprintln!("spira-runtime: source mail.arrived: {mailbox}: not watched: {e}");
            continue;
        }
        by_dir.insert(new.clone(), mailbox.clone());
        let files = unread(&new);
        if !files.is_empty() && !send(mailbox, any_event(&new, &files)) {
            return;
        }
    }
    loop {
        if tx.is_closed() {
            return;
        }
        let changed = match watch.wait(WAIT) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("spira-runtime: source mail.arrived: {e}");
                std::thread::sleep(WAIT); // clock-safe: backs off a failing watch on the source's own thread
                continue;
            }
        };
        for (dir, files) in changed {
            let Some(mailbox) = by_dir.get(&dir) else { continue };
            if !send(mailbox, any_event(&dir, &files)) {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registered_readers_name_the_mailboxes_and_nothing_else() {
        let got = mailboxes("# readers\nconcierge=inbox-append.sh\n\nops = wake ops\nconcierge=again\nbroken\n", Path::new("/m"));
        assert_eq!(got, vec![("concierge".to_string(), PathBuf::from("/m/concierge")), ("ops".to_string(), PathBuf::from("/m/ops"))]);
    }

    #[test]
    fn the_kind_header_is_read_from_the_headers_only() {
        assert_eq!(kind_of("From: x\nX-Spira-Kind: event\n\nbody").as_deref(), Some("event"));
        assert_eq!(kind_of("x-spira-kind:note\n\n").as_deref(), Some("note"));
        assert_eq!(kind_of("From: x\n\nX-Spira-Kind: event\n"), None, "a body line is not a header");
    }

    /// A watch that says when the source first waits: by then every mailbox is added and caught up.
    struct Waiting {
        inner: super::super::watch::ListingWatch,
        first_wait: Option<std::sync::mpsc::Sender<()>>,
    }

    impl Watch for Waiting {
        fn add(&mut self, dir: &Path) -> Result<(), String> {
            self.inner.add(dir)
        }

        fn wait(&mut self, timeout: Duration) -> Result<Vec<(PathBuf, Vec<String>)>, String> {
            if let Some(tx) = self.first_wait.take() {
                let _ = tx.send(());
            }
            self.inner.wait(timeout)
        }
    }

    #[test]
    fn a_delivery_after_the_catch_up_is_submitted_with_its_kind() {
        let dir = testkit::TempDir::new("mail-arrived-live");
        let new = dir.path().join("ops/new");
        std::fs::create_dir_all(&new).unwrap();
        let (tx, mut rx) = mpsc::channel(4);
        let boxes = vec![("ops".to_string(), dir.path().join("ops"))];
        let (waiting_tx, waiting) = std::sync::mpsc::channel();
        let watch = Waiting { inner: super::super::watch::ListingWatch::default(), first_wait: Some(waiting_tx) };
        let h = std::thread::spawn(move || run(&boxes, Box::new(watch), tx));
        waiting.recv().expect("the source caught up and waits");
        std::fs::write(new.join("m1"), "X-Spira-Kind: event\n\nhi\n").unwrap();
        let got = rx.blocking_recv().expect("the arrival");
        drop(rx);
        h.join().unwrap();
        assert_eq!(got.id, "mail.arrived/ops/1", "no catch-up for an empty mailbox, so this is the first");
        assert_eq!(crate::tasks::g::mail_arrived::Arrived::from_event_kind(&got.kind), Some(Arrived { mailbox: "ops".into(), kind_event: true }));
    }
}
