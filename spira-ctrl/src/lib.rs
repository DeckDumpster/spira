//! spira-ctrl — the operational control plane (DESIGN.md).
//!
//! A control operation (suspending a timer, pausing a persona, draining a lane) is not a
//! source-code change. State lives at one JSON file, `$SPIRA_CTRL`, in the gitignored
//! runtime directory — never written by git, install.sh, or any other source-management
//! operation. Every entry carries a reason and an owning bead; an entry without either is
//! refused at write time.
//!
//! STORAGE FORMAT (unchanged from ctrl.sh — this crate reads and writes the same file,
//! byte-for-byte compatible): a flat JSON object keyed by subject name; each value is an
//! object keyed by operation type (today, only "suspend"); each leaf carries reason,
//! owner, when, by. Modelled as nested `BTreeMap<String,String>` rather than a struct so
//! keys serialize in sorted order without depending on serde_json's `preserve_order`
//! feature — matching Python's `json.dumps(..., sort_keys=True)` byte for byte.

use std::collections::BTreeMap;
use std::path::Path;

/// One subject's operations: op name ("suspend") -> its fields (reason/owner/when/by).
pub type Ops = BTreeMap<String, BTreeMap<String, String>>;
/// The whole control file: subject -> its ops.
pub type CtrlData = BTreeMap<String, Ops>;

/// Read the control file. A missing file is empty data, not an error — ctrl.sh's own
/// `_read_ctrl` treats "no file" as `{}`.
pub fn read(path: &Path) -> Result<CtrlData, String> {
    if !path.is_file() {
        return Ok(CtrlData::new());
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(CtrlData::new());
    }
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Write the control file atomically: a temp file in the same directory, then a rename —
/// the same two-step ctrl.sh's `_write_ctrl` performs (mktemp + mv), so a reader never sees
/// a half-written file.
pub fn write_atomic(path: &Path, data: &CtrlData) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let json = serde_json::to_string_pretty(data).map_err(std::io::Error::other)?;
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(
        ".{}.tmp{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("ctrl"),
        std::process::id()
    ));
    std::fs::write(&tmp, format!("{json}\n"))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// suspend <subject> <reason> <owner> <when> <by> -> data with that subject's "suspend" op
/// set (replacing any prior one), every other key of the subject and every other subject
/// untouched — exactly `do_suspend`'s python: `data[subject]['suspend'] = {...}`.
pub fn suspend(data: &mut CtrlData, subject: &str, reason: &str, owner: &str, when: &str, by: &str) {
    let ops = data.entry(subject.to_string()).or_default();
    let mut fields = BTreeMap::new();
    fields.insert("reason".to_string(), reason.to_string());
    fields.insert("owner".to_string(), owner.to_string());
    fields.insert("when".to_string(), when.to_string());
    fields.insert("by".to_string(), by.to_string());
    ops.insert("suspend".to_string(), fields);
}

/// True for a plain `YYYY-MM-DD` date.
pub fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })
}

/// Declares when a suspension is due for review: a `YYYY-MM-DD` date. The suspension must
/// exist; returns false otherwise.
pub fn set_until(data: &mut CtrlData, subject: &str, until: &str) -> bool {
    match data.get_mut(subject).and_then(|o| o.get_mut("suspend")) {
        Some(f) => {
            f.insert("until".to_string(), until.to_string());
            true
        }
        None => false,
    }
}

/// resume <subject> -> true if a suspension was removed. Drops the subject entirely when
/// it carries no other ops, matching `do_resume`'s python (`del data[subject]` when empty).
pub fn resume(data: &mut CtrlData, subject: &str) -> bool {
    let mut was = false;
    if let Some(ops) = data.get_mut(subject) {
        if ops.remove("suspend").is_some() {
            was = true;
        }
        if ops.is_empty() {
            data.remove(subject);
        }
    }
    was
}

/// reason <subject> -> the suspension reason, if suspended.
pub fn reason<'a>(data: &'a CtrlData, subject: &str) -> Option<&'a str> {
    data.get(subject)?.get("suspend")?.get("reason").map(String::as_str)
}

/// The suspension's owner (a bead id) and its optional `until` date.
pub fn declared<'a>(data: &'a CtrlData, subject: &str) -> Option<(&'a str, Option<&'a str>)> {
    let f = data.get(subject)?.get("suspend")?;
    Some((f.get("owner").map(String::as_str).unwrap_or(""), f.get("until").map(String::as_str)))
}

/// is_suspended <subject> -> true if a "suspend" op is recorded.
pub fn is_suspended(data: &CtrlData, subject: &str) -> bool {
    data.get(subject).map(|o| o.contains_key("suspend")).unwrap_or(false)
}

