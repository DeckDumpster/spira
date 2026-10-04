//! The bead store, as `groomer.sh` called it: `$SPIRA_BD -C $SPIRA_DB <verb> …`. A trait
//! so every subcommand's argument-construction is testable against a recording fake
//! without a live Dolt server (law-gates-run-in-a-clean-environment) — the same split
//! `rebase-stale::seam::Seam` uses for the lib.sh boundary.

use std::io::Write;
use std::process::{Command, Stdio};

pub trait Bd {
    /// `label add <id> <label>`.
    fn label_add(&self, id: &str, label: &str) -> Result<(), String>;
    /// `label remove <id> <label>` — `label` may be a comma-joined list; bd accepts that
    /// as a single argument the same way the bash this replaces passed it.
    fn label_remove(&self, id: &str, label: &str) -> Result<(), String>;
    /// `label list <id>`, raw stdout (one label per line, as bd prints it).
    fn label_list(&self, id: &str) -> Result<String, String>;
    /// `close <id> --reason-file -`, reason on stdin (bd does not read stdin for a bare
    /// `--reason`; `--reason-file -` is required to avoid storing a literal dash).
    fn close(&self, id: &str, reason: &str) -> Result<(), String>;
    /// `note <id> "<text>"` — text as an argument, matching every `note` call
    /// `groomer.sh` made (only `close`'s reason goes on stdin).
    fn note(&self, id: &str, text: &str) -> Result<(), String>;
    /// `create --parent <id> --silent <extra args>`, returns the new bead id.
    fn create_child(&self, parent: &str, extra: &[String]) -> Result<String, String>;
    /// `show <id> --json`, parsed.
    fn show_json(&self, id: &str) -> Result<serde_json::Value, String>;
    /// `set-state <id> <key>=<value>`.
    fn set_state(&self, id: &str, kv: &str) -> Result<(), String>;
    /// `supersede <id> --with <successor>`.
    fn supersede(&self, id: &str, with: &str) -> Result<(), String>;
    /// `dep add <bug-id> <fix-id>`.
    fn dep_add(&self, bug_id: &str, fix_id: &str) -> Result<(), String>;
}

/// Shells to `$SPIRA_BD -C $SPIRA_DB …`, exactly as `groomer.sh` did.
pub struct RealBd {
    pub bd: String,
    pub db: String,
}

impl RealBd {
    fn cmd(&self) -> Command {
        let mut c = Command::new(&self.bd);
        c.arg("-C").arg(&self.db);
        c
    }

    /// Captures stdout — for a call whose value this needs to read.
    fn run(&self, args: &[&str]) -> Result<std::process::Output, String> {
        self.cmd()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("{} {}: {e}", self.bd, args.join(" ")))
    }

    /// Discards bd's own stdout (a write verb's confirmation chatter), keeping stderr
    /// visible so a real failure still surfaces. `groomer.sh` redirected most of these
    /// write calls to `/dev/null` for exactly this reason: `split-piece`'s own stdout
    /// contract is the new id alone, and nothing here should leak bd's own text into a
    /// caller's command substitution — `split-piece`'s `set-state`/`label remove` were
    /// the two call sites the original script was explicit about; the rest (label
    /// add/remove, note, supersede, dep add) never had a test that needed their real-bd
    /// stdout visible either, so the same treatment is both safe and consistent.
    fn run_inherit(&self, args: &[&str]) -> Result<std::process::ExitStatus, String> {
        self.cmd()
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .map_err(|e| format!("{} {}: {e}", self.bd, args.join(" ")))
    }

    fn run_stdin(&self, args: &[&str], payload: &str) -> Result<std::process::ExitStatus, String> {
        let mut child = self
            .cmd()
            .args(args)
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{} {}: {e}", self.bd, args.join(" ")))?;
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(payload.as_bytes());
        }
        child.wait().map_err(|e| format!("{} {}: {e}", self.bd, args.join(" ")))
    }

    fn ok(status: std::process::ExitStatus, what: &str) -> Result<(), String> {
        if status.success() {
            Ok(())
        } else {
            Err(format!("{what} exited {}", status.code().unwrap_or(-1)))
        }
    }
}

impl Bd for RealBd {
    fn label_add(&self, id: &str, label: &str) -> Result<(), String> {
        Self::ok(self.run_inherit(&["label", "add", id, label])?, "label add")
    }

    fn label_remove(&self, id: &str, label: &str) -> Result<(), String> {
        Self::ok(self.run_inherit(&["label", "remove", id, label])?, "label remove")
    }

    fn label_list(&self, id: &str) -> Result<String, String> {
        let o = self.run(&["label", "list", id])?;
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    }

    fn close(&self, id: &str, reason: &str) -> Result<(), String> {
        Self::ok(self.run_stdin(&["close", id, "--reason-file", "-"], reason)?, "close")
    }

