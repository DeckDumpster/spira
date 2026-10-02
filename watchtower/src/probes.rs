//! `--conditions-check` — the four standing conditions someone used to watch by hand:
//! a prod unit off the current release, a unit failing run after run, the box short of
//! room or under sustained pressure, and a release store grown past its keep. Each probe
//! gathers, decides purely, and hands [`conditions`](crate::conditions) what stands.

use crate::conditions::{Cond, Reading};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

pub struct Cfg {
    pub systemctl: String,
    pub journalctl: String,
    pub releases: Option<String>,
    pub keep: usize,
    pub store_slack: usize,
    pub unit_glob: String,
    pub stale_release_secs: i64,
    pub failed_runs: i64,
    pub tmp_path: String,
    pub tmp_floor_mib: i64,
    pub df_bin: String,
    pub disk_floor_pct: i64,
    pub psi_dir: String,
    pub psi_full_avg60: f64,
    pub psi_sustain_secs: i64,
}

fn systemctl_show(cfg: &Cfg, args: &[&str]) -> Option<String> {
    let out = crate::deadline::output("conditions systemctl", Command::new(&cfg.systemctl).arg("--user").arg("show").args(args)).ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn sha_of(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path)
}

// ---- (a) units whose rendered release is not current -------------------------------

pub fn units_off_current(show: &str, current: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for block in show.split("\n\n") {
        let id = block.lines().find_map(|l| l.strip_prefix("Id="));
        let release = block
            .lines()
            .filter(|l| l.starts_with("Environment="))
            .flat_map(|l| l.split_whitespace())
            .find_map(|w| w.trim_start_matches("Environment=").trim_matches(['"', '\'']).strip_prefix("SPIRA_RELEASE="));
        if let (Some(id), Some(r)) = (id, release) {
            let sha = sha_of(r);
            if sha != current {
                out.push((id.to_string(), sha.to_string()));
            }
        }
    }
    out
}

pub fn release_currency(cfg: &Cfg) -> Reading {
    let Some(rel) = &cfg.releases else { return Reading::Unknown };
    let Some(current) = std::fs::read_link(Path::new(rel).join("current")).ok().map(|t| sha_of(&t.to_string_lossy()).to_string()) else {
        return Reading::Unknown;
    };
    let Some(show) = systemctl_show(cfg, &[&cfg.unit_glob, "-p", "Id", "-p", "Environment"]) else { return Reading::Unknown };
    Reading::Standing(
        units_off_current(&show, &current)
            .into_iter()
            .map(|(unit, sha)| Cond {
                key: unit.clone(),
                reference: format!("incident:release-stale-{unit}"),
                title: format!("STALE RELEASE: {unit} renders {sha}, current is {current}"),
                body: format!("{unit} has rendered release {sha} for over {}s; the current release is {current}.\n\nLanded fixes are not in force in this unit. Re-render and restart it (`release status`, then `release activate {current}`).\n", cfg.stale_release_secs),
                priority: 1,
                sustain_secs: cfg.stale_release_secs,
            })
            .collect(),
    )
}

// ---- (b) a unit failed for N consecutive runs ---------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub active_state: String,
    pub invocation: String,
}

/// `Some((invocation, count))` is the new tally; `None` drops the unit.
pub fn tally(prev: Option<(&str, i64)>, run: &Run) -> Option<(String, i64)> {
    match run.active_state.as_str() {
        "failed" => match prev {
            Some((inv, n)) if inv == run.invocation => Some((inv.to_string(), n)),
            Some((_, n)) => Some((run.invocation.clone(), n + 1)),
            None => Some((run.invocation.clone(), 1)),
        },
        "activating" | "deactivating" | "reloading" => prev.map(|(i, n)| (i.to_string(), n)),
        _ => None,
    }
}

fn parse_tally(text: &str) -> BTreeMap<String, (String, i64)> {
    text.lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.split_whitespace().collect();
            if p.len() != 3 {
                return None;
            }
            Some((p[0].to_string(), (p[1].to_string(), p[2].parse().ok()?)))
        })
        .collect()
}

fn render_tally(m: &BTreeMap<String, (String, i64)>) -> String {
    m.iter().fold(String::new(), |mut acc, (u, (i, n))| {
        acc.push_str(&format!("{u} {i} {n}\n"));
        acc
    })
}

