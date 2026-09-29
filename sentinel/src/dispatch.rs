//! The two decoupled workers the full pass starts and never waits on: the audit worker
//! (`spira-audit`, this binary with --audit) and the landing worker (`spira-landing`,
//! landing.sh). The unit name is the mutex; `--collect` keeps a failed unit from blocking
//! every later dispatch; what a worker did is known only from what it wrote — the mailbox
//! (drained by rename) and the status file (the positive control).

use std::path::Path;

use crate::host::{Io, Spec};
use crate::model::status_fields;
use crate::pass::Sentinel;
use crate::seams;

/// The mailbox drain: rename it aside (a worker appending mid-drain lands its lines in the
/// next mailbox), then read EVERY drain file — one a dead pass left behind still holds real
/// movements — each line exactly once.
pub fn drain(run: &Path, mailbox: &Path, stem: &str) -> Vec<String> {
    let mine = run.join(format!("{stem}.drain.{}", std::process::id()));
    if std::fs::metadata(mailbox)
        .map(|m| m.len() > 0)
        .unwrap_or(false)
    {
        let _ = std::fs::rename(mailbox, &mine);
    }
    let prefix = format!("{stem}.drain.");
    let mut files: Vec<_> = std::fs::read_dir(run)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.is_file()
                        && p.file_name()
                            .map(|n| n.to_string_lossy().starts_with(&prefix))
                            .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    let mut lines = Vec::new();
    for f in files {
        if let Ok(t) = std::fs::read_to_string(&f) {
            lines.extend(t.lines().filter(|l| !l.is_empty()).map(str::to_string));
        }
        let _ = std::fs::remove_file(&f);
    }
    lines
}

/// A status file's `KEY=value` fields (digits or '-' only), by suffix.
pub fn status_get<'s>(fields: &'s [(String, String)], key: &str) -> Option<&'s str> {
    fields
        .iter()
        .rev()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

impl<'a> Sentinel<'a> {
    fn unit_active(&self, unit: &str) -> bool {
        let o = self.h.run(
            Spec::args_owned(
                self.cfg.systemctl.clone(),
                vec![
                    "--user".into(),
                    "is-active".into(),
                    format!("{unit}.service"),
                ],
            )
            .err(Io::Null),
        );
        o.stdout.trim() == "active"
    }

    fn setenv(&self, keys: &[&str]) -> Vec<String> {
        keys.iter()
            .map(|k| format!("--setenv={k}={}", self.cfg.raw(k)))
            .collect()
    }

    /// The switch this pass resolved, handed to a worker systemd-run starts with a clean
    /// environment — so the audit worker resolves the same mode, and (OFF) landing.sh is
    /// handed no live path to spira-lc. The audit worker disables its own children itself.
    fn lifecycle_setenv(&self, landing: bool) -> Vec<String> {
        let mut v = vec![format!(
            "--setenv=SPIRA_LIFECYCLE_ENFORCE={}",
            if self.lc == crate::cfg::Lifecycle::On {
                "1"
            } else {
                "0"
            }
        )];
        if landing && self.lc == crate::cfg::Lifecycle::Off {
            v.push(format!("--setenv=SPIRA_LC_BIN={}", crate::cfg::LC_DISABLED));
        }
        v
    }

    fn drain_into_progress(&self, mailbox: &Path, stem: &str) {
        for line in drain(&self.cfg.run, mailbox, stem) {
            self.progress(&line);
        }
    }

    fn read_status(&self, name: &str, prefix: &str) -> Option<Vec<(String, String)>> {
        std::fs::read_to_string(self.cfg.run.join(name))
            .ok()
            .map(|t| status_fields(&t, prefix))
    }

    /// Age from a `.dispatched` stamp when no status file exists yet.
    fn dispatched_age(&self, name: &str) -> Option<i64> {
        let p = self.cfg.run.join(name);
        if !p.is_file() {
            return None;
        }
        let now = self.h.now();
        let at = std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(now);
        Some(now - at)
    }

    fn stamp_dispatched(&self, name: &str) {
        let p = self.cfg.run.join(name);
        if !p.exists() {
            let _ = std::fs::write(p, format!("{}\n", self.h.now()));
        }
    }

