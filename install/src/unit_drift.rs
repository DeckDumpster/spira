//! Installed unit files and drop-ins this install's manifest does not ship — `drift.sh units`,
//! answered in-process so the sentinel pass and the cockpit probe fork nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub enum UnitDrift {
    Clean,
    Drift(String),
    Unreadable,
}

const KNOWN_DROPINS: [&str; 2] = ["50-spira-intake.conf", "cadence.conf"];

/// Every installed name this box ships or could ship: what `owned.sh list` prints as `unit`.
pub fn shipped_names(instance: &str) -> Result<BTreeSet<String>, String> {
    let m = crate::bootstrap::manifest_from_env(instance)?;
    let mut names: BTreeSet<String> = m.template_names().into_iter().map(|n| crate::manifest::inst_name(n, instance)).collect();
    names.extend(m.watch_names.iter().map(|w| crate::manifest::inst_watch_name(w, instance)));
    names.extend(m.unbuilt.iter().map(|n| crate::manifest::inst_name(n, instance)));
    names.extend(crate::manifest::union_installed_names(instance));
    Ok(names)
}

/// `<path>` → bead of each complete `[[unit]]` table; `-` is permanent, `?` names no bead.
pub fn local_units(manifest: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let (mut path, mut bead, mut perm) = (String::new(), String::new(), false);
    let mut flush = |path: &mut String, bead: &mut String, perm: &mut bool| {
        if *perm {
            *bead = "-".into();
        }
        if !path.is_empty() {
            out.insert(std::mem::take(path), if bead.is_empty() { "?".into() } else { std::mem::take(bead) });
        }
        path.clear();
        bead.clear();
        *perm = false;
    };
    for line in manifest.lines() {
        let t = line.trim_start();
        if t.starts_with("[[unit]]") {
            flush(&mut path, &mut bead, &mut perm);
        } else if let Some(v) = quoted_value(t, "path") {
            path = v;
        } else if let Some(v) = quoted_value(t, "bead") {
            bead = v;
        } else if t.strip_prefix("permanent").is_some_and(|r| r.trim_start().strip_prefix('=').is_some_and(|r| r.trim_start().starts_with("true"))) {
            perm = true;
        }
    }
    flush(&mut path, &mut bead, &mut perm);
    out
}

fn quoted_value(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?.trim_start().strip_prefix('=')?.trim_start().strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_string())
}

fn files_in(dir: &Path, want: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_file() && want(p)).collect();
    v.sort();
    v
}

/// `is_done` answers whether a retiring bead has landed; it is asked only for declared units.
pub fn units_drift_with(unitdir: &Path, shipped: Result<BTreeSet<String>, String>, local_manifest: &str, local_path: &str, is_done: &dyn Fn(&str) -> bool) -> UnitDrift {
    let Ok(shipped) = shipped else { return UnitDrift::Unreadable };
    if !unitdir.is_dir() || shipped.is_empty() {
        return UnitDrift::Unreadable;
    }
    let declared = local_units(local_manifest);
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut found = Vec::new();
    let mut verdict = |f: &Path, why: &str| {
        let rel = f.strip_prefix(unitdir).map(|r| r.to_string_lossy().into_owned()).unwrap_or_default();
        match declared.get(&rel).map(String::as_str) {
            Some("-") => {}
            Some("?") => found.push(format!("UNSHIPPED {} (declared but names no bead and is not permanent = true)", f.display())),
            Some(b) if is_done(b) => found.push(format!("RETIRE-NOW {} (declared local override; retiring bead {b} has landed)", f.display())),
            Some(_) => {}
            None => found.push(format!("UNSHIPPED {} ({why})", f.display())),
        }
    };
    let unit_ext = |p: &Path| matches!(p.extension().and_then(|e| e.to_str()), Some("service" | "timer" | "socket"));
    for f in files_in(unitdir, unit_ext) {
        if !shipped.contains(&name(&f)) {
            verdict(&f, "unit file this install does not ship");
        }
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(unitdir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir() && p.extension().is_some_and(|e| e == "d")).collect();
    dirs.sort();
    for d in dirs {
        for f in files_in(&d, |p| p.extension().is_some_and(|e| e == "conf")) {
            if !KNOWN_DROPINS.contains(&name(&f).as_str()) {
                verdict(&f, "drop-in no repo mechanism installs");
            }
        }
    }
    if found.is_empty() {
        UnitDrift::Clean
    } else {
        let hint = if local_path.is_empty() { "the local-units manifest" } else { local_path };
        found.push(format!("drift: to accept a file, declare it as a [[unit]] (path, reason, and bead or permanent = true) in {hint}"));
        UnitDrift::Drift(found.join("\n") + "\n")
    }
}

/// The production answer: this box's manifest, `SPIRA_LOCAL_UNITS`, and the lifecycle store.
pub fn units_drift(unitdir: &Path) -> UnitDrift {
    let instance = spira_config::process::cfg("SPIRA_INSTANCE").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "prod".into());
    let local_path = spira_config::process::cfg("SPIRA_LOCAL_UNITS").unwrap_or_default();
    let local = std::fs::read_to_string(&local_path).unwrap_or_default();
    let is_done = |bead: &str| {
        let Ok(c) = spira_store::lc_call::LcClient::configured() else { return false };
        spira_store::lc_state::list_ids_on(&c, &[bead.to_string()]).ok().is_some_and(|rows| rows.iter().any(|r| r.bead_id == bead && r.state == "LANDED"))
    };
    units_drift_with(unitdir, shipped_names(&instance), &local, &local_path, &is_done)
}