fn unit_run(cfg: &Cfg, unit: &str) -> Option<Run> {
    let show = systemctl_show(cfg, &[unit, "-p", "ActiveState", "-p", "InvocationID"])?;
    let get = |k: &str| show.lines().find_map(|l| l.strip_prefix(k)).unwrap_or("").trim().to_string();
    Some(Run { active_state: get("ActiveState="), invocation: get("InvocationID=") })
}

fn last_log_lines(cfg: &Cfg, unit: &str) -> String {
    crate::deadline::output("conditions journal", Command::new(&cfg.journalctl).args(["--user", "-u", unit, "-n", "5", "--no-pager"]))
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr))
        .unwrap_or_default()
}

pub fn failing_units(cfg: &Cfg, run_dir: &Path) -> Reading {
    let Some(failed) = crate::failed_units::gather(&cfg.systemctl) else { return Reading::Unknown };
    let path = run_dir.join("conditions").join("failed-runs.tally");
    let prev = parse_tally(&std::fs::read_to_string(&path).unwrap_or_default());
    let mut units: Vec<String> = failed;
    for u in prev.keys() {
        if !units.contains(u) {
            units.push(u.clone());
        }
    }
    let mut next = BTreeMap::new();
    let mut standing = Vec::new();
    for unit in units {
        let Some(run) = unit_run(cfg, &unit) else {
            if let Some(p) = prev.get(&unit) {
                next.insert(unit, p.clone());
            }
            continue;
        };
        let Some((inv, n)) = tally(prev.get(&unit).map(|(i, n)| (i.as_str(), *n)), &run) else { continue };
        if n >= cfg.failed_runs {
            standing.push(Cond {
                key: unit.clone(),
                reference: format!("incident:failed-unit-{unit}"),
                title: format!("FAILED UNIT: {unit} failed {n} consecutive runs"),
                body: format!("{unit} has failed {n} consecutive runs.\n\nLast log lines:\n{}\n\nCheck: journalctl --user -u {unit} -n 50\n", last_log_lines(cfg, &unit)),
                priority: 1,
                sustain_secs: 0,
            });
        }
        next.insert(unit, (inv, n));
    }
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let _ = std::fs::write(&path, render_tally(&next));
    Reading::Standing(standing)
}

// ---- (c) room and pressure ----------------------------------------------------------

pub fn df_avail_mib_and_used_pct(df_out: &str) -> Option<(i64, i64)> {
    let line = df_out.lines().last()?;
    let mut f = line.split_whitespace();
    let avail: i64 = f.next()?.trim_end_matches('M').parse().ok()?;
    let pct: i64 = f.next()?.trim_end_matches('%').parse().ok()?;
    Some((avail, pct))
}

fn df(cfg: &Cfg, path: &str) -> Option<(i64, i64)> {
    let out = crate::deadline::output("conditions df", Command::new(&cfg.df_bin).args(["--output=avail,pcent", "-BM", path])).ok()?;
    out.status.success().then(|| df_avail_mib_and_used_pct(&String::from_utf8_lossy(&out.stdout))).flatten()
}

/// The `full avg60` of a `/proc/pressure/<resource>` file.
pub fn psi_full_avg60(text: &str) -> Option<f64> {
    let line = text.lines().find(|l| l.starts_with("full "))?;
    line.split_whitespace().find_map(|w| w.strip_prefix("avg60="))?.parse().ok()
}

fn cond(key: &str, title: String, body: String, sustain: i64) -> Cond {
    Cond { key: key.into(), reference: format!("incident:pressure-{key}"), title, body, priority: 1, sustain_secs: sustain }
}

