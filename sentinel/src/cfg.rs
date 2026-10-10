//! Configuration: the context probe's answer (DESIGN.md §6, S0) turned into typed values,
//! plus every REGISTERED key (`spira/conf.d/`) `Cfg` needs (`Declared`, resolved from
//! `$SPIRA_TOML` via `spira_config::process::cfg`/`cfg_parse` — one source of config, per
//! Ryan 2026-10-05). `Cfg::from_context` itself never calls `cfg()`; it only combines a
//! `Context` (identity/non-registered knobs) with an already-resolved `Declared`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fayth {
    pub name: String,
    pub labels: Vec<String>,
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Partition {
    pub labels: Vec<String>,
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Repo {
    pub name: String,
    pub root: Option<String>,
    /// spira_landrefs: [0] is the base (spira_landref); a second is its local counterpart.
    pub landrefs: Vec<String>,
    pub forge_queued: bool,
}

impl Repo {
    pub fn base(&self) -> Option<&str> {
        self.landrefs.first().map(String::as_str)
    }
}

/// The probe's answer, parsed.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub env: Vec<(String, String)>,
    pub vars: BTreeMap<String, String>,
    pub fayths: Vec<Fayth>,
    pub partitions: Vec<Partition>,
    pub chamber: Vec<String>,
    pub repos: Vec<Repo>,
}

pub fn csv(s: &str) -> Vec<String> {
    s.split(',')
        .filter(|x| !x.is_empty())
        .map(str::to_string)
        .collect()
}

/// Environment entries a child must not inherit from the probe's own bash.
fn env_skip(k: &str) -> bool {
    matches!(k, "_" | "SHLVL" | "PWD" | "OLDPWD") || k.starts_with("SENTINEL_")
}

impl Context {
    /// Records are NUL-terminated; `@name` records open a section; before the first one,
    /// every record is `KEY=VALUE` from `env -0`.
    pub fn parse(raw: &[u8]) -> Result<Context, String> {
        let text = String::from_utf8_lossy(raw);
        let mut c = Context::default();
        let mut section = "env";
        let mut ended = false;
        for rec in text.split('\0') {
            if let Some(s) = rec.strip_prefix('@') {
                if !s.contains('=') && !s.contains('\t') {
                    section = match s {
                        "vars" => "vars",
                        "fayths" => "fayths",
                        "partitions" => "partitions",
                        "chamber" => "chamber",
                        "repos" => "repos",
                        "end" => {
                            ended = true;
                            break;
                        }
                        other => return Err(format!("probe: unknown section @{other}")),
                    };
                    continue;
                }
            }
            if rec.is_empty() {
                continue;
            }
            match section {
                "env" => {
                    if let Some((k, v)) = rec.split_once('=') {
                        if !env_skip(k) {
                            c.env.push((k.to_string(), v.to_string()));
                        }
                    }
                }
                "vars" => {
                    if let Some((k, v)) = rec.split_once('=') {
                        c.vars.insert(k.to_string(), v.to_string());
                    }
                }
                "fayths" => {
                    let mut f = rec.splitn(3, '\t');
                    let name = f.next().unwrap_or("").to_string();
                    if !name.is_empty() {
                        c.fayths.push(Fayth {
                            name,
                            labels: csv(f.next().unwrap_or("")),
                            exclude: csv(f.next().unwrap_or("")),
                        });
                    }
                }
                "partitions" => {
                    let mut f = rec.splitn(2, '\t');
                    let labels = csv(f.next().unwrap_or(""));
                    if !labels.is_empty() {
                        c.partitions.push(Partition {
                            labels,
                            exclude: csv(f.next().unwrap_or("")),
                        });
                    }
                }
                "chamber" => c.chamber.push(rec.to_string()),
                "repos" => {
                    let f: Vec<&str> = rec.splitn(4, '\t').collect();
                    let name = f.first().copied().unwrap_or("").to_string();
                    if !name.is_empty() {
                        c.repos.push(Repo {
                            name,
                            root: f.get(1).filter(|s| !s.is_empty()).map(|s| s.to_string()),
                            landrefs: f
                                .get(2)
                                .map(|s| s.split_whitespace().map(str::to_string).collect())
                                .unwrap_or_default(),
                            forge_queued: f.get(3).copied() == Some("1"),
                        });
                    }
                }
                _ => {}
            }
        }
        if !ended {
            return Err("probe: output ended before @end — lib.sh did not load cleanly".into());
        }
        Ok(c)
    }

