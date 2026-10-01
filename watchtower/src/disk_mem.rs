//! Root disk and memory. A full root disk kills every process on the box, not only
//! Spira's; `SPIRA_MEMINFO_PATH` is a seam for tests the same way `SPIRA_INCIDENT_SH` and
//! `SPIRA_SUITES_SH` are — `/proc/meminfo` cannot be stubbed by PATH the way `df` can.

use std::path::Path;

pub struct Cfg {
    pub disk_warn_pct: i64,
    pub mem_warn_mb: i64,
    pub df_bin: String,
}

impl Default for Cfg {
    fn default() -> Self {
        Cfg {
            disk_warn_pct: 90,
            mem_warn_mb: 1500,
            df_bin: "df".to_string(),
        }
    }
}

pub struct DiskMem {
    pub disk_breach: bool,
    pub disk_disp: String,
    pub mem_breach: bool,
    pub mem_disp: String,
}

/// `MemAvailable` from `/proc/meminfo` (kB), converted to whole MB by integer truncation —
/// the same `awk '{printf "%d", $2/1024}'`.
pub fn mem_available_mb(meminfo_path: &Path) -> Option<i64> {
    let text = std::fs::read_to_string(meminfo_path).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            let kb: i64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb / 1024);
        }
    }
    None
}

/// `df --output=pcent /`, digits only — matches the bash's `tail -1 | tr -dc '0-9'`.
pub fn disk_root_pct(df_bin: &str) -> Option<i64> {
    let out = std::process::Command::new(df_bin)
        .args(["--output=pcent", "/"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let last = text.lines().last()?;
    let digits: String = last.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

pub fn render(disk_pct: Option<i64>, mem_mb: Option<i64>, cfg: &Cfg) -> DiskMem {
    let disk_breach = disk_pct.map(|p| p >= cfg.disk_warn_pct).unwrap_or(false);
    let disk_disp = match disk_pct {
        None => "?".to_string(),
        Some(p) if disk_breach => format!("FAULT ({p}%, warn at {}%)", cfg.disk_warn_pct),
        Some(p) => format!("{p}%"),
    };
    let mem_breach = mem_mb.map(|m| m < cfg.mem_warn_mb).unwrap_or(false);
    let mem_disp = match mem_mb {
        None => "?".to_string(),
        Some(m) if mem_breach => format!("FAULT ({m}MB, warn below {}MB)", cfg.mem_warn_mb),
        Some(m) => format!("{m}MB"),
    };
    DiskMem {
        disk_breach,
        disk_disp,
        mem_breach,
        mem_disp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_available_mb_reads_the_fixture() {
        let d = testkit::TempDir::new("wt-meminfo");
        let p = d.join("meminfo");
        std::fs::write(&p, "MemTotal:       16384000 kB\nMemAvailable:    2048000 kB\n").unwrap();
        assert_eq!(mem_available_mb(&p), Some(2000));
    }

    #[test]
    fn mem_available_mb_is_none_when_the_key_is_absent() {
        let d = testkit::TempDir::new("wt-meminfo-missing-key");
        let p = d.join("meminfo");
        std::fs::write(&p, "MemTotal: 100 kB\n").unwrap();
        assert_eq!(mem_available_mb(&p), None);
    }

    #[test]
    fn render_flags_breach_only_at_or_over_the_disk_threshold() {
        let cfg = Cfg::default();
        let r = render(Some(90), Some(2000), &cfg);
        assert!(r.disk_breach);
        assert_eq!(r.disk_disp, "FAULT (90%, warn at 90%)");
        let r2 = render(Some(89), Some(2000), &cfg);
        assert!(!r2.disk_breach);
        assert_eq!(r2.disk_disp, "89%");
    }

    #[test]
    fn render_flags_mem_breach_strictly_below_threshold() {
        let cfg = Cfg::default();
        let r = render(Some(10), Some(1499), &cfg);
        assert!(r.mem_breach);
        let r2 = render(Some(10), Some(1500), &cfg);
        assert!(!r2.mem_breach);
    }

    #[test]
    fn render_is_unknown_never_zero_on_a_failed_read() {
        let cfg = Cfg::default();
        let r = render(None, None, &cfg);
        assert!(!r.disk_breach);
        assert!(!r.mem_breach);
        assert_eq!(r.disk_disp, "?");
        assert_eq!(r.mem_disp, "?");
    }
}