/// load_suspended -> subject -> reason, for every currently suspended subject, in one pass.
/// This is the one call world.sh's `start` and `status` need instead of a `check`/`reason`
/// per timer (ctrl.sh's own `ctrl_load_suspended`); as a Rust library function it costs no
/// process spawn at all, where the bash version still cost one python3 read.
pub fn load_suspended(data: &CtrlData) -> BTreeMap<String, String> {
    data.iter()
        .filter_map(|(s, ops)| {
            ops.get("suspend")
                .map(|f| (s.clone(), f.get("reason").cloned().unwrap_or_default()))
        })
        .collect()
}

/// Render `list`'s human text exactly as `do_list`'s python does: subjects and ops both in
/// sorted order (free, because both maps are `BTreeMap`), one header line and one reason
/// line per entry.
pub fn render_list(data: &CtrlData) -> String {
    if data.is_empty() {
        return "ctrl: no entries\n".to_string();
    }
    let mut out = String::new();
    for (subject, ops) in data {
        for (op, entry) in ops {
            out.push_str(&format!(
                "  {subject}  [{op}]  owner:{}  since:{}  by:{}\n",
                entry.get("owner").map(String::as_str).unwrap_or("?"),
                entry.get("when").map(String::as_str).unwrap_or("?"),
                entry.get("by").map(String::as_str).unwrap_or("?"),
            ));
            out.push_str(&format!(
                "    reason: {}\n",
                entry.get("reason").map(String::as_str).unwrap_or("?")
            ));
        }
    }
    out
}

/// The unit name forms a subject resolves to for a divergence probe — instance-qualified
/// first, then plain — matching `do_divergence`'s direction-1 loop (`"${subject}-${inst}.timer"`
/// etc). Order matters only for which form the caller reports; both are always checked.
pub fn unit_forms(subject: &str, inst: &str) -> [String; 4] {
    [
        format!("{subject}-{inst}.timer"),
        format!("{subject}-{inst}.service"),
        format!("{subject}.timer"),
        format!("{subject}.service"),
    ]
}

/// The standing-condition names watchtower consults [`is_suspended`] for before it files:
/// each is the `cause` its findings carry (the probe name for the conditions framework).
pub const WATCHTOWER_CONDITIONS: &[&str] = &[
    "batched-stranded",
    "batched-too-long",
    "closed-stranded",
    "czar-not-cleared",
    "czar-unclaimed",
    "dedup-meter",
    "deploy-fault",
    "disabled-timer",
    "dolt-client-drop",
    "failed-unit",
    "failing-units",
    "gate-silent",
    "gate-slow",
    "hotfix-standing",
    "idle-while-ready",
    "oldest-unsent",
    "pr-stall-auto-merge-off",
    "pr-stall-checks-red",
    "pressure",
    "queue-lock-holders",
    "release-currency",
    "release-skew",
    "release-store",
    "rowless-beads",
    "sccache-wedge",
    "slow-query",
    "throttle-engaged",
    "throttle-lifted",
    "throttle-stall",
    "unadopted-refs",
];

/// A subject is consultable when something reads it: a watchtower condition, or a subject
/// one of `unit_files` (systemd unit-file names) resolves to.
pub fn is_consultable(subject: &str, unit_files: &[String], inst: &str) -> bool {
    WATCHTOWER_CONDITIONS.contains(&subject)
        || unit_files.iter().any(|u| subject_of_masked_unit(u, inst) == subject)
}

/// True when the control plane declines `unit` (a `.service`/`.timer` file name): its
/// subject is the unit name minus extension and instance suffix.
pub fn unit_suspended(data: &CtrlData, unit: &str, inst: &str) -> bool {
    is_suspended(data, &subject_of_masked_unit(unit, inst))
}