pub fn pressure(cfg: &Cfg) -> Reading {
    let mut standing = Vec::new();
    let mut readable = false;
    if let Some((avail, _)) = df(cfg, &cfg.tmp_path) {
        readable = true;
        if avail < cfg.tmp_floor_mib {
            standing.push(cond(
                "tmp-free",
                format!("LOW SPACE: {} has {avail} MiB free, below the {} MiB testenv slots need", cfg.tmp_path, cfg.tmp_floor_mib),
                format!("{} has {avail} MiB free; warm testenv slots shed below {} MiB and suites fault on quota.\n\nFind what is holding it: `du -xsh {}/* | sort -h | tail`.\n", cfg.tmp_path, cfg.tmp_floor_mib, cfg.tmp_path),
                0,
            ));
        }
    }
    if let Some((_, used)) = df(cfg, "/") {
        readable = true;
        let free = 100 - used;
        if free < cfg.disk_floor_pct {
            standing.push(cond(
                "root-disk",
                format!("LOW DISK: root has {free}% free, below the {}% floor", cfg.disk_floor_pct),
                format!("The root filesystem has {free}% free (floor {}%).\n\nCheck release trees, worktrees and logs: `du -xh --max-depth=2 / | sort -h | tail -30`.\n", cfg.disk_floor_pct),
                0,
            ));
        }
    }
    for res in ["io", "memory"] {
        if let Some(v) = std::fs::read_to_string(Path::new(&cfg.psi_dir).join(res)).ok().and_then(|t| psi_full_avg60(&t)) {
            readable = true;
            if v >= cfg.psi_full_avg60 {
                standing.push(cond(
                    &format!("psi-{res}"),
                    format!("PRESSURE: {res} full avg60 {v} for over {}s", cfg.psi_sustain_secs),
                    format!("{res} pressure (PSI full avg60) is {v}, at or above {}, and has been for over {}s.\n\nSee what is running: `systemd-cgtop`, `cat /proc/pressure/{res}`.\n", cfg.psi_full_avg60, cfg.psi_sustain_secs),
                    cfg.psi_sustain_secs,
                ));
            }
        }
    }
    if readable {
        Reading::Standing(standing)
    } else {
        Reading::Unknown
    }
}

// ---- (d) the release store ----------------------------------------------------------

pub fn store_count(releases: &Path) -> Option<usize> {
    let rd = std::fs::read_dir(releases).ok()?;
    Some(
        rd.flatten()
            .filter(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.len() == 40 && n.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) && e.file_type().map(|t| t.is_dir()).unwrap_or(false)
            })
            .count(),
    )
}