    pub fn get(&self, k: &str) -> Option<&str> {
        self.vars.get(k).map(String::as_str).or_else(|| {
            self.env
                .iter()
                .rev()
                .find(|(x, _)| x == k)
                .map(|(_, v)| v.as_str())
        })
    }

    pub fn fayth_names(&self) -> Vec<String> {
        self.fayths.iter().map(|f| f.name.clone()).collect()
    }

    pub fn repo(&self, name: &str) -> Option<&Repo> {
        self.repos.iter().find(|r| r.name == name)
    }
}

/// Every REGISTERED config key (`spira/conf.d/`) `Cfg` needs, resolved by the caller —
/// `Declared::resolve` (production: `spira_config::process::cfg`/`cfg_parse`, called once
/// by main.rs) or a literal test fixture (`Declared::test_default`, `#[cfg(test)]` below).
/// `Cfg::from_context` only ever reads these off `Declared`, never off `Context` and never
/// off `cfg()` itself — THE ONE DOOR to config is the caller's business, not pure
/// construction logic's, so `from_context`'s own tests stay independent of
/// `spira_config::process::cfg`'s process-wide, once-per-process cache (one source of
/// config, per Ryan 2026-10-05).
#[derive(Debug, Clone)]
pub struct Declared {
    pub run: PathBuf,
    pub db: String,
    pub bd: String,
    pub scope: String,
    pub ask: String,
    pub no_loop: String,
    pub incident_label: String,
    pub queue_wait: String,
    pub open_children: String,
    pub submitted: String,
    pub work_types: Vec<String>,
    pub reclaim_grace: i64,
    pub c5_max_file: u32,
    pub c5_max_resolve: u32,
    pub land_maxsec: String,
    pub max_aeons: Option<i64>,
    pub max_live_aeons: Option<i64>,
    pub lanes_max_live: Option<i64>,
    pub queue_throttle_override: String,
    pub summon_lock_wait: u64,
    /// `SPIRA_LANES`, for CHECK 7's own log line (summon.rs) — display only.
    pub lanes: String,
    /// Forwarded verbatim to every spawned aeon's `--setenv` (dispatch.rs/summon.rs).
    pub repo_map: String,
    /// Forwarded verbatim; empty means "the system gh" (dispatch.rs).
    pub gh: String,
    /// Forwarded verbatim (dispatch.rs's `--setenv`); sentinel itself never parses it.
    pub batch_maxpar: String,
}