/// A masked unit file's subject: strip the extension, then the instance suffix if the base
/// carries it — matching `do_divergence`'s direction-2 derivation
/// (`base="${unit_name%.*}"`, `subject="${base%-${inst}}"`).
pub fn subject_of_masked_unit(unit_name: &str, inst: &str) -> String {
    let base = unit_name.rsplit_once('.').map(|(b, _)| b).unwrap_or(unit_name);
    base.strip_suffix(&format!("-{inst}")).unwrap_or(base).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspend_then_resume_round_trips() {
        let mut d = CtrlData::new();
        suspend(&mut d, "spira-groom", "why", "sp-1", "2026-09-30", "operator");
        assert!(is_suspended(&d, "spira-groom"));
        assert_eq!(reason(&d, "spira-groom"), Some("why"));
        assert!(resume(&mut d, "spira-groom"));
        assert!(!is_suspended(&d, "spira-groom"));
        assert!(d.is_empty(), "the subject is dropped once it carries no ops");
    }

    #[test]
    fn unit_suspended_resolves_instance_and_extension() {
        let mut d = CtrlData::new();
        suspend(&mut d, "spira-groom", "r", "o", "w", "b");
        assert!(unit_suspended(&d, "spira-groom-prod.timer", "prod"));
        assert!(unit_suspended(&d, "spira-groom-prod.service", "prod"));
        assert!(!unit_suspended(&d, "spira-other-prod.service", "prod"));
    }

    #[test]
    fn consultable_is_a_condition_or_a_unit_subject() {
        let units = vec!["spira-groom-prod.timer".to_string()];
        assert!(is_consultable("slow-query", &units, "prod"));
        assert!(is_consultable("spira-groom", &units, "prod"));
        assert!(!is_consultable("nonsense", &units, "prod"));
    }

    #[test]
    fn until_is_declared_on_an_existing_suspension_only() {
        let mut d = CtrlData::new();
        assert!(!set_until(&mut d, "a", "2026-10-09"));
        suspend(&mut d, "a", "r", "sp-1", "w", "b");
        assert!(set_until(&mut d, "a", "2026-10-09"));
        assert_eq!(declared(&d, "a"), Some(("sp-1", Some("2026-10-09"))));
        assert!(is_date("2026-10-09") && !is_date("tomorrow") && !is_date("2026-1-9"));
    }

    #[test]
    fn resume_of_unsuspended_subject_is_false_and_noop() {
        let mut d = CtrlData::new();
        assert!(!resume(&mut d, "nope"));
        assert!(d.is_empty());
    }

    #[test]
    fn suspend_leaves_other_ops_and_other_subjects_untouched() {
        let mut d = CtrlData::new();
        let mut ops = Ops::new();
        let mut other = BTreeMap::new();
        other.insert("k".to_string(), "v".to_string());
        ops.insert("other-op".to_string(), other);
        d.insert("s1".to_string(), ops);
        suspend(&mut d, "s1", "r", "o", "w", "b");
        suspend(&mut d, "s2", "r2", "o2", "w2", "b2");
        assert!(d["s1"].contains_key("other-op"));
        assert!(d["s1"].contains_key("suspend"));
        assert_eq!(d["s2"]["suspend"]["reason"], "r2");
    }

    #[test]
    fn json_round_trips_and_sorts_keys_like_python_sort_keys() {
        let mut d = CtrlData::new();
        suspend(&mut d, "spira-groom", "why", "sp-1", "2026-09-30", "operator");
        let text = serde_json::to_string_pretty(&d).unwrap();
        // by, owner, reason, when — alphabetical, unconditionally, because the leaf is a
        // BTreeMap<String,String> rather than a struct.
        let by = text.find("\"by\"").unwrap();
        let owner = text.find("\"owner\"").unwrap();
        let reason_i = text.find("\"reason\"").unwrap();
        let when = text.find("\"when\"").unwrap();
        assert!(by < owner && owner < reason_i && reason_i < when, "{text}");
        let back: CtrlData = serde_json::from_str(&text).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn read_of_missing_file_is_empty_not_an_error() {
        let d = testkit::TempDir::new("ctrl-lib");
        let p = d.join("ctrl.json");
        assert_eq!(read(&p).unwrap(), CtrlData::new());
    }

    #[test]
    fn write_then_read_round_trips_through_disk() {
        let d = testkit::TempDir::new("ctrl-lib");
        let p = d.join("sub/ctrl.json");
        let mut data = CtrlData::new();
        suspend(&mut data, "spira-groom", "why", "sp-1", "2026-09-30", "operator");
        write_atomic(&p, &data).unwrap();
        let back = read(&p).unwrap();
        assert_eq!(back, data);
    }

    #[test]
    fn load_suspended_collects_every_subject_in_one_pass() {
        let mut d = CtrlData::new();
        suspend(&mut d, "a", "ra", "oa", "wa", "ba");
        suspend(&mut d, "b", "rb", "ob", "wb", "bb");
        let m = load_suspended(&d);
        assert_eq!(m.get("a").map(String::as_str), Some("ra"));
        assert_eq!(m.get("b").map(String::as_str), Some("rb"));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn render_list_empty_matches_bash() {
        assert_eq!(render_list(&CtrlData::new()), "ctrl: no entries\n");
    }

    #[test]
    fn render_list_formats_like_do_list() {
        let mut d = CtrlData::new();
        suspend(&mut d, "spira-groom", "graph hygiene", "sp-1", "2026-09-30", "operator");
        let out = render_list(&d);
        assert_eq!(
            out,
            "  spira-groom  [suspend]  owner:sp-1  since:2026-09-30  by:operator\n    reason: graph hygiene\n"
        );
    }

    #[test]
    fn unit_forms_are_instance_qualified_first() {
        assert_eq!(
            unit_forms("spira-groom", "prod"),
            [
                "spira-groom-prod.timer".to_string(),
                "spira-groom-prod.service".to_string(),
                "spira-groom.timer".to_string(),
                "spira-groom.service".to_string(),
            ]
        );
    }

    #[test]
    fn subject_of_masked_unit_strips_extension_then_instance() {
        assert_eq!(subject_of_masked_unit("spira-groom-prod.timer", "prod"), "spira-groom");
        assert_eq!(subject_of_masked_unit("cockpit-ensure.service", "prod"), "cockpit-ensure");
        assert_eq!(subject_of_masked_unit("spira-groom.timer", "prod"), "spira-groom");
    }
}