    /// CHECK 4/5/6b/7c/7d dispatch — start the audit worker and move on.
    pub fn audit_dispatch(&self) {
        let mailbox = self.cfg.audit_mailbox.clone();
        let unit = self.cfg.audit_unit.clone();
        self.drain_into_progress(&mailbox, "audit.progress");

        let mut age: i64 = -1;
        if let Some(f) = self.read_status("audit.status", "SP_AUDIT_") {
            let at: i64 = status_get(&f, "SP_AUDIT_AT")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            age = self.h.now() - at;
            self.log(&format!(
                "CHECK4/5 audit: last run {age}s ago — rc={}",
                status_get(&f, "SP_AUDIT_RC").unwrap_or("?")
            ));
        }

        if self.unit_active(&unit) {
            self.log("CHECK4/5 audit: already running — this pass does not start another");
        } else {
            let mut a = vec![
                "--user".to_string(),
                "--collect".into(),
                "--quiet".into(),
                format!("--unit={unit}"),
                format!("--property=RuntimeMaxSec={}", self.cfg.audit_maxsec),
                format!("--property=CPUQuota={}%", self.cfg.audit_cpu),
                "--property=Nice=10".into(),
                format!(
                    "--property=StandardOutput=append:{}/audit.log",
                    self.cfg.run.display()
                ),
                format!(
                    "--property=StandardError=append:{}/audit.log",
                    self.cfg.run.display()
                ),
            ];
            a.extend(self.setenv(&[
                "PATH",
                "HOME",
                "SPIRA_HOME",
                "SPIRA_RUN",
                "SPIRA_DB",
                "SPIRA_REPO",
                "SPIRA_REPO_MAP",
            ]));
            a.push(format!("--setenv=SPIRA_HOME_REPO={}", self.cfg.home_repo));
            a.push(format!("--setenv=SPIRA_BD={}", self.cfg.bd));
            a.push(format!(
                "--setenv=SPIRA_GH={}",
                if self.cfg.raw("SPIRA_GH").is_empty() {
                    "gh"
                } else {
                    self.cfg.raw("SPIRA_GH")
                }
            ));
            a.push(format!("--setenv=SPIRA_POISON_AT={}", self.cfg.poison_at));
            a.push(format!("--setenv=SPIRA_REQUEUE_AT={}", self.cfg.requeue_at));
            a.push(format!("--setenv=SPIRA_RECLAIM_AT={}", self.cfg.reclaim_at));
            a.extend(self.setenv(&[
                "SPIRA_ASK_LABEL",
                "SPIRA_SCOPE_LABEL",
                "SPIRA_WORK_CLOSE_TYPES",
            ]));
            // B1: the switches the operator sets on the sentinel's unit reach the worker
            // that actually runs the checks they switch off.
            a.extend(self.lifecycle_setenv(false));
            for k in ["SPIRA_SKIP_CLOSED_CHECK", "SPIRA_SKIP_RECLAIM"] {
                if !self.cfg.raw(k).is_empty() {
                    a.push(format!("--setenv={k}={}", self.cfg.raw(k)));
                }
            }
            a.push(self.exe.clone());
            a.push("--audit".into());
            let o = self.h.run(
                Spec::args_owned(self.cfg.launch.clone(), a)
                    .out(Io::Inherit)
                    .err(Io::Null),
            );
            if o.ok() {
                self.log(&format!("CHECK4/5 audit: dispatched as {unit}"));
                self.stamp_dispatched("audit.dispatched");
            } else if self.unit_active(&unit) {
                self.log("CHECK4/5 audit: started underneath this pass — not starting another");
            } else {
                self.log("CHECK4/5 audit WARN: could not dispatch the audit worker; poison, closed-not-landed, sending and collision checks will not run until this is fixed");
            }
        }

        if age < 0 {
            if let Some(a) = self.dispatched_age("audit.dispatched") {
                age = a;
            }
        }
        if !self.unit_active(&unit) && age > self.cfg.audit_stale {
            self.log(&format!(
                "CHECK4/5 audit WARN: no audit pass has completed in {age}s and none is running"
            ));
        }
        self.drain_into_progress(&mailbox, "audit.progress");
    }

    /// lib.sh `land_log_tail 30`.
    fn land_log_tail(&self) -> String {
        match std::fs::read_to_string(self.cfg.run.join("landing.log")) {
            Ok(t) => {
                let lines: Vec<&str> = t.lines().collect();
                lines[lines.len().saturating_sub(30)..].join("\n")
            }
            Err(_) => "(no landing log — the worker has never written one)".into(),
        }
    }

    /// `tr '\n' ' ' < landing.status`.
    fn status_flat(&self) -> Option<String> {
        std::fs::read_to_string(self.cfg.run.join("landing.status"))
            .ok()
            .map(|t| t.replace('\n', " "))
    }

    /// S3 — land_escalate <why> <evidence>, both on stdin.
    fn land_escalate(&self, why: &str, evidence: &str) {
        let ev = evidence.trim_end_matches('\n');
        self.seam(
            "land-escalate",
            seams::LAND_ESCALATE,
            Some(format!("{why}\n{ev}").into_bytes()),
            Io::Inherit,
            Io::Inherit,
            true,
        );
    }