    fn note(&self, id: &str, text: &str) -> Result<(), String> {
        Self::ok(self.run_inherit(&["note", id, text])?, "note")
    }

    fn create_child(&self, parent: &str, extra: &[String]) -> Result<String, String> {
        let mut args: Vec<&str> = vec!["create", "--parent", parent, "--silent"];
        args.extend(extra.iter().map(|s| s.as_str()));
        let o = self.run(&args)?;
        if !o.status.success() {
            return Err(format!("bd create failed: {}", String::from_utf8_lossy(&o.stderr)));
        }
        let id = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if id.is_empty() {
            return Err("bd create returned no id".into());
        }
        if let Err(e) = spira_config::lifecycle_row::after_create("groomer", &id) {
            eprintln!("groomer: LIFECYCLE: row not written after create: {e}; the new bead is rowless and cannot be claimed");
        }
        Ok(id)
    }

    fn show_json(&self, id: &str) -> Result<serde_json::Value, String> {
        let o = self.run(&["show", id, "--json"])?;
        if !o.status.success() {
            return Err(format!("show {id} failed: {}", String::from_utf8_lossy(&o.stderr)));
        }
        serde_json::from_slice(&o.stdout).map_err(|e| format!("show {id}: {e}"))
    }

    fn set_state(&self, id: &str, kv: &str) -> Result<(), String> {
        Self::ok(self.run_inherit(&["set-state", id, kv])?, "set-state")
    }

    fn supersede(&self, id: &str, with: &str) -> Result<(), String> {
        Self::ok(self.run_inherit(&["supersede", id, "--with", with])?, "supersede")
    }

    fn dep_add(&self, bug_id: &str, fix_id: &str) -> Result<(), String> {
        Self::ok(self.run_inherit(&["dep", "add", bug_id, fix_id])?, "dep add")
    }
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::cell::RefCell;

    /// Records every call in order, for a test to assert against — the same shape as the
    /// bash suites' `STUB_BD` argv log, without a subprocess.
    #[derive(Default)]
    pub struct FakeBd {
        pub calls: RefCell<Vec<String>>,
        pub labels: RefCell<std::collections::BTreeMap<String, Vec<String>>>,
        pub shows: RefCell<std::collections::BTreeMap<String, serde_json::Value>>,
        pub next_child_id: RefCell<Option<String>>,
        pub fail_create: RefCell<bool>,
    }

    impl FakeBd {
        pub fn new() -> FakeBd {
            FakeBd::default()
        }

        pub fn log(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }

        pub fn set_labels(&self, id: &str, labels: &[&str]) {
            self.labels.borrow_mut().insert(id.to_string(), labels.iter().map(|s| s.to_string()).collect());
        }

        pub fn set_show(&self, id: &str, v: serde_json::Value) {
            self.shows.borrow_mut().insert(id.to_string(), v);
        }
    }

    impl Bd for FakeBd {
        fn label_add(&self, id: &str, label: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("label add {id} {label}"));
            self.labels.borrow_mut().entry(id.to_string()).or_default().push(label.to_string());
            Ok(())
        }

        fn label_remove(&self, id: &str, label: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("label remove {id} {label}"));
            if let Some(v) = self.labels.borrow_mut().get_mut(id) {
                v.retain(|l| l != label);
            }
            Ok(())
        }

        fn label_list(&self, id: &str) -> Result<String, String> {
            self.calls.borrow_mut().push(format!("label list {id}"));
            Ok(self
                .labels
                .borrow()
                .get(id)
                .map(|v| v.iter().map(|l| format!("  - {l}\n")).collect::<String>())
                .unwrap_or_default())
        }

        fn close(&self, id: &str, reason: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("close {id} {reason}"));
            Ok(())
        }

        fn note(&self, id: &str, text: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("note {id} {text}"));
            Ok(())
        }

        fn create_child(&self, parent: &str, extra: &[String]) -> Result<String, String> {
            if *self.fail_create.borrow() {
                return Err("bd create failed".into());
            }
            let id = self.next_child_id.borrow().clone().unwrap_or_else(|| format!("{parent}.1"));
            self.calls.borrow_mut().push(format!("create --parent {parent} --silent {}", extra.join(" ")));
            Ok(id)
        }

        fn show_json(&self, id: &str) -> Result<serde_json::Value, String> {
            self.calls.borrow_mut().push(format!("show {id}"));
            self.shows.borrow().get(id).cloned().ok_or_else(|| format!("no fixture for show {id}"))
        }

        fn set_state(&self, id: &str, kv: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("set-state {id} {kv}"));
            Ok(())
        }

        fn supersede(&self, id: &str, with: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("supersede {id} --with {with}"));
            Ok(())
        }

        fn dep_add(&self, bug_id: &str, fix_id: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("dep add {bug_id} {fix_id}"));
            Ok(())
        }
    }
}
