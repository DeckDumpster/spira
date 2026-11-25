//! The world outside the classifier: the bead store (through the lifecycle door), spira-lc, /proc,
//! systemctl, and the harness's own run files. Every payload a subprocess needs travels on
//! its stdin (law-payloads-go-on-stdin); argv carries only verbs, flags, ids and labels.

use std::process::Command;
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::classify::Throttle;
use crate::config::Config;
use crate::model::{parse_beads, Bead};
use spira_store::lc_call::LcClient;

/// A subprocess's outcome: exit success, stdout, stderr.
pub struct Out {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

pub fn run(program: &str, args: &[&str], stdin: Option<&[u8]>, env: &[(&str, &str)]) -> Out {
    // batch-job: this runs whatever its caller names, as long as that takes
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let child = cmd.spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return Out { ok: false, stdout: String::new(), stderr: format!("{program}: {e}") },
    };
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A writer thread, so a child that fills its stdout pipe before draining stdin
        // cannot deadlock us.
        let data = data.to_vec();
        std::thread::spawn(move || {
            let _ = pipe.write_all(&data);
        });
    }
    match child.wait_with_output() {
        Ok(o) => Out {
            ok: o.status.success(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        },
        Err(e) => Out { ok: false, stdout: String::new(), stderr: format!("{program}: {e}") },
    }
}

/// The store view the classification needs, in two door reads: the open beads, then by id
/// every closed bead an open one names as a blocker or parent (R1: a blocker's status is a
/// lookup, never an inference from its absence). A refused or failed read is `Err`.
pub fn load_store(lc: &LcClient) -> Result<Vec<Bead>, String> {
    let read = |args: &[&str]| -> Result<Vec<Bead>, String> {
        let argv: Vec<&str> = ["content", "list"].iter().chain(args).copied().chain(["--brief", "--json"]).collect();
        let raw = lc.answer(&argv).map_err(|e| format!("content list: {e}"))?;
        parse_beads(&raw).map_err(|e| format!("content list: {e}"))
    };
    let mut beads = read(&["--limit", "0"])?;
    let have: HashSet<String> = beads.iter().map(|b| b.id.clone()).collect();
    let mut named: Vec<String> = beads
        .iter()
        .flat_map(|b| b.blocks_targets().chain(b.parent.as_deref()))
        .filter(|id| !id.is_empty() && !have.contains(*id))
        .map(str::to_string)
        .collect();
    named.sort();
    named.dedup();
    for chunk in named.chunks(NAMED_CHUNK) {
        beads.extend(read(&["--all", "--limit", "0", "--id", &chunk.join(",")])?);
    }
    // An open epic with no open child may have only closed ones; without them it reads as empty.
    let parents: HashSet<&str> = beads.iter().filter_map(|b| b.parent.as_deref()).collect();
    let childless: Vec<String> = beads.iter().filter(|b| b.is_epic() && !b.is_closed() && !parents.contains(b.id.as_str())).map(|b| b.id.clone()).collect();
    for epic in childless {
        beads.extend(read(&["--all", "--limit", "0", "--parent", &epic])?);
    }
    Ok(beads)
}

const NAMED_CHUNK: usize = 500;

/// The claimable set under one partition's predicate: spira-claim's `ready-count --json`,
/// the one ready set the caller counts and a dwarf claims from (the machine's READY/REWORK
/// rows) — never bd's ready query nor the sentinel's bd-ready snapshot, whose status and
/// assignee no claim writes (sp-7g5q6). A refusal is `Err`, never empty.
pub fn ready(cfg: &Config, labels: &str, exclude: &[String]) -> Result<HashSet<String>, String> {
    let excl = exclude.join(",");
    let o = run("timeout", &["60", cfg.claim_bin.as_str(), "ready-count", labels, &excl, "--json"], None, &[]);
    if !o.ok {
        return Err(format!("spira-claim ready-count {labels}: {}", o.stderr.trim()));
    }
    ready_ids(&o.stdout).map_err(|e| format!("spira-claim ready-count {labels}: {e}"))
}

/// The bead ids in `spira-claim ready-count --json` output.
pub fn ready_ids(json: &str) -> Result<HashSet<String>, String> {
    Ok(parse_beads(json)?.into_iter().map(|b| b.id).collect())
}

pub fn show(lc: &LcClient, id: &str) -> Option<Bead> {
    let raw = lc.answer(&["content", "show", id, "--json"]).ok()?;
    parse_beads(&raw).ok()?.into_iter().next()
}

