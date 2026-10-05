//! Standing conditions: a probe reports what is true now; this turns that into one incident
//! bead per condition while it stands, and closes the bead when it stops standing.
//!
//! A condition that stays crossed files nothing further (an open bead for its reference
//! already holds it). One that clears has its bead closed. A probe that could not read
//! reports [`Reading::Unknown`], which neither files nor clears.

use crate::incident::{self, Finding};
use crate::log::log;
use spira_config::nonwork::{self, Kind, Which};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};


#[derive(Debug, Clone, PartialEq)]
pub struct Cond {
    pub key: String,
    pub reference: String,
    pub title: String,
    pub body: String,
    pub priority: u32,
    pub sustain_secs: i64,
}

pub enum Reading {
    Unknown,
    Standing(Vec<Cond>),
}

pub struct Ctx<'a> {
    pub run: &'a Path,
    pub db: &'a str,
    pub home_repo: &'a str,
    pub incident_sh: &'a str,
    pub bd: &'a str,
}

fn safe(key: &str) -> String {
    key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect()
}

fn dir(ctx: &Ctx, kind: &str, probe: &str) -> PathBuf {
    ctx.run.join("conditions").join(kind).join(safe(probe))
}

fn names(d: &Path) -> BTreeSet<String> {
    std::fs::read_dir(d).map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default()
}

/// Ids of the unfinished beads holding `reference`; `None` when the store did not answer.
fn open_beads(ctx: &Ctx, reference: &str) -> Option<Vec<String>> {
    let out = crate::deadline::output(
        "open beads by ref",
        // Condition beads are incident records, not work: bd status is their only state.
        Command::new(ctx.bd)
            .args(["-C", ctx.db, "list", "--external-ref", reference])
            .args(nonwork::status_args(Kind::Incident, Which::Live))
            .args(["--json", "--limit", "0", "--brief"]),
    )
    .ok()
    .filter(|o| o.status.success())?;
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).ok()?;
    Some(rows.iter().filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(String::from)).collect())
}