impl Declared {
    /// THE ONE DOOR, for every registered key `Cfg` needs: `spira_config::process::cfg`/
    /// `cfg_parse`, resolved from `$SPIRA_TOML`. No default, no fallback — a key that does
    /// not resolve is a refusal naming it, propagated to the caller (main.rs) rather than
    /// guessed at.
    pub fn resolve() -> Result<Declared, String> {
        let opt_i64 = |k: &str| -> Result<Option<i64>, String> {
            let v = spira_config::process::cfg(k)?;
            if v.trim().is_empty() {
                Ok(None)
            } else {
                v.trim()
                    .parse::<i64>()
                    .map(Some)
                    .map_err(|e| format!("{k} = {v:?} in spira.toml does not parse: {e}"))
            }
        };
        Ok(Declared {
            run: PathBuf::from(spira_config::process::cfg("SPIRA_RUN")?),
            db: spira_config::process::cfg("SPIRA_DB")?,
            bd: spira_config::process::cfg("SPIRA_BD")?,
            scope: spira_config::process::cfg("SPIRA_SCOPE_LABEL")?,
            ask: spira_config::process::cfg("SPIRA_ASK_LABEL")?,
            no_loop: spira_config::process::cfg("SPIRA_NO_LOOP_LABEL")?,
            incident_label: spira_config::process::cfg("SPIRA_INCIDENT_LABEL")?,
            queue_wait: spira_config::process::cfg("SPIRA_QUEUE_WAIT_LABEL")?,
            open_children: spira_config::process::cfg("SPIRA_OPEN_CHILDREN_LABEL")?,
            submitted: spira_config::process::cfg("SPIRA_SUBMITTED_LABEL")?,
            work_types: spira_config::process::cfg("SPIRA_WORK_CLOSE_TYPES")?
                .split_whitespace()
                .map(str::to_string)
                .collect(),
            reclaim_grace: spira_config::process::cfg_parse::<i64>("SPIRA_RECLAIM_GRACE_SECS")?,
            c5_max_file: spira_config::process::cfg_parse::<u32>("SPIRA_CHECK5_MAX_FILE")?,
            c5_max_resolve: spira_config::process::cfg_parse::<u32>("SPIRA_CHECK5_MAX_RESOLVE")?,
            land_maxsec: spira_config::process::cfg("SPIRA_LAND_MAXSEC")?,
            max_aeons: opt_i64("SPIRA_MAX_AEONS")?,
            max_live_aeons: opt_i64("SPIRA_MAX_LIVE_AEONS")?,
            lanes_max_live: opt_i64("SPIRA_LANES_MAX_LIVE")?,
            queue_throttle_override: spira_config::process::cfg("SPIRA_QUEUE_THROTTLE_OVERRIDE")?,
            summon_lock_wait: spira_config::process::cfg_parse::<u64>("SPIRA_SUMMON_LOCK_WAIT")?,
            lanes: spira_config::process::cfg("SPIRA_LANES")?,
            repo_map: spira_config::process::cfg("SPIRA_REPO_MAP")?,
            gh: spira_config::process::cfg("SPIRA_GH")?,
            batch_maxpar: spira_config::process::cfg("SPIRA_BATCH_MAXPAR")?,
        })
    }
}

#[cfg(test)]
impl Declared {
    /// Test-only literal fixture — NEVER calls `cfg()`, so building one never touches
    /// `spira_config::process::cfg`'s process-wide cache. A test overrides just the
    /// field(s) it cares about: `Declared { scope: "x".into(), ..Declared::test_default() }`.
    pub fn test_default() -> Declared {
        Declared {
            run: PathBuf::from("/tmp"),
            db: String::new(),
            bd: "bd".into(),
            scope: String::new(),
            ask: String::new(),
            no_loop: String::new(),
            incident_label: "incident".into(),
            queue_wait: String::new(),
            open_children: String::new(),
            submitted: String::new(),
            work_types: vec!["task".into(), "bug".into(), "feature".into()],
            reclaim_grace: 10800,
            c5_max_file: 5,
            c5_max_resolve: 50,
            land_maxsec: "5400".into(),
            max_aeons: None,
            max_live_aeons: None,
            lanes_max_live: None,
            queue_throttle_override: String::new(),
            summon_lock_wait: 30,
            lanes: "ops groomer qa maechen czar warden".into(),
            repo_map: String::new(),
            gh: String::new(),
            batch_maxpar: String::new(),
        }
    }
}

