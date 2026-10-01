//! Configuration: the context probe's answer (DESIGN.md §6, S0) turned into typed values.
//! conf.sh resolves every key through spira-config; this module never reads spira.toml.

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
    pub queued: bool,
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

/// Where the lifecycle machine's records are the truth, or the legacy ones are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    /// `lifecycle_enforce` off (production today): bd status/labels, landstate files and the
    /// bd events trail. The sentinel never calls spira-lc, and nothing it starts can.
    Off,
    /// `lifecycle_enforce` on: spira-lc is authoritative; an unreachable machine is an error.
    On,
}

/// `lifecycle_enforce`, resolved as the aeon crate resolves it (concierge/rw-aeon
/// aeon/src/conf.rs): the unit's own environment wins (`SPIRA_LIFECYCLE_ENFORCE`, 1/true =
/// on, anything else = off) — read from this process's ORIGINAL environment, because
/// conf.sh defaults it to 0 and would mask the toml; else `spira.lifecycle_enforce` in the
/// spira.toml conf.sh resolved (`SPIRA_TOML_FILE`), read through the spira-config library;
/// else off. Whether spira-lc happens to be installed is never consulted.
pub fn lifecycle_enforce(original: Option<&str>, toml_file: Option<&Path>) -> Lifecycle {
    if let Some(v) = original {
        return if v == "1" || v == "true" {
            Lifecycle::On
        } else {
            Lifecycle::Off
        };
    }
    let Some(p) = toml_file.filter(|p| p.is_file()) else {
        return Lifecycle::Off;
    };
    match spira_config::load(p) {
        Ok(doc)
            if doc
                .spira
                .as_ref()
                .and_then(|s| s.lifecycle_enforce)
                .unwrap_or(false) =>
        {
            Lifecycle::On
        }
        _ => Lifecycle::Off,
    }
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
                            queued: f.get(3).copied() == Some("1"),
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
    pub skip_closed: bool,
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
    /// OFF mode's CHECK 2 protection label (the retired SPIRA_RECLAIM_SKIP_LABEL).
    pub reclaim_skip_label: String,
    /// The capacity pause file (family K, wave 4.26 — owned by `aeon`; this is a pure
    /// read, never a probe: `aeon::capacity::pause_state`).
    pub capacity_pause: PathBuf,
    /// For the systemd-run --setenv lists: the raw values, "" when unset.
    pub raw: BTreeMap<String, String>,
}

