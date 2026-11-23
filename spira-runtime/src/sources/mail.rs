//! The real `mail.arrived` source (`tasks::g::mail_arrived`): one watch on `<mailbox>/new` per
//! registered reader, a catch-up submit per mailbox already holding unread mail, then one submit
//! per wait that saw a delivery. Replaces the watch half of `spira/spira-mail-deliver.sh`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use runtime::host::Submit;
use runtime::sources::on_pool;
use tokio::sync::mpsc;

use super::watch::Watch;
use crate::tasks::g::mail_arrived::Arrived;

/// How long the source backs off after its watch fails.
const BACKOFF: Duration = Duration::from_millis(500);

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

async fn say(what: String) {
    let _ = on_pool("mail.arrived", Arc::new(move || eprintln!("spira-runtime: source mail.arrived: {what}"))).await;
}

async fn send(tx: &mpsc::Sender<Submit>, seq: &mut BTreeMap<String, u64>, mailbox: &str, kind_event: bool) -> bool {
    let n = seq.entry(mailbox.to_string()).or_default();
    *n += 1;
    tx.send(Arrived { mailbox: mailbox.to_string(), kind_event }.submit(*n)).await.is_ok()
}

async fn event_among(new: &Path, files: Vec<String>) -> bool {
    let new = new.to_path_buf();
    on_pool("mail.arrived", Arc::new(move || any_event(&new, &files))).await.unwrap_or(false)
}

/// Runs the source over `mailboxes` on `watch` until `tx` closes. A mailbox whose `new/` cannot
/// be watched is said once on stderr and left out; the rest are still watched. A full channel
/// holds this source at its send; arrivals wait in the watch.
pub async fn run(mailboxes: &[(String, PathBuf)], watch: Box<dyn Watch>, tx: mpsc::Sender<Submit>) {
    if tx.is_closed() {
        return;
    }
    let watch = Arc::new(tokio::sync::Mutex::new(watch));
    let mut seq: BTreeMap<String, u64> = BTreeMap::new();
    let mut by_dir: BTreeMap<PathBuf, String> = BTreeMap::new();
    for (mailbox, dir) in mailboxes {
        let new = dir.join("new");
        let added = {
            let (watch, new) = (watch.clone(), new.clone());
            on_pool("mail.arrived", Arc::new(move || watch.blocking_lock().add(&new))).await
        };
        if let Err(e) = added.map_err(|e| e.to_string()).and_then(|r| r) {
            say(format!("{mailbox}: not watched: {e}")).await;
            continue;
        }
        by_dir.insert(new.clone(), mailbox.clone());
        let files = {
            let new = new.clone();
            on_pool("mail.arrived", Arc::new(move || unread(&new))).await.unwrap_or_default()
        };
        if !files.is_empty() {
            let kind_event = event_among(&new, files).await;
            if !send(&tx, &mut seq, mailbox, kind_event).await {
                return;
            }
        }
    }
    let mut watch = watch.lock().await;
    loop {
        let changed = tokio::select! {
            _ = tx.closed() => return,
            c = watch.changed() => c,
        };
        let changed = match changed {
            Ok(c) => c,
            Err(e) => {
                say(e).await;
                tokio::time::sleep(BACKOFF).await; // clock-safe: backs off a failing watch on the source's own task
                continue;
            }
        };
        for (dir, files) in changed {
            let Some(mailbox) = by_dir.get(&dir) else { continue };
            let kind_event = event_among(&dir, files).await;
            if !send(&tx, &mut seq, mailbox, kind_event).await {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tvouois)]
    #[test]
    fn the_registered_readers_name_the_mailboxes_and_nothing_else() {
        let got = mailboxes("# readers\nconcierge=inbox-append.sh\n\nops = wake ops\nconcierge=again\nbroken\n", Path::new("m"));
        assert_eq!(got, vec![("concierge".to_string(), PathBuf::from("m/concierge")), ("ops".to_string(), PathBuf::from("m/ops"))]);
    }

    #[test_id(txy6bdd)]
    #[test]
    fn the_kind_header_is_read_from_the_headers_only() {
        assert_eq!(kind_of("From: x\nX-Spira-Kind: event\n\nbody").as_deref(), Some("event"));
        assert_eq!(kind_of("x-spira-kind:note\n\n").as_deref(), Some("note"));
        assert_eq!(kind_of("From: x\n\nX-Spira-Kind: event\n"), None, "a body line is not a header");
    }

    /// A watch that says when the source first waits: by then every mailbox is added and caught up.
    struct Waiting {
        inner: crate::sources::watch::ListingWatch,
        first_wait: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl Watch for Waiting {
        fn add(&mut self, dir: &Path) -> Result<(), String> {
            self.inner.add(dir)
        }

        fn changed(&mut self) -> crate::sources::watch::Changed<'_> {
            if let Some(tx) = self.first_wait.take() {
                let _ = tx.send(());
            }
            self.inner.changed()
        }
    }

    #[test_id(t8n3n4g)]
    #[tokio::test]
    async fn a_delivery_after_the_catch_up_is_submitted_with_its_kind() {
        let dir = testkit::TempDir::new("mail-arrived-live");
        let new = dir.path().join("ops/new");
        std::fs::create_dir_all(&new).unwrap();
        let (tx, mut rx) = mpsc::channel(4);
        let boxes = vec![("ops".to_string(), dir.path().join("ops"))];
        let (waiting_tx, waiting) = tokio::sync::oneshot::channel();
        let watch = Waiting { inner: crate::sources::watch::ListingWatch::default(), first_wait: Some(waiting_tx) };
        let h = tokio::spawn(async move { run(&boxes, Box::new(watch), tx).await });
        waiting.await.expect("the source caught up and waits");
        let staged = dir.path().join("m1.staged");
        std::fs::write(&staged, "X-Spira-Kind: event\n\nhi\n").unwrap();
        std::fs::rename(&staged, new.join("m1")).unwrap();
        let got = rx.recv().await.expect("the arrival");
        drop(rx);
        h.await.unwrap();
        assert_eq!(got.id, "mail.arrived/ops/1", "no catch-up for an empty mailbox, so this is the first");
        assert_eq!(crate::tasks::g::mail_arrived::Arrived::from_event_kind(&got.kind), Some(Arrived { mailbox: "ops".into(), kind_event: true }));
    }
}
