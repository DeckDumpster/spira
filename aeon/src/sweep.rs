//! Sweep mode: the persona without a bead (spira-ops.service). Capacity, draining and the
//! account are checked; the ledger is written; no claim, no worktree, no verdict.

use crate::brief;
use crate::decide;
use crate::ledger;
use crate::ports::s;
use crate::run::Run;
use crate::session::SessionSpec;

impl Run<'_> {
    pub fn sweep(&mut self, prompt: Option<String>) -> i32 {
        let f = self.fayth.name.clone();
        let own = self.own_unit.clone();
        let have: i64 = self.sv("aeon_count", &s(&[&f, &own])).text().trim().parse().unwrap_or(0);
        let max = self.fayth.max_concurrent as i64;
        if have >= max {
            self.log(&format!("{f}: at capacity ({have}/{max}), not sweeping"));
            self.ledger.awake(self.now(), &f, "capacity");
            return 0;
        }
        if self.run_dir().join("world.draining").is_file() {
            self.log(&format!("{f}: draining — not sweeping (world.sh resume to lift)"));
            self.ledger.awake(self.now(), &f, "draining");
            return 0;
        }
        match self.capacity_check().state {
            crate::capacity::Paused::Open => {}
            crate::capacity::Paused::Paused(left) => {
                self.log(&format!("{f}: the account is out of capacity for another {left}s — not sweeping"));
                self.ledger.awake(self.now(), &f, "paused");
                return 0;
            }
            crate::capacity::Paused::Unknown => {
                self.log(&format!("{f}: the account's capacity pause file could not be read — not sweeping (failing closed)"));
                self.ledger.awake(self.now(), &f, "paused");
                return 0;
            }
        }
        // In-process (wave 4.23, sp-0ffox) — see run.rs::take_name's own comment.
        let name = crate::naming::aeon_name_take(self.run_dir(), &f);
        let actor = format!("aeon-{name}");
        for (k, v) in [
            ("SPIRA_AEON", name.clone()),
            ("BEADS_ACTOR", actor.clone()),
            ("GIT_AUTHOR_NAME", actor.clone()),
            ("GIT_AUTHOR_EMAIL", format!("{actor}@spira.local")),
            ("GIT_COMMITTER_NAME", actor.clone()),
            ("GIT_COMMITTER_EMAIL", format!("{actor}@spira.local")),
        ] {
            self.d.env.set(k, &v);
        }
        let run = self.run_dir().to_path_buf();
        let pidfile = run.join(format!("aeon-{f}-sweep-{}.pid", self.pid));
        let _ = std::fs::write(pidfile.with_extension("name"), &name);
        let _ = std::fs::write(&pidfile, format!("{}\n", self.pid));
        self.s.bead = "sweep".into();
        self.ledger.awake(self.now(), &f, "sweep");
        let logf = run.join(format!("sweep-{f}-{}.log", self.pid));
        self.log(&format!("{f}: sweeping (log: {})", logf.display()));

        let statutes = match self.statutes() {
            Ok(s) => s,
            Err(m) => {
                self.log(&m);
                self.ledger.awake(self.now(), &f, "statute-core-missing");
                return 1;
            }
        };
        let prompt = match prompt.filter(|p| !p.is_empty()) {
            Some(p) => p,
            None => std::fs::read_to_string(self.home().join("chamber").join(format!("{f}.md"))).unwrap_or_default().trim_end_matches('\n').to_string(),
        };
        let sys_file = run.join(format!("sweep-{f}-{}.system.md", self.pid));
        let task_file = run.join(format!("sweep-{f}-{}.task.md", self.pid));
        let (sys, task) = brief::split(&statutes, &prompt);
        let _ = std::fs::write(&sys_file, sys);
        let _ = std::fs::write(&task_file, task);
        let argv = self.claude_argv(&sys_file);
        let spec = SessionSpec {
            prog: self.conf.agent(),
            args: argv,
            stdin_file: task_file,
            log: logf.clone(),
            cwd: std::env::current_dir().unwrap_or_else(|_| run.clone()),
            env: self.d.env.child(),
            timeout: self.fayth.timeout_seconds,
        };
        let rc = self.d.launcher.run(&spec, &self.stop);
        let _ = std::fs::remove_file(&pidfile);
        let _ = std::fs::remove_file(pidfile.with_extension("name"));
        let fields = ledger::session_result_fields(Some(&logf), &self.conf.trace_mark());
        self.ledger.done(self.now(), &f, "sweep", rc, "sweep", &fields);
        // A sweep that ran and did work succeeded even if the CLI exited 1; a refused one
        // keeps its rc so the named unit enters FAILED.
        let seg = ledger::trace_segment(Some(&logf), 0, &self.conf.trace_mark());
        match decide::session_outcome(seg.as_deref()) {
            "unlanded" | "killed" => 0,
            _ => rc,
        }
    }
}