/// Sends `recompute-blocked` to the content door; `Err` names why it did not apply.
pub fn recompute_blocked(lc: &Result<LcClient, String>) -> Result<(), String> {
    let lc = lc.as_ref().map_err(Clone::clone)?;
    lc.answer(&["content", "recompute-blocked"]).map(drop).map_err(|e| format!("content recompute-blocked: {e}"))
}

/// An id or actor fit to appear inside a SQL literal: bead-id characters only.
pub fn sql_safe(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@'))
}

// ------------------------------------------------------------------------------------------
// The roster probe (DESIGN.md §2.5).
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionSpec {
    pub labels: String,
    pub exclude: String,
    pub runes: Vec<String>,
}

pub fn parse_roster(text: &str) -> Vec<PartitionSpec> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.splitn(3, '\t');
            let labels = f.next()?.trim().to_string();
            if labels.is_empty() {
                return None;
            }
            let exclude = f.next().unwrap_or("").trim().to_string();
            let runes = f.next().unwrap_or("").split_whitespace().map(str::to_string).collect();
            Some(PartitionSpec { labels, exclude, runes })
        })
        .collect()
}

/// Every partition this run covers, with the personas that work it. `want` narrows to one
/// partition (SPIRA_LABELS). Err when the runes directory is missing — never an empty roster.
pub fn roster(cfg: &Config, want: Option<&str>) -> Result<Vec<PartitionSpec>, String> {
    use spira_config::runes::{rune_partitions, runes_dir, runes_for_labels, spira_runes};
    let home = cfg.home.as_ref().ok_or("neither SPIRA_HOME nor SPIRA_RELEASE is set")?;
    let dir = runes_dir(home);
    if !dir.is_dir() {
        return Err(format!("{} is missing — cannot read the persona roster", dir.display()));
    }
    let over = cfg.runes_override.as_deref();
    let spec = |labels: String, exclude: String| {
        let mut runes = runes_for_labels(home, &labels);
        if runes.is_empty() {
            runes = spira_runes(home, over).split_whitespace().map(str::to_string).collect();
        }
        PartitionSpec { labels, exclude, runes }
    };
    Ok(match want.filter(|w| !w.is_empty()) {
        Some(w) => vec![spec(w.to_string(), String::new())],
        None => rune_partitions(home, over).into_iter().filter(|(l, _)| !l.is_empty()).map(|(l, x)| spec(l, x)).collect(),
    })
}

// ------------------------------------------------------------------------------------------
// Liveness and the fleet
// ------------------------------------------------------------------------------------------

pub use hel::reap::{lease_file, lease_live};

/// Write an identity lease: the deadline, in epoch seconds, before which the dwarf is alive.
pub fn write_lease(pidfile: &Path, deadline: i64) {
    let f = lease_file(pidfile);
    let tmp = f.with_extension("lease.tmp");
    if fs::write(&tmp, deadline.to_string()).is_ok() {
        let _ = fs::rename(&tmp, &f);
    }
}

/// A dwarf is alive while its identity lease is ahead of the clock — the one liveness signal.
/// The pidfile names the identity; the pid and `/proc` are never consulted.
pub fn dwarf_alive(pf: &Path) -> bool {
    hel::reap::dwarf_alive(pf)
}

fn pidfiles(run: &Path, prefix: &str, suffix: &str) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(run) else { return Vec::new() };
    rd.filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(prefix) && n.ends_with(suffix) && n.len() > prefix.len() + suffix.len())
        })
        .collect()
}

/// Is a live process working this bead: a hold pidfile (pid alive), or a dwarf pidfile
/// (pid alive and a dwarf). A thin wrapper onto `hel::reap::holder_alive` — the
/// destruction chokepoint's own canonical implementation (sp-9envm) — rather than this
/// crate's own copy (wave 4.23, sp-0ffox): ONE holder-liveness predicate, not two.
pub fn holder_alive(run: &Path, id: &str) -> bool {
    hel::reap::holder_alive(run, id)
}

/// Every live dwarf unit, by the globs and liveness rule `dwarves.sh` and the fleet ceiling count
/// by. Err when systemd cannot answer: an unreadable list is not an empty fleet.
pub fn live_units(cfg: &Config) -> Result<Vec<String>, String> {
    all_live_units(cfg)
}

/// How many of `live_units` were directed onto a named bead.
pub fn dwarves_live_directed(cfg: &Config) -> Result<u32, String> {
    Ok(all_live_units(cfg)?.iter().filter(|u| runtime::units::is_directed(u)).count() as u32)
}