    /// CHECK 6 — dispatch landing.sh; read what the previous run left behind.
    pub fn check6(&self) {
        let unit = self.cfg.land_unit.clone();
        let mailbox = self.cfg.run.join("landing.progress");
        self.drain_into_progress(&mailbox, "landing.progress");

        let wt = self.script("watchtower.sh");
        if std::fs::File::open(&wt).is_ok() {
            for flag in [
                "--throttle-check",
                "--czar-outcome-check",
                "--pr-stall-check",
                "--disabled-timer-check",
            ] {
                self.h.run(
                    Spec::args_owned("bash", vec![wt.to_string_lossy().into_owned(), flag.into()])
                        .out(Io::Inherit)
                        .err(Io::Null),
                );
            }
        }

        let mut age: i64 = -1;
        if let Some(f) = self.read_status("landing.status", "SP_LAND_") {
            let at: i64 = status_get(&f, "SP_LAND_AT")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            age = self.h.now() - at;
            let g = |k: &str| status_get(&f, k).unwrap_or("?").to_string();
            self.log(&format!(
                "CHECK6: last landing {age}s ago — rc={}, {} branch(es) seen, {} moved",
                g("SP_LAND_RC"),
                g("SP_LAND_BRANCHES"),
                g("SP_LAND_MOVED")
            ));
            let rc = status_get(&f, "SP_LAND_RC").unwrap_or("0");
            if rc != "0" {
                self.log(&format!(
                    "CHECK6 WARN: the last landing exited {rc} — see {}/landing.log",
                    self.cfg.run.display()
                ));
                self.land_escalate(
                    &format!("its last run exited {rc}"),
                    &format!(
                        "STATUS  {}\n\n--- landing.log (tail) ---\n{}\n",
                        self.status_flat().unwrap_or_default(),
                        self.land_log_tail()
                    ),
                );
            }
        } else {
            self.log("CHECK6: no landing has ever completed on this host");
        }

        if self.unit_active(&unit) {
            self.log("CHECK6: a landing is already in flight — this pass does not start another");
        } else {
            let mut a = vec![
                "--user".to_string(),
                "--collect".into(),
                "--quiet".into(),
                format!("--unit={unit}"),
                format!("--property=RuntimeMaxSec={}", self.cfg.land_maxsec),
                format!("--property=CPUQuota={}%", self.cfg.land_cpu),
                "--property=Nice=10".into(),
                format!(
                    "--property=StandardOutput=append:{}/landing.log",
                    self.cfg.run.display()
                ),
                format!(
                    "--property=StandardError=append:{}/landing.log",
                    self.cfg.run.display()
                ),
            ];
            a.extend(self.setenv(&[
                "PATH",
                "HOME",
                "SPIRA_HOME",
                "SPIRA_RUN",
                "SPIRA_DB",
                "SPIRA_REPO",
                "SPIRA_REPO_MAP",
            ]));
            a.push(format!("--setenv=SPIRA_HOME_REPO={}", self.cfg.home_repo));
            a.push(format!("--setenv=SPIRA_BD={}", self.cfg.bd));
            a.push(format!(
                "--setenv=SPIRA_GH={}",
                if self.cfg.raw("SPIRA_GH").is_empty() {
                    "gh"
                } else {
                    self.cfg.raw("SPIRA_GH")
                }
            ));
            a.extend(self.setenv(&["SPIRA_BATCH_MAXPAR"]));
            a.push(format!(
                "--setenv=SPIRA_LAND_MAXSEC={}",
                self.cfg.land_maxsec
            ));
            a.extend(self.lifecycle_setenv(true));
            a.push(self.script("landing.sh").to_string_lossy().into_owned());
            let o = self.h.run(
                Spec::args_owned(self.cfg.launch.clone(), a)
                    .out(Io::Inherit)
                    .err(Io::Null),
            );
            if o.ok() {
                self.log(&format!("CHECK6: landing dispatched as {unit}"));
                self.stamp_dispatched("landing.dispatched");
            } else if self.unit_active(&unit) {
                self.log("CHECK6: a landing started underneath this pass — not starting another");
            } else {
                self.log("CHECK6 WARN: could not dispatch the landing worker; nothing will land until this is fixed");
                self.land_escalate(
                    "the landing worker will not start",
                    &format!("systemd-run --unit={unit} refused, and the unit is not active.\n\n--- landing.log (tail) ---\n{}\n", self.land_log_tail()),
                );
            }
        }

        if age < 0 {
            if let Some(a) = self.dispatched_age("landing.dispatched") {
                age = a;
            }
        }
        if !self.unit_active(&unit) && age > self.cfg.land_stale {
            self.log(&format!(
                "CHECK6 WARN: no landing has completed in {age}s and none is running"
            ));
            self.land_escalate(
                &format!("nothing has completed a landing pass in {age}s"),
                &format!(
                    "STATUS  {}\n\n--- landing.log (tail) ---\n{}\n",
                    self.status_flat()
                        .unwrap_or_else(|| "none — no run has ever written one".into()),
                    self.land_log_tail()
                ),
            );
        }
        self.drain_into_progress(&mailbox, "landing.progress");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_reads_every_drain_file_once() {
        let d = std::env::temp_dir().join(format!("sentinel-drain-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mb = d.join("audit.progress");
        std::fs::write(&mb, "poisoned x\n\nreclaimed 1\n").unwrap();
        std::fs::write(d.join("audit.progress.drain.999999"), "left behind\n").unwrap();
        let l = drain(&d, &mb, "audit.progress");
        assert_eq!(l.len(), 3);
        assert!(l.contains(&"left behind".to_string()) && l.contains(&"poisoned x".to_string()));
        assert!(!mb.exists());
        assert!(
            drain(&d, &mb, "audit.progress").is_empty(),
            "each line exactly once"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