fn close_bead(ctx: &Ctx, id: &str, why: &str) -> bool {
    use std::io::Write;
    let child = Command::new(ctx.bd)
        .args(["-C", ctx.db, "close", id, "--reason-file", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return false };
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(format!("OUTCOME: delivered\n{why}\n").as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

pub fn reconcile(now: i64, ctx: &Ctx, probe: &str, reading: Reading) {
    let Reading::Standing(conds) = reading else {
        log(&format!("watchtower: conditions: {probe} could not read — nothing filed or cleared"));
        return;
    };
    let filed_dir = dir(ctx, "filed", probe);
    let pending_dir = dir(ctx, "pending", probe);
    let _ = std::fs::create_dir_all(&filed_dir);
    let _ = std::fs::create_dir_all(&pending_dir);
    let standing: BTreeSet<String> = conds.iter().map(|c| safe(&c.key)).collect();

    for c in &conds {
        let k = safe(&c.key);
        let stamp = pending_dir.join(&k);
        let since = std::fs::read_to_string(&stamp).ok().and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or_else(|| {
            let _ = std::fs::write(&stamp, now.to_string());
            now
        });
        if now - since < c.sustain_secs {
            continue;
        }
        match open_beads(ctx, &c.reference) {
            Some(ids) if ids.is_empty() => {
                if !incident::is_usable(ctx.incident_sh) {
                    log(&format!("watchtower: conditions: {} is missing — {} not filed", ctx.incident_sh, c.key));
                    continue;
                }
                let f = Finding::new(ctx.db, ctx.home_repo, &c.title, &c.body).priority(c.priority).reference(&c.reference).cause(probe);
                if incident::file(ctx.incident_sh, &f) {
                    let _ = std::fs::write(filed_dir.join(&k), &c.reference);
                    log(&format!("watchtower: conditions: filed {} ({probe})", c.reference));
                }
            }
            _ => {}
        }
    }

    for k in names(&pending_dir).difference(&standing) {
        let _ = std::fs::remove_file(pending_dir.join(k));
    }
    for k in names(&filed_dir).difference(&standing) {
        let marker = filed_dir.join(k);
        let Ok(reference) = std::fs::read_to_string(&marker) else { continue };
        let Some(ids) = open_beads(ctx, reference.trim()) else { continue };
        let all_closed = ids.iter().all(|id| close_bead(ctx, id, &format!("The condition {} no longer holds; watchtower resolved this incident.", reference.trim())));
        if all_closed {
            let _ = std::fs::remove_file(&marker);
            log(&format!("watchtower: conditions: cleared {} ({probe})", reference.trim()));
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub struct Fixture {
        pub dir: testkit::TempDir,
    }

    impl Fixture {
        pub fn new(name: &str) -> Self {
            let dir = testkit::TempDir::new(name);
            std::fs::create_dir_all(dir.join("run")).unwrap();
            let bd = "#!/bin/bash\nS=\"$(dirname \"$0\")/store\"\ntouch \"$S\"\ncase \"$3\" in\nlist) ref=\"$5\"; ids=$(awk -v r=\"$ref\" '$2==r{print $1}' \"$S\"); printf '['; s=''; for i in $ids; do printf '%s{\"id\":\"%s\"}' \"$s\" \"$i\"; s=','; done; printf ']\\n';;\nclose) id=\"$4\"; cat > /dev/null; grep -v \"^$id \" \"$S\" > \"$S.n\"; mv \"$S.n\" \"$S\"; echo \"$id\" >> \"$(dirname \"$0\")/closed\";;\nesac\n";
            let inc = "#!/bin/bash\nD=\"$(dirname \"$0\")\"\ncat > /dev/null\nn=$(wc -l < \"$D/store\"); echo \"b$((n+1)) $SPIRA_INCIDENT_REF\" >> \"$D/store\"; echo \"$2\" >> \"$D/filed\"\n";
            for (n, body) in [("bd", bd), ("inc.sh", inc)] {
                let p = dir.join(n);
                testkit::write_exe(&p, body);
            }
            std::fs::write(dir.join("store"), "").unwrap();
            Fixture { dir }
        }
        pub fn ctx(&self) -> Ctx<'_> {
            let run: &'static Path = Box::leak(self.dir.join("run").into_boxed_path());
            let leak = |p: PathBuf| -> &'static str { Box::leak(p.to_string_lossy().into_owned().into_boxed_str()) };
            Ctx { run, db: "db", home_repo: "harness", incident_sh: leak(self.dir.join("inc.sh")), bd: leak(self.dir.join("bd")) }
        }
        pub fn filed(&self) -> usize {
            std::fs::read_to_string(self.dir.join("filed")).map(|s| s.lines().count()).unwrap_or(0)
        }
        pub fn open(&self) -> usize {
            std::fs::read_to_string(self.dir.join("store")).map(|s| s.lines().count()).unwrap_or(0)
        }
    }

    pub fn cond(key: &str, sustain: i64) -> Cond {
        Cond { key: key.into(), reference: format!("incident:test:{key}"), title: format!("T {key}"), body: "b".into(), priority: 1, sustain_secs: sustain }
    }

    #[test]
    fn crossing_files_one_bead_staying_crossed_files_no_second_and_clearing_closes_it() {
        let fx = Fixture::new("cond-lifecycle");
        let ctx = fx.ctx();
        reconcile(100, &ctx, "p", Reading::Standing(vec![cond("a", 0)]));
        assert_eq!((fx.filed(), fx.open()), (1, 1));
        reconcile(160, &ctx, "p", Reading::Standing(vec![cond("a", 0)]));
        reconcile(220, &ctx, "p", Reading::Standing(vec![cond("a", 0)]));
        assert_eq!((fx.filed(), fx.open()), (1, 1), "a condition that stays crossed must not file again");
        reconcile(280, &ctx, "p", Reading::Standing(vec![]));
        assert_eq!(fx.open(), 0, "a cleared condition resolves its bead");
        reconcile(340, &ctx, "p", Reading::Standing(vec![cond("a", 0)]));
        assert_eq!((fx.filed(), fx.open()), (2, 1), "a recurrence after clearing is a new filing");
    }

    #[test]
    fn a_condition_files_only_once_it_has_stood_for_its_sustain_window() {
        let fx = Fixture::new("cond-sustain");
        let ctx = fx.ctx();
        reconcile(1000, &ctx, "p", Reading::Standing(vec![cond("a", 300)]));
        reconcile(1299, &ctx, "p", Reading::Standing(vec![cond("a", 300)]));
        assert_eq!(fx.filed(), 0);
        reconcile(1300, &ctx, "p", Reading::Standing(vec![cond("a", 300)]));
        assert_eq!(fx.filed(), 1);
    }

    #[test]
    fn a_blip_shorter_than_the_window_restarts_the_clock() {
        let fx = Fixture::new("cond-blip");
        let ctx = fx.ctx();
        reconcile(1000, &ctx, "p", Reading::Standing(vec![cond("a", 300)]));
        reconcile(1200, &ctx, "p", Reading::Standing(vec![]));
        reconcile(1400, &ctx, "p", Reading::Standing(vec![cond("a", 300)]));
        reconcile(1600, &ctx, "p", Reading::Standing(vec![cond("a", 300)]));
        assert_eq!(fx.filed(), 0, "the window counts from the latest crossing");
    }

    #[test]
    fn an_unreadable_probe_neither_files_nor_clears() {
        let fx = Fixture::new("cond-unknown");
        let ctx = fx.ctx();
        reconcile(100, &ctx, "p", Reading::Standing(vec![cond("a", 0)]));
        reconcile(160, &ctx, "p", Reading::Unknown);
        assert_eq!((fx.filed(), fx.open()), (1, 1));
    }

    #[test]
    fn each_key_is_its_own_condition() {
        let fx = Fixture::new("cond-keys");
        let ctx = fx.ctx();
        reconcile(100, &ctx, "p", Reading::Standing(vec![cond("a", 0), cond("b", 0)]));
        assert_eq!(fx.open(), 2);
        reconcile(160, &ctx, "p", Reading::Standing(vec![cond("b", 0)]));
        assert_eq!(fx.open(), 1, "only the cleared key's bead resolves");
    }
}