fn all_live_units(cfg: &Config) -> Result<Vec<String>, String> {
    let units = runtime::units::host((cfg.systemctl != "systemctl").then_some(cfg.systemctl.as_str()));
    let mut live = std::collections::BTreeSet::new();
    for glob in runtime::units::dwarf_globs() {
        let listed = units.list(&glob).map_err(|e| format!("listing the dwarf units failed, so the live dwarves are unknown: {}", e.0))?;
        live.extend(listed.into_iter().filter(|u| runtime::units::is_live_dwarf(&u.name, u.state)).map(|u| u.name));
    }
    Ok(live.into_iter().collect())
}

fn unit_count(cfg: &Config, keep: &dyn Fn(&str) -> bool, exclude: Option<&str>) -> u32 {
    let live = live_units(cfg).unwrap_or_default();
    live.iter().filter(|u| keep(u) && Some(u.as_str()) != exclude).count() as u32
}

/// Live dwarves of one persona: the unit list under systemd-run (the pidfile lags the unit),
/// live pidfiles otherwise. Read-only: a dead pidfile is left for its owner to reap.
///
/// `exclude` is the caller's OWN transient unit (lib.sh `dwarf_count <rune>
/// [exclude-unit]`, sp-0hnm6): systemd-run's unit exists before the capacity check ever
/// runs, so a sweep counting its own rune was counting itself. Matched only in the
/// systemd-run branch, exactly as the bash original — the pidfile fallback never sees the
/// race it guards against (dwarf.sh writes its own pidfile only after this check runs).
pub fn dwarf_count(cfg: &Config, rune: &str, exclude: Option<&str>) -> u32 {
    dwarf_count_named(cfg, rune, rune, exclude)
}

/// [`dwarf_count`] for a rune whose by-name units carry `rune_name` (its `RUNE_NAME`): the
/// scheduler names its units by the rune id, `spira-dwarf-*` units by `RUNE_NAME`.
fn dwarf_count_named(cfg: &Config, rune: &str, rune_name: &str, exclude: Option<&str>) -> u32 {
    if cfg.call == "systemd-run" {
        return unit_count(cfg, &|u| runtime::units::is_dwarf_of(u, rune, rune_name), exclude);
    }
    let Some(run_dir) = cfg.run.as_ref() else { return 0 };
    pidfiles(run_dir, &format!("dwarf-{rune_name}-"), ".pid").iter().filter(|p| dwarf_alive(p)).count() as u32
}

/// How many dwarves exist right now, across every persona.
pub fn dwarves_live_total(cfg: &Config) -> u32 {
    if cfg.call == "systemd-run" {
        return unit_count(cfg, &|_| true, None);
    }
    let Some(run_dir) = cfg.run.as_ref() else { return 0 };
    pidfiles(run_dir, "dwarf-", ".pid").iter().filter(|p| dwarf_alive(p)).count() as u32
}

const UNBOUNDED: i64 = 999_999;

/// `rune_free <rune> [pool-remaining] [exclude-unit]` (lib.sh): how many more of this
/// persona may be called right now. lib.sh's OWN `rune_free` stays bash — it calls
/// `dwarf_count`/`rune_get` BY NAME, and several suites (test-dwarf-elastic-concurrency.sh, test-builder-qa-proposed.sh,
/// test-dependents.sh) redefine those bash functions to control
/// `rune_free`'s inputs without a real process table. Reducing `rune_free` itself to a
/// one-line shim would move that arithmetic into a separate process where a bash-level
/// override can no longer reach it (the EXEC-BOUNDARY TRAP) — so this Rust copy exists for
/// any FUTURE in-process Rust caller, and lib.sh's bash body is deliberately left alone.
pub fn rune_free(cfg: &Config, rune: &str, pool: Option<i64>, exclude: Option<&str>) -> i64 {
    let Some(home) = cfg.home.as_ref() else { return 0 };
    let single_flight = spira_config::runes::rune_get(home, rune, "RUNE_SINGLE_FLIGHT", "0") == "1";
    let mut max: i64 = if single_flight { 1 } else { UNBOUNDED };
    let elastic = spira_config::runes::rune_get(home, rune, "RUNE_ELASTIC", "0") == "1";
    let is_remainder = elastic && pool.is_some();
    if is_remainder {
        max = pool.unwrap();
    } else if elastic {
        max = 1;
    }
    let have: i64 = dwarf_count(cfg, rune, exclude).into();
    let mut free = if is_remainder { max } else { (max - have).max(0) };
    if let Some(p) = pool {
        if p < free {
            free = p;
        }
    }
    free
}

// ------------------------------------------------------------------------------------------
// Correct refusals the starved rule must recognise
// ------------------------------------------------------------------------------------------

