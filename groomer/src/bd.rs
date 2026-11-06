//! The bead store, through `bdq <verb> …`. A trait
//! so every subcommand's argument-construction is testable against a recording fake
//! without a live Dolt server (law-gates-run-in-a-clean-environment) — the same split
//! `rebase-stale::seam::Seam` uses for the lib.sh boundary.

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
    /// Whether the lifecycle machine holds the bead in a terminal state; Err when it has no row.
    fn lifecycle_terminal(&self, id: &str) -> Result<bool, String>;
    /// Close `<id>` superseded by `<successor>`, through the lifecycle machine.
    fn supersede(&self, id: &str, with: &str) -> Result<(), String>;
    /// `dep add <bug-id> <fix-id>`.
    fn dep_add(&self, bug_id: &str, fix_id: &str) -> Result<(), String>;
}

/// Shells to `bdq …`, which addresses the configured database itself.
pub struct RealBd {
    pub bdq: String,
}

impl RealBd {
    fn cmd(&self) -> Command {
        // FORK-EDGE: label list, set-state and dep add — the content door serves none of them
        Command::new(&self.bdq)
    }

    /// Captures stdout — for a call whose value this needs to read.
    fn run(&self, args: &[&str]) -> Result<std::process::Output, String> {
        self.cmd()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("{} {}: {e}", self.bdq, args.join(" ")))
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
            .map_err(|e| format!("{} {}: {e}", self.bdq, args.join(" ")))
    }

    /// One call through the store's content door, which applies every protection bdq applies
    /// and writes a created bead's lifecycle row in the same request: its stdout when it applied.
    fn content(&self, argv: &[&str], body: Option<&str>) -> Result<String, String> {
        let lc = spira_config::lc_call::LcClient::configured()?;
        let argv: Vec<&str> = std::iter::once("content").chain(argv.iter().copied()).collect();
        let r = match body {
            Some(b) => lc.call_stdin(&argv, b),
            None => lc.call(&argv),
        }
        .map_err(|e| format!("{}: {e}", argv.join(" ")))?;
        if r.code != 0 {
            return Err(format!("{} exited {}: {}", argv[1], r.code, r.why()));
        }
        Ok(r.out)
    }

    fn ok(status: std::process::ExitStatus, what: &str) -> Result<(), String> {
        if status.success() {
            Ok(())
        } else {
            Err(format!("{what} exited {}", status.code().unwrap_or(-1)))
        }
    }
}

/// `create [<title>] --parent <parent> --silent <flags>`: a split piece's create, its positional
/// title first, where the content door reads a title.
pub fn create_child_argv<'a>(parent: &'a str, extra: &'a [String]) -> Vec<&'a str> {
    const VALUES: &[&str] = &["--title", "-d", "--description", "-p", "--priority", "-t", "--type", "-l", "--label", "--labels"];
    let (mut title, mut flags) = (None, Vec::new());
    let mut it = extra.iter().map(String::as_str);
    while let Some(a) = it.next() {
        if VALUES.contains(&a) {
            flags.push(a);
            flags.extend(it.next());
        } else if !a.starts_with('-') && title.is_none() {
            title = Some(a);
        } else {
            flags.push(a);
        }
    }
    let mut argv = vec!["create"];
    argv.extend(title);
    argv.extend(["--parent", parent, "--silent"]);
    argv.extend(flags);
    argv
}

impl Bd for RealBd {
    fn label_add(&self, id: &str, label: &str) -> Result<(), String> {
        self.content(&["label", "add", id, label], None).map(drop)
    }

    fn label_remove(&self, id: &str, label: &str) -> Result<(), String> {
        self.content(&["label", "remove", id, label], None).map(drop)
    }

    fn label_list(&self, id: &str) -> Result<String, String> {
        let o = self.run(&["label", "list", id])?;
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    }

    fn close(&self, id: &str, reason: &str) -> Result<(), String> {
        // Through the lifecycle machine (sp-3fue0j), never a raw bd close.
        spira_config::lifecycle_row::close(id, reason, "groomer", None)
    }

    fn note(&self, id: &str, text: &str) -> Result<(), String> {
        self.content(&["note", id, "--stdin"], Some(text)).map(drop)
    }

    fn create_child(&self, parent: &str, extra: &[String]) -> Result<String, String> {
        let out = self.content(&create_child_argv(parent, extra), None).map_err(|e| format!("bd create failed: {e}"))?;
        let id = out.trim().to_string();
        if id.is_empty() {
            return Err("bd create returned no id".into());
        }
        Ok(id)
    }

    fn show_json(&self, id: &str) -> Result<serde_json::Value, String> {
        let out = self.content(&["show", id, "--json"], None).map_err(|e| format!("show {id} failed: {e}"))?;
        serde_json::from_str(&out).map_err(|e| format!("show {id}: {e}"))
    }

    fn set_state(&self, id: &str, kv: &str) -> Result<(), String> {
        Self::ok(self.run_inherit(&["set-state", id, kv])?, "set-state")
    }

    fn supersede(&self, id: &str, with: &str) -> Result<(), String> {
        spira_config::lifecycle_row::close(id, &format!("superseded by {with}"), "groomer", Some(with))
    }

    fn lifecycle_terminal(&self, id: &str) -> Result<bool, String> {
        match spira_config::lc_state::row(id)? {
            Some(r) => Ok(r.terminal()),
            None => Err(format!("no lifecycle row for {id}")),
        }
    }

    fn dep_add(&self, bug_id: &str, fix_id: &str) -> Result<(), String> {
        Self::ok(self.run_inherit(&["dep", "add", bug_id, fix_id])?, "dep add")
    }
}

#[cfg(test)]
mod argv_tests {
    #[test]
    fn a_split_pieces_title_comes_first_whatever_its_place_among_the_flags() {
        let extra: Vec<String> = ["-p", "2", "Piece two", "-l", "a,b", "--description", "-- not a flag"].iter().map(|s| s.to_string()).collect();
        assert_eq!(super::create_child_argv("sp-o", &extra), ["create", "Piece two", "--parent", "sp-o", "--silent", "-p", "2", "-l", "a,b", "--description", "-- not a flag"]);
        let flags: Vec<String> = ["--title", "T"].iter().map(|s| s.to_string()).collect();
        assert_eq!(super::create_child_argv("sp-o", &flags), ["create", "--parent", "sp-o", "--silent", "--title", "T"]);
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
        pub states: RefCell<std::collections::BTreeMap<String, String>>,
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

        pub fn set_lifecycle(&self, id: &str, state: &str) {
            self.states.borrow_mut().insert(id.to_string(), state.to_string());
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

        fn lifecycle_terminal(&self, id: &str) -> Result<bool, String> {
            self.states.borrow().get(id).map(|s| spira_config::lc_state::is_terminal(s)).ok_or_else(|| format!("no lifecycle row for {id}"))
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