/// Every knob the sentinel reads, with sentinel.sh's defaults.
#[derive(Debug, Clone)]
pub struct Cfg {
    pub home: PathBuf,
    pub run: PathBuf,
    pub db: String,
    pub bd: String,
    pub bd_timeout: u64,
    pub bd_tries: u32,
    pub bd_fixture: Option<String>,
    pub scope: String,
    pub ask: String,
    pub no_loop: String,
    pub incident_label: String,
    pub queue_wait: String,
    /// SPIRA_OPEN_CHILDREN_LABEL (conf.sh defaults it); empty disables CHECK 3c.
    pub open_children: String,
    pub submitted: String,
    pub work_types: Vec<String>,
    pub poison_at: u32,
    pub requeue_at: u32,
    pub reclaim_at: u32,
    pub reclaim_grace: i64,
    pub inference_every: i64,
    pub c5_max_file: u32,
    pub c5_max_resolve: u32,
    pub c5_budget: i64,
    pub audit_unit: String,
    pub audit_maxsec: String,
    pub audit_stale: i64,
    pub audit_mailbox: PathBuf,
    pub land_unit: String,
    pub land_maxsec: String,
    pub land_stale: i64,
    /// `SPIRA_LAND_ESCALATE_EVERY` (default 3600s): `land_escalate`'s own re-ask floor,
    /// once the database has already said no ask is open (sp-31hjr — was lib.sh).
    pub land_escalate_every: i64,
    pub launch: String,
    pub systemctl: String,
    pub summon: String,
    pub skip_reclaim: bool,
    /// Spira tools, invoked by bare name on the launcher's PATH (sp-gypjk). Plain fields so a
    /// unit test can point one at a fixture; nothing reads them from the environment.
    pub tsd_bin: String,
    pub lc_bin: String,
    /// The landing worker CHECK 6 dispatches (`landing-pass land`, landing-pass/DESIGN.md §7.1).
    pub landing_bin: String,
    pub claim_bin: String,
    pub strand_bin: String,
    /// The Sending CHECK 6b runs (`sending --skip-queue`, sending/DESIGN.md).
    pub sending_bin: String,
    pub incident_sh: PathBuf,
    pub home_repo: String,
    pub fayths_str: String,
    pub pass_target: i64,
    pub poison_asked: PathBuf,
    pub requeue_asked: PathBuf,
    pub reclaim_asked: PathBuf,
    pub poison_lifted: PathBuf,
    pub roster_stamp: PathBuf,
    /// The capacity pause file (family K, wave 4.26 — owned by `aeon`; this is a pure
    /// read, never a probe: `aeon::capacity::pause_state`).
    pub capacity_pause: PathBuf,
    /// `SPIRA_DRAIN_TTL` (default 1800s): how long a drain stamp with no `expires` line of
    /// its own is honoured before `world_gate` lifts it unasked (family G, wave 4.27).
    pub drain_ttl: i64,
    /// `SPIRA_MAX_AEONS`: the task pool's ceiling. `None` (unset/empty) means no pool at
    /// all — every persona's own concurrency cap applies unchanged.
    pub max_aeons: Option<i64>,
    /// `SPIRA_MAX_LIVE_AEONS`: the whole-fleet ceiling `summon_fayth` enforces above every
    /// per-persona cap and above the pool. `None` means no ceiling (today's behaviour).
    /// `ck7_fill_cap`'s own per-persona-per-pass cap defaults this to 4 when unset.
    pub max_live_aeons: Option<i64>,
    /// `SPIRA_LANES_MAX_LIVE`: the collective cap on lane aeons. `None` means no cap.
    pub lanes_max_live: Option<i64>,
    /// `SPIRA_QUEUE_THROTTLE_OVERRIDE`: "off" pins the task pool unthrottled regardless of
    /// the admission-throttle stamp.
    pub queue_throttle_override: String,
    /// `SPIRA_THROTTLE_STAMP` (default `$SPIRA_RUN/queue-throttled`).
    pub throttle_stamp: PathBuf,
    /// `SPIRA_SENTINEL_PASS_BUDGET_SECS` (default 90s): CHECK 7's own per-partition budget,
    /// distinct from `pass_target`, the whole pass's budget.
    pub pass_budget_secs: i64,
    /// `SPIRA_SUMMON_LOCK_WAIT` (default 30s): how long `ck7_summon_pass` waits on
    /// `summon.lock` before giving up this pass to whoever already holds it.
    pub summon_lock_wait: u64,
    /// `$SPIRA_RUN/lane-round-robin`: the lane fayth summoned last pass.
    pub lane_round_robin: PathBuf,
    /// `$SPIRA_RUN/summon.lock`: the one lock every `ck7_summon_pass` caller serializes on.
    pub summon_lock: PathBuf,
    /// `SPIRA_LANES` as declared — summon.rs's own CHECK 7 lane log line. From `Declared`,
    /// never read ad hoc off `Context` (that used to default to "none" when unset; the key
    /// itself always resolves now — `spira/conf.d/SPIRA_LANES` declares the default).
    pub lanes_declared: String,
    /// For the systemd-run --setenv lists: the raw values, "" when unset.
    pub raw: BTreeMap<String, String>,
}