/// Some(detail) while the capacity pause runs. Read-only (DESIGN.md §2.3).
pub fn capacity(cfg: &Config, now: i64) -> Option<String> {
    let path = cfg.capacity_pause_path()?;
    let text = fs::read_to_string(path).ok()?;
    let at: i64 = text.lines().next()?.split_whitespace().next()?.parse().ok()?;
    (at > now).then(|| {
        format!(
            "the account is out for another {}s (until {})",
            at - now,
            crate::timefmt::local_hhmm(at)
        )
    })
}

pub fn throttle(cfg: &Config) -> Throttle {
    let Some(stamp) = cfg.throttle_stamp_path() else {
        return Throttle::Open;
    };
    if fs::symlink_metadata(&stamp).is_err() {
        return Throttle::Open;
    }
    let line = fs::read(&stamp)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).lines().next().unwrap_or("").to_string())
        .filter(|l| !l.is_empty());
    match line {
        Some(l) => {
            let depth = l
                .match_indices("depth=")
                .find_map(|(i, _)| {
                    let digits: String = l[i + 6..].chars().take_while(char::is_ascii_digit).collect();
                    (!digits.is_empty()).then_some(digits)
                })
                .unwrap_or_else(|| "?".into());
            Throttle::Shut(format!("depth {} >= release-at {}", depth, cfg.throttle_release_at))
        }
        None => Throttle::Unreadable(format!("{} exists but could not be read", stamp.display())),
    }
}

/// The scheduler's last-pass refusals, by rune.
pub fn scheduler_uncalled() -> Result<std::collections::BTreeMap<String, String>, String> {
    let sock = spira_config::process::cfg("SPIRA_RUNTIME_SOCKET")?;
    let body = spira_config::scheduler_link::call(std::path::Path::new(sock.trim()), &["scheduler", "uncalled"])?;
    serde_json::from_str(&body).map_err(|e| format!("the scheduler's uncalled answer does not decode: {e}"))
}

/// `rune: reason` for each of `runes` the scheduler refused, joined by `; `; the scheduler's error when it did not answer.
pub fn scheduler_reason(uncalled: &Result<std::collections::BTreeMap<String, String>, String>, runes: &[String]) -> Result<String, String> {
    let map = uncalled.as_ref().map_err(|e| e.clone())?;
    let said: Vec<String> = runes.iter().filter_map(|r| map.get(r).map(|why| format!("{r}: {why}"))).collect();
    if said.is_empty() {
        return Ok(format!("no refusal recorded for {}", runes.join(", ")));
    }
    Ok(said.join("; "))
}

pub fn throttle_line(t: &Throttle) -> String {
    match t {
        Throttle::Open => "open\t".into(),
        Throttle::Shut(d) => format!("shut\t{d}"),
        Throttle::Unreadable(d) => format!("unreadable\t{d}"),
    }
}

/// The sentinel's pass-start marker: its `state: open=<n> …` line (sp-k6m1m dropped the
/// `goal=` field that used to lead it).
pub const PASS_MARKER: &str = ": state: open=";

/// Did the last sentinel pass skip this partition's personas ("pass budget exhausted")?
pub fn pass_truncated(log_text: &str, runes: &[String]) -> bool {
    let lines: Vec<&str> = log_text.lines().collect();
    let Some(start) = lines.iter().rposition(|l| l.contains(PASS_MARKER)) else {
        return false;
    };
    runes.iter().any(|f| {
        let needle = format!("CHECK7 {f}: not evaluated");
        lines[start..].iter().any(|l| l.contains(&needle))
    })
}

// ------------------------------------------------------------------------------------------
// The harness's pulse (report header)
// ------------------------------------------------------------------------------------------

/// Seconds since the scheduler's last pass, from the runtime's tick stamp; -1 when it never ran.
pub fn last_pass_age(cfg: &Config) -> i64 {
    cfg.run.as_deref().and_then(|r| spira_config::ticked::quiet_secs(r, "sentinel-pass")).map_or(-1, |s| s as i64)
}

#[cfg(test)]
mod tests {
    #[test_id(tpjoi0i)]
    #[test]
    fn scheduler_reason_names_the_partitions_runes_the_scheduler_refused() {
        let map: Result<std::collections::BTreeMap<String, String>, String> = Ok([("skuld".to_string(), "at concurrency cap".to_string()), ("other".to_string(), "x".to_string())].into());
        let runes = vec!["skuld".to_string(), "mender".to_string()];
        assert_eq!(scheduler_reason(&map, &runes), Ok("skuld: at concurrency cap".into()));
        assert_eq!(scheduler_reason(&map, &["mender".to_string()]), Ok("no refusal recorded for mender".into()));
        assert_eq!(scheduler_reason(&Err("down".into()), &runes), Err("down".into()));
    }