pub fn release_store(cfg: &Cfg) -> Reading {
    let Some(count) = cfg.releases.as_deref().and_then(|r| store_count(Path::new(r))) else { return Reading::Unknown };
    let limit = cfg.keep + cfg.store_slack;
    Reading::Standing(if count > limit {
        vec![Cond {
            key: "release-store".into(),
            reference: "incident:release-store-growth".into(),
            title: format!("RELEASE STORE: {count} trees held, keep is {} (+{} slack)", cfg.keep, cfg.store_slack),
            body: format!("The release store holds {count} release trees; prune keeps {} and the alarm allows {} more.\n\nPrune is not keeping up: run `release prune` and read why it failed.\n", cfg.keep, cfg.store_slack),
            priority: 2,
            sustain_secs: 0,
        }]
    } else {
        Vec::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_off_current_names_only_units_rendering_another_release() {
        let show = "Id=spira-a-prod.service\nEnvironment=SPIRA_RELEASE=/r/aaa PATH=/x\n\nId=spira-b-prod.service\nEnvironment=SPIRA_RELEASE=/r/bbb\n\nId=spira-c-prod.service\nEnvironment=\n";
        assert_eq!(units_off_current(show, "bbb"), vec![("spira-a-prod.service".to_string(), "aaa".to_string())]);
        assert!(units_off_current(show, "aaa").iter().any(|(u, _)| u == "spira-b-prod.service"));
    }

    fn run(state: &str, inv: &str) -> Run {
        Run { active_state: state.into(), invocation: inv.into() }
    }

    #[test]
    fn tally_counts_each_new_failed_invocation_once() {
        assert_eq!(tally(None, &run("failed", "i1")), Some(("i1".into(), 1)));
        assert_eq!(tally(Some(("i1", 1)), &run("failed", "i1")), Some(("i1".into(), 1)), "the same failed run seen twice is one run");
        assert_eq!(tally(Some(("i1", 1)), &run("failed", "i2")), Some(("i2".into(), 2)));
    }

    #[test]
    fn tally_survives_a_restart_in_flight_and_resets_on_success() {
        assert_eq!(tally(Some(("i1", 2)), &run("activating", "i2")), Some(("i1".into(), 2)));
        assert_eq!(tally(Some(("i1", 2)), &run("inactive", "i2")), None);
        assert_eq!(tally(Some(("i1", 2)), &run("active", "i2")), None);
    }

    #[test]
    fn tally_file_round_trips() {
        let mut m = BTreeMap::new();
        m.insert("u.service".to_string(), ("abc".to_string(), 3));
        assert_eq!(parse_tally(&render_tally(&m)), m);
    }

    #[test]
    fn df_and_psi_parse_the_real_shapes() {
        assert_eq!(df_avail_mib_and_used_pct("Avail Use%\n 5000M  83%\n"), Some((5000, 83)));
        assert_eq!(df_avail_mib_and_used_pct("Avail Use%\n"), None);
        let psi = "some avg10=1.00 avg60=2.00 avg300=0.50 total=1\nfull avg10=0.00 avg60=12.34 avg300=1.00 total=1\n";
        assert_eq!(psi_full_avg60(psi), Some(12.34));
        assert_eq!(psi_full_avg60("some avg60=9.0\n"), None);
    }

    fn cfg(dir: &Path) -> Cfg {
        Cfg {
            systemctl: "/nonexistent/systemctl".into(),
            journalctl: "/nonexistent/journalctl".into(),
            releases: Some(dir.join("releases").to_string_lossy().into_owned()),
            keep: 2,
            store_slack: 1,
            unit_glob: "spira-*-prod.service".into(),
            stale_release_secs: 3600,
            failed_runs: 3,
            tmp_path: "/tmp".into(),
            tmp_floor_mib: 100,
            df_bin: "/nonexistent/df".into(),
            disk_floor_pct: 15,
            psi_dir: dir.join("psi").to_string_lossy().into_owned(),
            psi_full_avg60: 10.0,
            psi_sustain_secs: 300,
        }
    }

    fn standing(r: Reading) -> Vec<Cond> {
        match r {
            Reading::Standing(c) => c,
            Reading::Unknown => panic!("probe read as unknown"),
        }
    }

    #[test]
    fn release_store_crosses_only_past_keep_plus_slack() {
        let d = testkit::TempDir::new("wt-store");
        let c = cfg(&d);
        for i in 0..3 {
            std::fs::create_dir_all(d.join(format!("releases/{:040x}", i))).unwrap();
        }
        std::fs::create_dir_all(d.join("releases/not-a-sha")).unwrap();
        assert!(standing(release_store(&c)).is_empty(), "3 trees is keep 2 + slack 1");
        std::fs::create_dir_all(d.join(format!("releases/{:040x}", 9))).unwrap();
        let s = standing(release_store(&c));
        assert_eq!(s.len(), 1);
        assert!(s[0].title.contains("4 trees"));
    }

    #[test]
    fn release_store_is_unknown_when_the_directory_cannot_be_read() {
        let d = testkit::TempDir::new("wt-store-missing");
        assert!(matches!(release_store(&cfg(&d)), Reading::Unknown));
    }

    #[test]
    fn pressure_reads_psi_against_its_threshold_and_is_unknown_when_nothing_reads() {
        let d = testkit::TempDir::new("wt-psi");
        let c = cfg(&d);
        assert!(matches!(pressure(&c), Reading::Unknown));
        std::fs::create_dir_all(d.join("psi")).unwrap();
        std::fs::write(d.join("psi/io"), "full avg10=0 avg60=9.99 avg300=0 total=0\n").unwrap();
        std::fs::write(d.join("psi/memory"), "full avg10=0 avg60=10.00 avg300=0 total=0\n").unwrap();
        let s = standing(pressure(&c));
        assert_eq!(s.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(), vec!["psi-memory"]);
        assert_eq!(s[0].sustain_secs, 300);
    }

    #[test]
    fn pressure_flags_tmp_and_root_through_a_fake_df() {
        use std::os::unix::fs::PermissionsExt;
        let d = testkit::TempDir::new("wt-df");
        let mut c = cfg(&d);
        let df = d.join("df");
        std::fs::write(&df, "#!/bin/bash\necho 'Avail Use%'\nif [ \"${@: -1}\" = / ]; then echo '90000M  92%'; else echo '50M  10%'; fi\n").unwrap();
        std::fs::set_permissions(&df, std::fs::Permissions::from_mode(0o755)).unwrap();
        c.df_bin = df.to_string_lossy().into_owned();
        let keys: Vec<String> = standing(pressure(&c)).into_iter().map(|c| c.key).collect();
        assert_eq!(keys, vec!["tmp-free", "root-disk"]);
    }
}