impl Cfg {
    pub fn from_context(c: &Context, home: &Path) -> Cfg {
        let s = |k: &str| c.get(k).map(str::to_string).unwrap_or_default();
        // `${X:-d}`: empty means default.
        let or = |k: &str, d: &str| c.get(k).filter(|v| !v.is_empty()).unwrap_or(d).to_string();
        let num = |k: &str, d: i64| c.get(k).and_then(|v| v.trim().parse().ok()).unwrap_or(d);
        let run = PathBuf::from(or("SPIRA_RUN", "/tmp"));
        let home = c
            .get("SPIRA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.to_path_buf());
        let dir = |k: &str, d: &str| {
            c.get(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| run.join(d))
        };
        let mut raw = BTreeMap::new();
        for k in [
            "PATH",
            "SPIRA_RELEASE",
            "HOME",
            "SPIRA_HOME",
            "SPIRA_RUN",
            "SPIRA_DB",
            "SPIRA_REPO",
            "SPIRA_REPO_MAP",
            "SPIRA_BD",
            "SPIRA_GH",
            "SPIRA_ASK_LABEL",
            "SPIRA_SCOPE_LABEL",
            "SPIRA_WORK_CLOSE_TYPES",
            "SPIRA_BATCH_MAXPAR",
            "SPIRA_SKIP_CLOSED_CHECK",
            "SPIRA_SKIP_RECLAIM",
        ] {
            raw.insert(k.to_string(), s(k));
        }
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
            db: s("SPIRA_DB"),
            bd: or("SPIRA_BD", "bd"),
            bd_timeout: num("BD_TIMEOUT", 180).max(0) as u64,
            bd_tries: num("SPIRA_BDQ_CONN_RETRIES", 2).max(1) as u32,
            bd_fixture: c
                .get("SPIRA_BDJSON_FIXTURE")
                .filter(|v| !v.is_empty())
                .map(str::to_string),
            land_escalate_every: num("SPIRA_LAND_ESCALATE_EVERY", 3600).max(0),
            scope: s("SPIRA_SCOPE_LABEL"),
            ask: s("SPIRA_ASK_LABEL"),
            no_loop: s("SPIRA_NO_LOOP_LABEL"),
            incident_label: or("SPIRA_INCIDENT_LABEL", "incident"),
            queue_wait: s("SPIRA_QUEUE_WAIT_LABEL"),
            open_children: s("SPIRA_OPEN_CHILDREN_LABEL"),
            submitted: s("SPIRA_SUBMITTED_LABEL"),
            work_types: or("SPIRA_WORK_CLOSE_TYPES", "task bug feature")
                .split_whitespace()
                .map(str::to_string)
                .collect(),
            poison_at: num("SPIRA_POISON_AT", 3).max(0) as u32,
            requeue_at: num("SPIRA_REQUEUE_AT", 5).max(0) as u32,
            reclaim_at: num("SPIRA_RECLAIM_AT", 5).max(0) as u32,
            reclaim_grace: num("SPIRA_RECLAIM_GRACE_SECS", 10800),
            inference_every: num("SPIRA_INFERENCE_EVERY", 3600),
            c5_max_file: num("SPIRA_CHECK5_MAX_FILE", 5).max(0) as u32,
            c5_max_resolve: num("SPIRA_CHECK5_MAX_RESOLVE", 50).max(0) as u32,
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
            land_maxsec: or("SPIRA_LAND_MAXSEC", "3600"),
            land_stale: num("SPIRA_LAND_STALE", 1800),
            launch: or("SPIRA_LAUNCH", "systemd-run"),
            systemctl: or("SPIRA_SYSTEMCTL", "systemctl"),
            summon: or("SPIRA_SUMMON", "systemd-run"),
            skip_reclaim: c.get("SPIRA_SKIP_RECLAIM") == Some("1"),
            skip_closed: c.get("SPIRA_SKIP_CLOSED_CHECK") == Some("1"),
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
            reclaim_skip_label: or("SPIRA_RECLAIM_SKIP_LABEL", "spira-waiting-operator"),
            capacity_pause: dir("SPIRA_CAPACITY_PAUSE", "capacity-pause"),
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
        assert!(c.repo("other").unwrap().queued);
        assert_eq!(c.repo("other").unwrap().root, None);
        let mut cut = b.clone();
        cut.truncate(b.len() - 5);
        assert!(Context::parse(&cut).is_err());
    }

    #[test]
    fn the_switch_resolves_like_the_aeon_crate() {
        let dir = testkit::TempDir::new("sentinel-enforce");
        let toml = dir.join("spira.toml");
        std::fs::write(&toml, "[spira]\nlifecycle_enforce = true\n").unwrap();
        assert_eq!(lifecycle_enforce(None, Some(&toml)), Lifecycle::On);
        assert_eq!(
            lifecycle_enforce(Some("0"), Some(&toml)),
            Lifecycle::Off,
            "the unit env wins"
        );
        std::fs::write(&toml, "[spira]\n").unwrap();
        assert_eq!(lifecycle_enforce(None, Some(&toml)), Lifecycle::Off);
        assert_eq!(lifecycle_enforce(Some("1"), Some(&toml)), Lifecycle::On);
        assert_eq!(lifecycle_enforce(Some("true"), None), Lifecycle::On);
        assert_eq!(lifecycle_enforce(Some("yes"), None), Lifecycle::Off);
        assert_eq!(lifecycle_enforce(None, None), Lifecycle::Off);
        assert_eq!(
            lifecycle_enforce(None, Some(&dir.join("absent.toml"))),
            Lifecycle::Off
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn defaults_are_sentinel_sh_defaults() {
        let c = Context::parse(&probe_bytes(
            &[("SPIRA_RUN", "/r"), ("SPIRA_SCOPE_LABEL", "")],
            &[],
            &[],
            &[],
            &[],
            None,
        ))
        .unwrap();
        let k = Cfg::from_context(&c, Path::new("/h"));
        assert_eq!((k.poison_at, k.requeue_at, k.reclaim_at), (3, 5, 5));
        assert_eq!(k.work_types, vec!["task", "bug", "feature"]);
        assert_eq!(k.audit_unit, "spira-audit");
        assert_eq!(k.land_maxsec, "3600");
        assert_eq!(k.audit_mailbox, PathBuf::from("/r/audit.progress"));
        assert_eq!(k.poison_asked, PathBuf::from("/r/poison-asked"));
        assert_eq!(k.plan_labels(), vec!["plan"]);
        assert_eq!(k.home, PathBuf::from("/h"));
        assert_eq!(k.home_repo, "h");
        assert_eq!(k.incident_sh, PathBuf::from("incident.sh"));
        assert_eq!((k.lc_bin.as_str(), k.claim_bin.as_str(), k.strand_bin.as_str(), k.landing_bin.as_str(), k.tsd_bin.as_str(), k.sending_bin.as_str()), ("spira-lc", "spira-claim", "strand", "landing-pass", "tsd-write", "sending"));
        assert_eq!(k.pass_target, 60);
    }
}
