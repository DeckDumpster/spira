//! The loop world's test registry: the real registry file and verbs (`test_registry::cli`) under
//! a directory of the run's own, read and written at the world's virtual time. The flip handling,
//! the landing's clearing and the ops pane's rows go through the code those paths run in the
//! service; nothing here models what the registry answers.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use test_registry::{cli, store};

/// The wall-clock second the loop's virtual time zero stands for in the registry.
pub const EPOCH: i64 = 1_800_000_000;
pub const DEFAULT_HOLD_SECS: i64 = 604_800;
const COLUMNS: i64 = 120;

static DIRS: AtomicU64 = AtomicU64::new(0);

pub struct Registry {
    dir: PathBuf,
    path: String,
    hold_secs: String,
}

/// A row the pane shows: test, bead, expiry in epoch seconds.
pub type Held = (String, String, i64);

pub struct Pane {
    pub text: String,
    pub held: Vec<Held>,
}

pub fn epoch_at(at_ms: u64) -> i64 {
    EPOCH + (at_ms / 1000) as i64
}

impl Registry {
    pub fn new(hold_secs: &str) -> Result<Registry, String> {
        let dir = std::env::temp_dir().join(format!("sim-registry-{}-{}", std::process::id(), DIRS.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join("test-registry.sqlite").display().to_string();
        Ok(Registry { dir, path, hold_secs: hold_secs.to_string() })
    }

    pub fn call(&self, at_ms: u64, args: &[&str]) -> cli::Outcome {
        let argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        self.call_owned(at_ms, &argv)
    }

    fn call_owned(&self, at_ms: u64, argv: &[String]) -> cli::Outcome {
        let cfg = |k: &str| match k {
            cli::PATH_KEY => Ok(self.path.clone()),
            cli::HOLD_KEY => Ok(self.hold_secs.clone()),
            other => Err(format!("{other} is not a registry key")),
        };
        cli::run(argv, epoch_at(at_ms), &cfg, &store::open)
    }

    fn said(&self, at_ms: u64, args: &[&str]) -> Result<String, String> {
        let o = self.call(at_ms, args);
        if o.code == 0 {
            Ok(o.stdout)
        } else {
            Err(format!("test-registry {} exited {}: {}{}", args.first().copied().unwrap_or(""), o.code, o.stdout.trim(), o.stderr.trim()))
        }
    }

    pub fn disable(&self, at_ms: u64, test: &str, bead: &str, reason: &str) -> Result<batcher_cut::io::DisabledRow, String> {
        let said = self.said(at_ms, &["disable", "--json", "--test", test, "--bead", bead, "--reason", reason, "--actor", "batcher"])?;
        batcher_cut::io::parse_disabled_row(&said)
    }

    pub fn clear_landed(&self, at_ms: u64, beads: &[String]) -> Result<Vec<String>, String> {
        let mut args = vec!["clear-landed"];
        for b in beads {
            args.extend(["--bead", b]);
        }
        args.extend(["--actor", "batcher"]);
        Ok(self.said(at_ms, &args)?.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect())
    }

    /// The tests the deny list holds out of a run at `at_ms`.
    pub fn denied(&self, at_ms: u64) -> Result<Vec<String>, String> {
        Ok(self.said(at_ms, &["deny-list", "--at", &epoch_at(at_ms).to_string()])?.lines().skip(1).filter_map(|l| l.split('\t').nth(1)).map(String::from).collect())
    }

    /// Whether the registry has ever held a row for `test`.
    pub fn ever_disabled(&self, test: &str) -> Result<bool, String> {
        Ok(self.said(0, &["history", "--test", test])?.lines().any(|l| !l.trim().is_empty()))
    }

    /// The directory a round's pass leaves its deny list in, as the registry answers at `at_ms`.
    pub fn pass_dir(&self, at_ms: u64, name: &str) -> Result<PathBuf, String> {
        let dir = self.dir.join(name);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let list = self.said(at_ms, &["deny-list", "--at", &epoch_at(at_ms).to_string()])?;
        std::fs::write(dir.join(batcher_cut::vm::DenySnapshot::FILE), list).map_err(|e| e.to_string())?;
        Ok(dir)
    }

    /// The ops pane's queue section as the cockpit's collector and the pane draw it at `at_ms`.
    pub fn pane(&self, at_ms: u64, queue_depth: usize) -> Pane {
        let now = epoch_at(at_ms);
        let mut kv = cockpit_collect::probes::queue::testreg_keys(&|args: &[&str]| Some(self.call(at_ms, args).stdout), now);
        kv.push(("SP_QUEUE_DEPTH".to_string(), queue_depth.to_string()));
        let snap = cockpit_ops::health::model::Snapshot::parse(&cockpit_collect::probes::render(&kv));
        let drawn = cockpit_ops::health::sections::queue_section(&snap, COLUMNS).join("\n");
        let text = strip_colour(&drawn);
        let n: usize = snap.get("SP_TESTREG_N").and_then(|v| v.parse().ok()).unwrap_or(0);
        let held = (0..n)
            .map(|i| {
                let f = |k: &str| snap.get(&format!("SP_TESTREG{i}_{k}")).unwrap_or("").to_string();
                (f("TEST"), f("BEAD"), f("UNTIL").parse().unwrap_or(0))
            })
            .collect();
        Pane { text, held }
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn strip_colour(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for e in chars.by_ref() {
                if e == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