impl Cfg {
    /// `d` carries every REGISTERED key's declared value (`Declared::resolve` in
    /// production, a literal fixture in tests — never read here). Everything else below
    /// is genuinely NOT config: per-invocation identity (`SPIRA_HOME`, `SPIRA_REPO`...),
    /// knobs with no `spira/conf.d/` entry yet, or sentinel.sh's own bash-shaped defaults
    /// for those — left exactly as they were; migrating them is a separate, later pass.
    pub fn from_context(c: &Context, home: &Path, d: Declared) -> Cfg {
        let s = |k: &str| c.get(k).map(str::to_string).unwrap_or_default();
        // `${X:-d}`: empty means default. NOT used below for anything `d` already carries.
        let or = |k: &str, dflt: &str| c.get(k).filter(|v| !v.is_empty()).unwrap_or(dflt).to_string();
        let num = |k: &str, dflt: i64| c.get(k).and_then(|v| v.trim().parse().ok()).unwrap_or(dflt);
        let run = d.run.clone();
        let home = c
            .get("SPIRA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.to_path_buf());
        let dir = |k: &str, dflt: &str| {
            c.get(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| run.join(dflt))
        };
        let mut raw = BTreeMap::new();
        // Genuinely non-config identity/passthrough — still read off Context, unchanged.
        for k in ["PATH", "SPIRA_RELEASE", "HOME", "SPIRA_HOME", "SPIRA_REPO", "SPIRA_SKIP_RECLAIM"] {
            raw.insert(k.to_string(), s(k));
        }
        // The spec this sentinel runs under — every launch runs under the same one source.
        raw.insert("SPIRA_TOML".into(), spira_config::process::spec().unwrap_or_default());
        // Registered keys forwarded to a spawned aeon's `--setenv`: from `d`, not Context.
        raw.insert("SPIRA_RUN".into(), run.to_string_lossy().into_owned());
        raw.insert("SPIRA_DB".into(), d.db.clone());
        raw.insert("SPIRA_REPO_MAP".into(), d.repo_map.clone());
        raw.insert("SPIRA_BD".into(), d.bd.clone());
        raw.insert("SPIRA_GH".into(), d.gh.clone());
        raw.insert("SPIRA_ASK_LABEL".into(), d.ask.clone());
        raw.insert("SPIRA_SCOPE_LABEL".into(), d.scope.clone());
        raw.insert("SPIRA_WORK_CLOSE_TYPES".into(), d.work_types.join(" "));
        raw.insert("SPIRA_BATCH_MAXPAR".into(), d.batch_maxpar.clone());
        let home_repo = c
            .get("SPIRA_HOME_REPO_RESOLVED")
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                let base = c
                    .get("SPIRA_REPO")
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.clone());
                base.file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
        Cfg {
            run: run.clone(),
            db: d.db,
            bd: d.bd,
            bd_timeout: num("BD_TIMEOUT", 180).max(0) as u64,
            bd_tries: num("SPIRA_BDQ_CONN_RETRIES", 2).max(1) as u32,
            bd_fixture: c
                .get("SPIRA_BDJSON_FIXTURE")
                .filter(|v| !v.is_empty())
                .map(str::to_string),
            land_escalate_every: num("SPIRA_LAND_ESCALATE_EVERY", 3600).max(0),
            scope: d.scope,
            ask: d.ask,
            no_loop: d.no_loop,
            incident_label: d.incident_label,
            queue_wait: d.queue_wait,
            open_children: d.open_children,
            submitted: d.submitted,
            work_types: d.work_types,
            poison_at: num("SPIRA_POISON_AT", 3).max(0) as u32,
            requeue_at: num("SPIRA_REQUEUE_AT", 5).max(0) as u32,
            reclaim_at: num("SPIRA_RECLAIM_AT", 5).max(0) as u32,
            reclaim_grace: d.reclaim_grace,
            inference_every: num("SPIRA_INFERENCE_EVERY", 3600),
            c5_max_file: d.c5_max_file,
            c5_max_resolve: d.c5_max_resolve,
            c5_budget: num("SPIRA_CHECK5_BUDGET_SECS", 60),
            audit_unit: or("SPIRA_AUDIT_UNIT", "spira-audit"),
            audit_maxsec: or("SPIRA_AUDIT_MAXSEC", "1800"),
            audit_stale: num("SPIRA_AUDIT_STALE", 1800),
            audit_mailbox: c
                .get("SPIRA_AUDIT_MAILBOX")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| run.join("audit.progress")),
            land_unit: or("SPIRA_LAND_UNIT", "spira-landing"),
            land_maxsec: d.land_maxsec,
            land_stale: num("SPIRA_LAND_STALE", 1800),
            launch: or("SPIRA_LAUNCH", "systemd-run"),
            systemctl: or("SPIRA_SYSTEMCTL", "systemctl"),
            summon: or("SPIRA_SUMMON", "systemd-run"),
            skip_reclaim: c.get("SPIRA_SKIP_RECLAIM") == Some("1"),
            tsd_bin: "tsd-write".into(),
            lc_bin: "spira-lc".into(),
            landing_bin: "landing-pass".into(),
            claim_bin: "spira-claim".into(),
            strand_bin: "strand".into(),
            sending_bin: "sending".into(),
            // SPIRA_INCIDENT_SH stays a test seam (a mock incident.sh); unset, the release's
            // incident.sh by name — `bash incident.sh` finds a slashless script on PATH.
            incident_sh: c
                .get("SPIRA_INCIDENT_SH")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("incident.sh")),
            home_repo,
            fayths_str: c.fayth_names().join(" "),
            pass_target: num("SPIRA_SENTINEL_PASS_TARGET_SECS", 60),
            poison_asked: dir("SPIRA_POISON_ASKED", "poison-asked"),
            requeue_asked: dir("SPIRA_REQUEUE_ASKED", "requeue-asked"),
            reclaim_asked: dir("SPIRA_RECLAIM_ASKED", "reclaim-asked"),
            poison_lifted: dir("SPIRA_POISON_LIFTED", "poison-lifted"),
            roster_stamp: dir("SPIRA_ROSTER_WARN_STAMP", "roster-warn.stamp"),
            // literal-ok: conf.sh's default before sp-i2m7y retired the key
            capacity_pause: dir("SPIRA_CAPACITY_PAUSE", "capacity-pause"),
            drain_ttl: num("SPIRA_DRAIN_TTL", 1800),
            max_aeons: d.max_aeons,
            max_live_aeons: d.max_live_aeons,
            lanes_max_live: d.lanes_max_live,
            queue_throttle_override: d.queue_throttle_override,
            throttle_stamp: c
                .get("SPIRA_THROTTLE_STAMP")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| run.join("queue-throttled")),
            pass_budget_secs: num("SPIRA_SENTINEL_PASS_BUDGET_SECS", 90),
            summon_lock_wait: d.summon_lock_wait,
            lane_round_robin: run.join("lane-round-robin"),
            summon_lock: run.join("summon.lock"),
            lanes_declared: d.lanes,
            raw,
            home,
        }
    }

    pub fn raw(&self, k: &str) -> &str {
        self.raw.get(k).map(String::as_str).unwrap_or("")
    }

    /// `${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan` as a label set.
    pub fn plan_labels(&self) -> Vec<String> {
        let mut v = Vec::new();
        if !self.scope.is_empty() {
            v.push(self.scope.clone());
        }
        v.push("plan".into());
        v
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn probe_bytes(
        env: &[(&str, &str)],
        vars: &[(&str, &str)],
        fayths: &[&str],
        parts: &[&str],
        chamber: &[&str],
        repos: Option<&[&str]>,
    ) -> Vec<u8> {
        let mut s = String::new();
        for (k, v) in env {
            s.push_str(&format!("{k}={v}\0"));
        }
        s.push_str("@vars\0");
        for (k, v) in vars {
            s.push_str(&format!("{k}={v}\0"));
        }
        s.push_str("@fayths\0");
        for f in fayths {
            s.push_str(&format!("{f}\0"));
        }
        s.push_str("@partitions\0");
        for p in parts {
            s.push_str(&format!("{p}\0"));
        }
        s.push_str("@chamber\0");
        for c in chamber {
            s.push_str(&format!("{c}\0"));
        }
        if let Some(r) = repos {
            s.push_str("@repos\0");
            for x in r {
                s.push_str(&format!("{x}\0"));
            }
        }
        s.push_str("@end\0");
        s.into_bytes()
    }

    #[test]
    fn parses_every_section_and_requires_end() {
        let b = probe_bytes(
            &[
                ("SPIRA_RUN", "/r"),
                ("_", "/bin/bash"),
                ("SENTINEL_LIB", "/x"),
                ("MULTI", "a\nb"),
            ],
            &[("SPIRA_POISON_ASKED", "/r/pa")],
            &[
                "builder\tspira,plan\tspira-poison,ask",
                "ops\tspira,incident\t",
            ],
            &["spira,plan\tspira-poison,ask", "spira,incident\t"],
            &["builder", "ops", "spike"],
            Some(&["spira\t/src/spira\torigin/main main\t0", "other\t\t\t1"]),
        );
        let c = Context::parse(&b).unwrap();
        assert_eq!(
            c.env,
            vec![
                ("SPIRA_RUN".into(), "/r".into()),
                ("MULTI".into(), "a\nb".into())
            ]
        );
        assert_eq!(c.get("SPIRA_POISON_ASKED"), Some("/r/pa"));
        assert_eq!(c.fayths[0].labels, vec!["spira", "plan"]);
        assert!(c.fayths[1].exclude.is_empty());
        assert_eq!(c.partitions.len(), 2);
        assert_eq!(c.chamber, vec!["builder", "ops", "spike"]);
        assert_eq!(c.repo("spira").unwrap().base(), Some("origin/main"));
        assert_eq!(c.repo("spira").unwrap().landrefs.len(), 2);
        assert!(c.repo("other").unwrap().forge_queued);
        assert_eq!(c.repo("other").unwrap().root, None);
        let mut cut = b.clone();
        cut.truncate(b.len() - 5);
        assert!(Context::parse(&cut).is_err());
    }

    #[test]
    fn non_registered_fields_keep_sentinel_sh_defaults() {
        // Only identity/non-registered fields are under test here; registered fields
        // (scope, work_types, land_maxsec, ...) come from `Declared`, exercised by
        // `declared_fields_pass_through_unchanged` below instead.
        let c = Context::parse(&probe_bytes(&[], &[], &[], &[], &[], None)).unwrap();
        let d = Declared { run: PathBuf::from("/r"), ..Declared::test_default() };
        let k = Cfg::from_context(&c, Path::new("/h"), d);
        assert_eq!((k.poison_at, k.requeue_at, k.reclaim_at), (3, 5, 5));
        assert_eq!(k.audit_unit, "spira-audit");
        assert_eq!(k.audit_mailbox, PathBuf::from("/r/audit.progress"));
        assert_eq!(k.poison_asked, PathBuf::from("/r/poison-asked"));
        assert_eq!(k.home, PathBuf::from("/h"));
        assert_eq!(k.home_repo, "h");
        assert_eq!(k.incident_sh, PathBuf::from("incident.sh"));
        assert_eq!((k.lc_bin.as_str(), k.claim_bin.as_str(), k.strand_bin.as_str(), k.landing_bin.as_str(), k.tsd_bin.as_str(), k.sending_bin.as_str()), ("spira-lc", "spira-claim", "strand", "landing-pass", "tsd-write", "sending"));
        assert_eq!(k.pass_target, 60);
    }

    #[test]
    fn declared_fields_pass_through_unchanged() {
        // Registered keys: `Cfg::from_context` must take `Declared`'s values as given,
        // with no Rust-side default or fallback of its own (one source of config).
        let c = Context::parse(&probe_bytes(&[], &[], &[], &[], &[], None)).unwrap();
        let k = Cfg::from_context(&c, Path::new("/h"), Declared::test_default());
        assert_eq!(k.work_types, vec!["task", "bug", "feature"]);
        assert_eq!(k.land_maxsec, "5400");
        assert_eq!(k.plan_labels(), vec!["plan"]);
        assert_eq!(k.lanes_declared, "ops groomer qa maechen czar warden");
        assert_eq!(k.reclaim_grace, 10800);
        assert_eq!((k.c5_max_file, k.c5_max_resolve), (5, 50));
        assert_eq!(k.summon_lock_wait, 30);
        assert_eq!((k.max_aeons, k.max_live_aeons, k.lanes_max_live), (None, None, None));
        assert_eq!(k.raw.get("SPIRA_ASK_LABEL").map(String::as_str), Some(""));
    }
}
