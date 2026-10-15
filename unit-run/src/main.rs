//! unit-run follow | backfill [--force] | ensure-accounting [--dir DIR]
//!
//! follow: tail the user journal from the cursor last checkpointed, writing one record per
//! unit execution to run/tsd/unit-run.jsonl. backfill: replay the journal from its oldest
//! entry, writing only what the file lacks; seeds the follower's checkpoint when it has none.
//! follow never replays: with no checkpoint it starts at now.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;
use unit_run::{accounting_dropin_path, ensure_io_accounting, journal_args, pass, written_ids, Clock, io_controller_available, own_controllers_file, Folder, Item, Proc, Sink, Source, Triggers};

struct Wall;
impl Clock for Wall {
    fn now_epoch(&self) -> u64 {
        spira_config::vtime::now_epoch()
    }
}

struct Spawn;
impl Proc for Spawn {
    fn run(&self, argv: &[&str]) -> Result<String, String> {
        let out = spira_config::bounded::bounded(argv[0]).args(&argv[1..]).output().map_err(|e| format!("{}: {e}", argv[0]))?;
        if !out.status.success() {
            return Err(format!("{}: {}", argv[0], String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

struct Journal;

const CHANNEL_DEPTH: usize = 1024;

struct Lines {
    rx: mpsc::Receiver<String>,
    follow: bool,
    child: Child,
}

impl Iterator for Lines {
    type Item = Item;
    fn next(&mut self) -> Option<Item> {
        if !self.follow {
            return self.rx.recv().ok().map(Item::Line);
        }
        match self.rx.recv_timeout(Duration::from_secs(1)) {
            Ok(l) => Some(Item::Line(l)),
            Err(RecvTimeoutError::Timeout) => Some(Item::Idle),
            Err(RecvTimeoutError::Disconnected) => None,
        }
    }
}

impl Drop for Lines {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Source for Journal {
    fn open(&self, after_cursor: Option<&str>, follow: bool) -> Result<Box<dyn Iterator<Item = Item>>, String> {
        // batch-job: the journal follower and the backfill replay run until the journal ends or the unit stops
        let mut cmd = Command::new("journalctl");
        cmd.args(journal_args(after_cursor, follow));
        let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().map_err(|e| format!("journalctl: {e}"))?;
        let out = child.stdout.take().ok_or("journalctl: no stdout")?;
        let (tx, rx) = mpsc::sync_channel::<String>(CHANNEL_DEPTH);
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        Ok(Box::new(Lines { rx, follow, child }))
    }
}

fn run_dir() -> Result<PathBuf, String> {
    Ok(PathBuf::from(spira_config::process::cfg("SPIRA_RUN")?))
}

fn checkpoint_path(run: &Path) -> PathBuf {
    run.join("tsd").join(".unit-run-checkpoint.json")
}

fn load_checkpoint(path: &Path) -> Result<Option<Folder>, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => serde_json::from_str(&t).map(Some).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn save_checkpoint(path: &Path, f: &Folder) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string(f).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn sink<'a>(run: &Path, clock: &'a dyn Clock, proc: &'a dyn Proc) -> Result<Sink<'a>, String> {
    let path = tsd::family_path(run, unit_run::FAMILY);
    let seen = written_ids(&std::fs::read_to_string(&path).unwrap_or_default());
    let scanned = std::fs::metadata(&path).map_or(0, |m| m.len());
    Ok(Sink {
        path,
        host: tsd::default_host(),
        instance: spira_config::process::cfg("SPIRA_INSTANCE")?,
        clock,
        triggers: Triggers::new(proc),
        seen,
        scanned,
    })
}

fn follow() -> Result<(), String> {
    let run = run_dir()?;
    let cp = checkpoint_path(&run);
    std::fs::create_dir_all(cp.parent().ok_or("no tsd dir")?).map_err(|e| e.to_string())?;
    let start = load_checkpoint(&cp)?.unwrap_or_default();
    let mut s = sink(&run, &Wall, &Spawn)?;
    pass(&Journal, start, true, &mut s, &mut |f| save_checkpoint(&cp, f))?;
    Err("journalctl ended".into())
}

fn backfill(force: bool, seed: bool) -> Result<(), String> {
    let run = run_dir()?;
    let cp = checkpoint_path(&run);
    let marker = run.join("tsd").join(".unit-run-backfilled");
    if marker.exists() && !force {
        eprintln!("unit-run backfill: already run; --force replays again");
        return Ok(());
    }
    std::fs::create_dir_all(cp.parent().ok_or("no tsd dir")?).map_err(|e| e.to_string())?;
    let mut s = sink(&run, &Wall, &Spawn)?;
    let (folder, n) = pass(&Journal, Folder::default(), false, &mut s, &mut |_| Ok(()))?;
    if seed && load_checkpoint(&cp)?.is_none() {
        let fresh = std::fs::OpenOptions::new().write(true).create_new(true).open(&cp);
        if fresh.is_ok() {
            save_checkpoint(&cp, &folder)?;
        }
    }
    std::fs::write(&marker, "").map_err(|e| format!("{}: {e}", marker.display()))?;
    eprintln!("unit-run backfill: {n} record(s) written");
    Ok(())
}

fn ensure_accounting(dir: Option<String>) -> Result<(), String> {
    let home = match dir {
        Some(d) => PathBuf::from(d),
        None => std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .ok_or("neither XDG_CONFIG_HOME nor HOME is set")?,
    };
    if ensure_io_accounting(&home)? {
        eprintln!("unit-run: wrote {} — re-executing the user manager to apply it", accounting_dropin_path(&home).display());
        Spawn.run(&["systemctl", "--user", "daemon-reexec"])?;
    }
    if !own_controllers_file().is_some_and(|f| io_controller_available(&f)) {
        eprintln!("unit-run: the io cgroup controller is not delegated to the user manager, so no record can carry io bytes; set Delegate=io in a user@.service drop-in (root) and restart the user manager");
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("follow") => follow(),
        Some("backfill") => backfill(args.iter().any(|a| a == "--force"), true),
        Some("ensure-accounting") => ensure_accounting(args.iter().position(|a| a == "--dir").and_then(|i| args.get(i + 1)).cloned()),
        _ => {
            eprintln!("usage: unit-run <follow|backfill [--force]|ensure-accounting [--dir DIR]>");
            std::process::exit(2);
        }
    };
    if let Err(e) = r {
        eprintln!("unit-run: {e}");
        std::process::exit(1);
    }
}
