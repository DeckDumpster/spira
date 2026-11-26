//! Which unit a cgroup path belongs to, and the comm labels a series carries.

use runtime::units::{dwarf_rune, parse_unit_name, DWARF_PREFIX, DWARF_TASK, PREFIX};

pub const SYSTEM: &str = "system";
pub const UNRESOLVED: &str = "unresolved";
const ROUND_SCOPE: &str = "spira-round-";

/// A unit name with each launch-specific id cut off: kept up to the first `-` segment holding a
/// digit, so the label set stays bounded.
fn bounded(name: &str) -> String {
    let (stem, suffix) = name.rsplit_once('.').map_or((name, ""), |(s, x)| (s, x));
    if !stem.chars().any(|c| c.is_ascii_digit()) {
        return name.to_string();
    }
    let kept: Vec<&str> = stem.split('-').take_while(|s| !s.chars().any(|c| c.is_ascii_digit())).collect();
    let head = if kept.is_empty() {
        let first = stem.split('-').next().unwrap_or("");
        first.split(|c: char| c.is_ascii_digit()).next().unwrap_or("").trim_end_matches(['@', '_', '.']).to_string()
    } else {
        kept.join("-")
    };
    format!("{head}.{suffix}")
}

fn spira_run(name: &str) -> Option<(String, String)> {
    let (task, key) = parse_unit_name(name)?;
    let class = if task == DWARF_TASK { format!("dwarf-{}", dwarf_rune(name)?) } else { task };
    let slot = key.split('/').next().filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && key.contains('/'));
    let unit = slot.map_or_else(|| class.clone(), |n| format!("{class}-{n}"));
    Some((class, unit))
}

/// `(unit_class, unit)` of a cgroup path such as `/user.slice/…/spira-run-gate.worker-2_….service`.
pub fn classify(cgroup_path: &str) -> (String, String) {
    let segs: Vec<&str> = cgroup_path.split('/').filter(|s| !s.is_empty()).collect();
    if let Some(name) = segs.iter().rev().find(|s| s.ends_with(".service") || s.ends_with(".scope")) {
        let is_service = name.ends_with(".service");
        if is_service && name.starts_with(PREFIX) {
            if let Some(found) = spira_run(name) {
                return found;
            }
        }
        if is_service && name.starts_with(DWARF_PREFIX) {
            if let Some(rune) = name[DWARF_PREFIX.len()..].split('-').next().filter(|r| !r.is_empty()) {
                let class = format!("dwarf-{rune}");
                return (class.clone(), class);
            }
        }
        if !is_service && name.starts_with(ROUND_SCOPE) {
            return ("round".into(), "round".into());
        }
        let unit = bounded(name);
        return if is_service { (unit.clone(), unit) } else { ("scope".into(), unit) };
    }
    let slice = segs.iter().rev().find(|s| s.ends_with(".slice")).map_or_else(|| "-.slice".to_string(), |s| bounded(s));
    (SYSTEM.into(), slice)
}

/// A test binary's comm ends in its hash (`round_vm-304631`, cut at 15 bytes); every build would be a label.
pub fn fold_program(comm: &str) -> String {
    match comm.rsplit_once('-') {
        Some((stem, hash)) if !stem.is_empty() && hash.len() >= 6 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => format!("{stem}-*"),
        _ => comm.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is(path: &str, class: &str, unit: &str) {
        assert_eq!(classify(path), (class.to_string(), unit.to_string()), "{path}");
    }

    const APP: &str = "/user.slice/user-1000.slice/user@1000.service/app.slice"; // not a path: a cgroup path

    #[test_id(t4nrykk)]
    #[test]
    fn a_gate_worker_unit_is_its_class_and_its_slot() {
        is(&format!("{APP}/spira-run-gate.worker-2_2fgate_2fspira_2fspira_2fsp_2d9npvpj_2fabc.service"), "gate.worker", "gate.worker-2");
    }

    #[test_id(tmxst3z)]
    #[test]
    fn a_batcher_unit_is_its_class_alone() {
        is(&format!("{APP}/spira-run-batcher.local-spira.49b8ff96a0.service"), "batcher.local", "batcher.local");
    }

    #[test_id(t4ubj8z)]
    #[test]
    fn a_dwarf_started_by_the_scheduler_is_classed_by_its_rune() {
        is(&format!("{APP}/spira-run-sentinel_2dcall-builder_2d1795123456_2d3.service"), "dwarf-builder", "dwarf-builder");
    }

    #[test_id(t1s36i4)]
    #[test]
    fn a_dwarf_started_by_name_is_classed_by_its_rune() {
        is(&format!("{APP}/spira-dwarf-builder-directed-1795.service"), "dwarf-builder", "dwarf-builder");
    }

    #[test_id(tmdswcg)]
    #[test]
    fn a_round_scope_is_the_round() {
        is(&format!("{APP}/spira-round-spira.49b8ff96a0-1.scope"), "round", "round");
    }

    #[test_id(to23hcu)]
    #[test]
    fn a_named_service_is_itself() {
        is(&format!("{APP}/spira-runtime-prod.service"), "spira-runtime-prod.service", "spira-runtime-prod.service");
    }

    #[test_id(tfo41up)]
    #[test]
    fn a_scope_is_cut_at_the_first_segment_holding_a_digit() {
        is(&format!("{APP}/tmux-spawn-cab2ff08-1f2e-4a.scope"), "scope", "tmux-spawn.scope");
    }

    #[test_id(tu7468t)]
    #[test]
    fn a_transient_service_loses_its_digits() {
        is(&format!("{APP}/run-p13533-i29036.service"), "run.service", "run.service");
    }

    #[test_id(tr3y3f7)]
    #[test]
    fn the_root_cgroup_is_system() {
        is("/", SYSTEM, "-.slice");
        is("/user.slice/user-1000.slice", SYSTEM, "user.slice"); // not a path: a cgroup path
    }

    #[test_id(te2lzn6)]
    #[test]
    fn a_service_cgroup_beneath_a_sub_cgroup_is_still_the_service() {
        is("/system.slice/dolt-beads.service/payload", "dolt-beads.service", "dolt-beads.service"); // not a path: a cgroup path
    }

    #[test_id(t7etoch)]
    #[test]
    fn a_test_binary_hash_folds_and_other_commands_pass_through() {
        assert_eq!(fold_program("round_vm-304631"), "round_vm-*");
        assert_eq!(fold_program("hel-a19db904b0c"), "hel-*");
        for plain in ["bash", "nextest-runner-", "9", "systemd-journal", "-abcdef"] {
            assert_eq!(fold_program(plain), plain);
        }
    }
}