    use super::*;

    fn now_secs() -> i64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
    }

    // ---- load_store: two door reads, never the whole history (sp-yf8lf0) -----------------

    fn row(id: &str, status: &str, parent: Option<&str>, blocked_by: &[&str]) -> serde_json::Value {
        let deps: Vec<_> = blocked_by.iter().map(|b| serde_json::json!({"issue_id": id, "depends_on_id": b, "type": "blocks"})).collect();
        serde_json::json!({"id": id, "title": id, "status": status, "parent": parent, "dependencies": deps})
    }

    fn door(rows: Vec<serde_json::Value>) -> (std::sync::Arc<spira_store::lc_call::fake::Content>, LcClient) {
        let fake = std::sync::Arc::new(spira_store::lc_call::fake::Content::new(rows));
        let lc = LcClient::over(fake.clone(), true);
        (fake, lc)
    }

    #[test_id(ttff11i)]
    #[test]
    fn sp_yf8lf0_the_store_view_is_the_open_beads_and_the_closed_ones_they_name() {
        let (fake, lc) = door(vec![
            row("sp-a", "open", None, &["sp-c"]),
            row("sp-c", "closed", None, &[]),
            row("sp-k", "open", Some("sp-e"), &[]),
            row("sp-e", "closed", None, &[]),
            row("sp-z", "closed", None, &[]),
        ]);
        let mut ids: Vec<String> = load_store(&lc).unwrap().into_iter().map(|b| b.id).collect();
        ids.sort();
        assert_eq!(ids, ["sp-a", "sp-c", "sp-e", "sp-k"]);
        let seen = fake.requests();
        assert_eq!(seen.len(), 2, "{seen:?}");
        let at = seen[1].iter().position(|x| x == "--id").expect("the second read is by id");
        assert_eq!(seen[1][at + 1], "sp-c,sp-e");
        assert!(seen[1..].iter().all(|r| r.iter().any(|x| x == "--id")), "{seen:?}");
    }

    #[test_id(tmaz543)]
    #[test]
    fn sp_yf8lf0_an_open_epic_with_only_closed_children_keeps_them() {
        let mut epic = row("sp-e", "open", None, &[]);
        epic["issue_type"] = "epic".into();
        let (fake, lc) = door(vec![epic, row("sp-k", "closed", Some("sp-e"), &[]), row("sp-z", "closed", None, &[])]);
        let mut ids: Vec<String> = load_store(&lc).unwrap().into_iter().map(|b| b.id).collect();
        ids.sort();
        assert_eq!(ids, ["sp-e", "sp-k"]);
        assert!(fake.requests().iter().skip(1).all(|r| r.iter().any(|x| x == "--parent")), "{:?}", fake.requests());
    }

    #[test_id(trzlos2)]
    #[test]
    fn sp_yf8lf0_a_refused_read_is_an_error_not_an_empty_store() {
        struct Refusing;
        impl spira_store::lc_call::Transport for Refusing {
            fn exchange(&self, _: &[String], _: Option<&str>, _: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
                Ok(spira_store::lc_call::Reply { code: spira_store::lc_call::REFUSED, out: String::new(), err: "no\n".into(), read_ended: false })
            }
        }
        let lc = LcClient::over(std::sync::Arc::new(Refusing), true);
        assert!(load_store(&lc).is_err());
    }

    #[test_id(t79od6r)]
    #[test]
    fn sp_yf8lf0_a_blocker_in_neither_read_stays_unknown() {
        let (_fake, lc) = door(vec![row("sp-a", "open", None, &["sp-gone"])]);
        let store = load_store(&lc).unwrap();
        assert!(store.iter().all(|b| b.id != "sp-gone"));
    }

    #[test_id(tk1wwi6)]
    #[test]
    fn sp_yf8lf0_many_named_blockers_are_read_in_chunks_of_500_each_once() {
        let mut rows: Vec<_> = (0..1100).map(|i| row(&format!("sp-b{i}"), "closed", None, &[])).collect();
        rows.push(row("sp-a", "open", None, &(0..1100).map(|i| format!("sp-b{i}")).collect::<Vec<_>>().iter().map(String::as_str).collect::<Vec<_>>()));
        rows.push(row("sp-d", "open", None, &["sp-b0"]));
        let (fake, lc) = door(rows);
        let store = load_store(&lc).unwrap();
        assert_eq!(store.len(), 1102);
        let seen = fake.requests();
        assert_eq!(seen.len(), 4, "one open read and three by-id chunks");
        let ids: Vec<&str> = seen[1..].iter().flat_map(|r| r[r.iter().position(|x| x == "--id").unwrap() + 1].split(',')).collect();
        assert_eq!(ids.len(), 1100);
        assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 1100, "no id is asked twice");
    }

    #[test_id(tmziypd)]
    #[test]
    fn sp_yf8lf0_show_reads_one_bead_through_the_door_and_is_none_when_refused() {
        struct Show(i32);
        impl spira_store::lc_call::Transport for Show {
            fn exchange(&self, argv: &[String], _: Option<&str>, _: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
                assert_eq!(argv, ["content", "show", "sp-x", "--json"]);
                Ok(spira_store::lc_call::Reply { code: self.0, out: r#"[{"id":"sp-x","title":"t","status":"open"}]"#.into(), err: String::new(), read_ended: false })
            }
        }
        let ok = LcClient::over(std::sync::Arc::new(Show(0)), true);
        assert_eq!(show(&ok, "sp-x").unwrap().id, "sp-x");
        let refused = LcClient::over(std::sync::Arc::new(Show(spira_store::lc_call::REFUSED)), true);
        assert!(show(&refused, "sp-x").is_none());
    }

    // ---- holder_alive: delegates to hel::reap (wave 4.23, sp-0ffox) -------------------

    #[test_id(tzluq4x)]
    #[test]
    fn holder_alive_is_hels_own_predicate() {
        let run = testkit::TempDir::new("strand-holder-alive");
        std::fs::write(run.join("hold-sp-h1.pid"), std::process::id().to_string()).unwrap();
        assert!(holder_alive(&run, "sp-h1"));
        assert!(!holder_alive(&run, "sp-h2"), "nothing holds sp-h2");
        // Liveness is the lease: a live pid with no lease is not a dwarf, and a lease in the
        // past is not one either.
        std::fs::write(run.join("dwarf-builder-sp-a1.pid"), std::process::id().to_string()).unwrap();
        assert!(!holder_alive(&run, "sp-a1"), "a pid alone does not hold a claim");
        std::fs::write(run.join("dwarf-builder-sp-a1.lease"), "1").unwrap();
        assert!(!holder_alive(&run, "sp-a1"), "an expired lease does not hold a claim");
        std::fs::write(run.join("dwarf-builder-sp-a1.lease"), (now_secs() + 60).to_string()).unwrap();
        assert!(holder_alive(&run, "sp-a1"), "a running lease does");
        std::fs::write(run.join("dwarf-builder-sp-a1.pid"), "999999999").unwrap();
        assert!(holder_alive(&run, "sp-a1"), "the pid is never probed");
    }

    // ---- dwarf_count / rune_free (wave 4.23, sp-0ffox) ------------------

    #[test_id(tju2r9r)]
    #[test]
    fn dwarf_count_pid_fallback_ignores_exclude_exactly_as_the_bash_original() {
        let t = testkit::TempDir::new("strand-dwarf-count-fallback");
        let run = t.join("run");
        std::fs::create_dir_all(&run).unwrap();
        let cfg = Config { call: "mock".into(), run: Some(run.clone()), ..Config::test_fixture() };
        assert_eq!(dwarf_count(&cfg, "builder", None), 0, "no pidfiles at all");
        assert_eq!(dwarf_count(&cfg, "builder", Some("anything")), 0, "exclude is a no-op off the systemd-run branch");
    }

    /// A `systemctl` that answers `show <glob>...` as systemd does: one block per unit
    /// matching a glob, with its `ActiveState`; `list-unit-files` lists none.
    fn fake_systemctl(dir: &Path, units: &[(&str, &str)]) -> String {
        let table: String = units.iter().map(|(n, st)| format!("{n} {st}\n")).collect();
        std::fs::write(dir.join("units"), table).unwrap();
        let script = dir.join("systemctl");
        let body = format!(
            "#!/bin/sh\ncase \"$*\" in *show*) ;; *) exit 0;; esac\nfor g in \"$@\"; do case \"$g\" in -*|show) ;; *) while read -r n st; do case \"$n\" in $g) printf 'Id=%s\\nLoadState=loaded\\nActiveState=%s\\nSubState=running\\n\\n' \"$n\" \"$st\";; esac; done < '{}';; esac; done\n",
            dir.join("units").display()
        );
        testkit::write_exe(&script, &body);
        script.to_string_lossy().into_owned()
    }

    #[test_id(tzzgq01)]
    #[test]
    fn sp_li4zys_dwarves_the_scheduler_started_are_counted() {
        let t = testkit::TempDir::new("strand-sp-li4zys");
        let b1 = "spira-run-sentinel_2dcall-builder_2d1794009721297_2d1.service";
        let units = [
            (b1, "active"),
            ("spira-run-sentinel_2dcall-builder_2d1794012840137_2d0.service", "activating"),
            ("spira-run-sentinel_2dcall-ops_2d1794012840137_2d2.service", "active"),
            ("spira-run-sentinel_2dcall-builder_2d1794000000000_2d0.service", "failed"),
            ("spira-run-sentinel_2dpass-landing.service", "active"),
        ];
        let cfg = Config { call: "systemd-run".into(), systemctl: fake_systemctl(&t, &units), ..Config::test_fixture() };
        assert_eq!(dwarf_count(&cfg, "builder", None), 2, "two live builder dwarves, one failed");
        assert_eq!(dwarf_count(&cfg, "builder", Some(b1)), 1, "the caller's own unit is excluded");
        assert_eq!(dwarf_count(&cfg, "ops", None), 1);
        assert_eq!(dwarves_live_total(&cfg), 3, "every live dwarf; the landing pass is not one");
    }

    #[test_id(tc3dpmu)]
    #[test]
    fn the_report_names_the_scheduler_started_dwarf_even_when_the_call_mode_is_not_systemd_run() {
        let t = testkit::TempDir::new("strand-evtrs9-live");
        let u = "spira-run-sentinel_2dcall-builder_2d1_2d0.service";
        let cfg = Config { call: "runtime".into(), systemctl: fake_systemctl(&t, &[(u, "active")]), ..Config::test_fixture() };
        assert_eq!(live_units(&cfg).unwrap(), vec![u.to_string()]);
    }

    #[test_id(t7ypxjf)]
    #[test]
    fn a_directed_dwarf_is_a_live_unit_and_the_directed_count_is_part_of_the_total() {
        let t = testkit::TempDir::new("strand-sp-ljgmia");
        let d = "spira-dwarf-builder-directed-1794009721.service";
        let p = "spira-dwarf-builder-1794009700.service";
        let cfg = Config { systemctl: fake_systemctl(&t, &[(p, "active"), (d, "active")]), ..Config::test_fixture() };
        assert_eq!(live_units(&cfg).unwrap(), vec![p.to_string(), d.to_string()]);
        assert_eq!(dwarves_live_directed(&cfg).unwrap(), 1);
    }

    #[test_id(tykrmgs)]
    #[test]
    fn an_unanswering_systemctl_is_an_error_not_zero_live_dwarves() {
        let t = testkit::TempDir::new("strand-evtrs9-unreadable");
        let sc = t.join("systemctl");
        testkit::write_exe(&sc, "#!/bin/sh\necho 'Failed to connect to bus' >&2\nexit 1\n");
        let cfg = Config { systemctl: sc.to_string_lossy().into_owned(), ..Config::test_fixture() };
        let e = live_units(&cfg).unwrap_err();
        assert!(e.contains("live dwarves are unknown") && e.contains("exited 1"), "{e}");
    }

    #[test_id(t1ed66x)]
    #[test]
    fn the_last_pass_is_the_sentinel_pass_tick_and_never_ticked_is_negative() {
        let run = testkit::TempDir::new("strand-evtrs9-pass");
        let cfg = Config { run: Some(run.to_path_buf()), ..Config::test_fixture() };
        assert_eq!(last_pass_age(&cfg), -1);
        spira_config::ticked::stamp(&run, "sentinel-pass");
        assert!((0..5).contains(&last_pass_age(&cfg)));
    }

    #[test_id(teb2vj0)]
    #[test]
    fn a_directed_dwarf_counts_in_its_runes_count_and_the_total() {
        let t = testkit::TempDir::new("strand-directed");
        let units = [("spira-dwarf-builder-1794009721.service", "active"), ("spira-dwarf-builder-directed-1794009722.service", "active")];
        let cfg = Config { call: "systemd-run".into(), systemctl: fake_systemctl(&t, &units), ..Config::test_fixture() };
        assert_eq!(dwarf_count(&cfg, "builder", None), 2);
        assert_eq!(dwarves_live_total(&cfg), 2);
    }

    #[test_id(t73quzf)]
    #[test]
    fn rune_free_matches_the_bash_originals_arithmetic() {
        let t = testkit::TempDir::new("strand-rune-free");
        let _env = testkit::env(&[("SPIRA_TOML", None), ("SPIRA_RUNES_DIR", Some(t.join("runes").to_str().unwrap()))]);
        std::fs::create_dir_all(t.join("runes")).unwrap();
        std::fs::write(t.join("runes/stretchy.rune"), "RUNE_ELASTIC=1\n").unwrap();
        std::fs::write(t.join("runes/anchor.rune"), "\n").unwrap();
        std::fs::write(t.join("runes/laner.rune"), "RUNE_SINGLE_FLIGHT=1\n").unwrap();
        let run = t.join("run");
        std::fs::create_dir_all(&run).unwrap();
        // pid fallback, no real dwarves: have=0 throughout
        let cfg = Config { home: Some(t.to_path_buf()), call: "mock".into(), run: Some(run.clone()), ..Config::test_fixture() };
        // elastic: pool remainder whole, never subtracting running twice (have is ignored
        // here since the stub always reports 0 — the G18 regression this guards against is
        // "have" double-counted against a pool that already nets it out).
        assert_eq!(rune_free(&cfg, "stretchy", Some(5), None), 5);
        // no pool: an elastic rune takes one.
        assert_eq!(rune_free(&cfg, "stretchy", None, None), 1);
        // non-elastic: cap minus running (0), clamped at 0 from below.
        assert_eq!(rune_free(&cfg, "anchor", Some(10), None), 10);
        assert_eq!(rune_free(&cfg, "laner", None, None), 1);
    }

    #[test_id(t24dxgo)]
    #[test]
    fn roster_lines() {
        let r = parse_roster("spira,plan\tspira-poison,ask-x\tbuilder \nspira,incident\t\tops\n\n");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].labels, "spira,plan");
        assert_eq!(r[0].exclude, "spira-poison,ask-x");
        assert_eq!(r[0].runes, vec!["builder"]);
        assert_eq!(r[1].exclude, "");
    }

    #[test_id(ti5f544)]
    #[test]
    fn truncation_reads_only_the_last_pass() {
        let log = "a: state: open=1\nCHECK7 builder: not evaluated (pass budget exhausted)\nb: state: open=2\nCHECK7 ops: ok\n";
        assert!(!pass_truncated(log, &["builder".into()]));
        let log2 = format!("{log}CHECK7 builder: not evaluated (pass budget exhausted)\n");
        assert!(pass_truncated(&log2, &["builder".into()]));
        assert!(!pass_truncated("no passes", &["builder".into()]));
    }

    #[test_id(t5nizp7)]
    #[test]
    fn sql_safety() {
        assert!(sql_safe("sp-zs04v.5"));
        assert!(!sql_safe("x'; drop table events; --"));
        assert!(!sql_safe(""));
    }

    #[test_id(tqndvv9)]
    #[test]
    fn throttle_states_from_the_stamp() {
        let dir = testkit::TempDir::new("strand-throttle");
        let stamp = dir.join("queue-throttled");
        let cfg = Config { throttle_stamp: Some(stamp.clone()), ..Config::test_fixture() };
        assert_eq!(throttle_line(&throttle(&cfg)), "open\t");
        fs::write(&stamp, "2026-09-29T00:00:00Z depth=14 x\n").unwrap();
        assert_eq!(throttle_line(&throttle(&cfg)), "shut\tdepth 14 >= release-at 8");
        fs::write(&stamp, "").unwrap();
        assert!(throttle_line(&throttle(&cfg)).starts_with("unreadable\t"));
        fs::write(&stamp, "no depth here\n").unwrap();
        assert_eq!(throttle_line(&throttle(&cfg)), "shut\tdepth ? >= release-at 8");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test_id(t4jyk7z)]
    #[test]
    fn the_roster_is_read_from_the_runes_directory_not_from_a_lib_sh_that_fails() {
        let home = testkit::TempDir::new("strand-roster");
        std::fs::write(home.join("lib.sh"), "exit 1\n").unwrap();
        std::fs::create_dir(home.join("runes")).unwrap();
        std::fs::write(home.join("runes/builder.rune"), "RUNE_LABELS=spira,plan\nRUNE_EXCLUDE_LABELS=poison\n").unwrap();
        std::fs::write(home.join("runes/ops.rune"), "RUNE_LABELS=spira,incident\n").unwrap();
        let cfg = Config { home: Some(home.path().to_path_buf()), runes_override: None, ..Config::test_fixture() };
        let all = roster(&cfg, None).unwrap();
        assert_eq!(
            all,
            vec![
                PartitionSpec { labels: "spira,plan".into(), exclude: "poison".into(), runes: vec!["builder".into()] },
                PartitionSpec { labels: "spira,incident".into(), exclude: String::new(), runes: vec!["ops".into()] },
            ]
        );
        let one = roster(&cfg, Some("nobody")).unwrap();
        assert_eq!(one[0].runes, vec!["builder", "ops"], "a partition no persona names falls back to the whole roster");
        let gone = Config { home: Some(home.join("nowhere")), ..cfg };
        assert!(roster(&gone, None).is_err(), "a missing runes directory is never an empty roster");
    }
}
