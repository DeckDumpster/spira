//! The boundary to lib.sh's STATE and LIVELOCK detectors, and to the lifecycle hold
//! verb. These stay in lib.sh (group 4, last — "leave lib.sh alone") and are shared with
//! callers this crate does not own (`cockpit.sh livelock`, `attempts.sh`, `auron.sh`,
//! `incident.sh`); `groomer` reaches them the same way `sentinel` reaches its own lib.sh
//! seams — `bash -c '. "$LIB"; <func> "$@"'` — rather than re-deriving the detection
//! logic. Production shells out for real; tests use a recording fake.

use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub trait Seam {
    /// `detect_livelocked` — one `LIVELOCK <id> <category> — <reason>` line per row.
    fn detect_livelocked(&self) -> Result<String, String>;
    /// `detect_landed_but_open` — one `STATE <id> landed-but-open — <evidence>` line.
    fn detect_landed_but_open(&self) -> Result<String, String>;
    /// `detect_closed_unlanded_states` — `closed-no-branch` / `closed-never-landed
    /// conflict|batch-ready` STATE lines.
    fn detect_closed_unlanded_states(&self) -> Result<String, String>;
    /// `detect_false_blockers <space-separated ids>`.
    fn detect_false_blockers(&self, reopened_ids: &str) -> Result<String, String>;
    /// `detect_incident_needs_builder`.
    fn detect_incident_needs_builder(&self) -> Result<String, String>;
    /// `bead_reopen <id> <cause> <note>`.
    fn bead_reopen(&self, id: &str, cause: &str, note: &str) -> Result<(), String>;
    /// `bump_poison_cleared <id> <cause>`.
    fn bump_poison_cleared(&self, id: &str, cause: &str) -> Result<(), String>;
    /// `poison_asked_clear <id>`.
    fn poison_asked_clear(&self, id: &str) -> Result<(), String>;
    /// A resolved `SPIRA_*` config key, read after sourcing conf.sh/lib.sh (e.g.
    /// `SPIRA_CI_LABEL`, `SPIRA_INCIDENT_LABEL`, `SPIRA_PLAN_LABEL`). Empty if unset.
    fn conf(&self, key: &str) -> Result<String, String>;
    /// `all_partition_members` — every open/in_progress bead id across every partition
    /// this roster covers, deduplicated, one per line.
    fn all_partition_members(&self) -> Result<String, String>;
    /// `bead_repo <id>` — its `repo:` label, or the home repo if it names none.
    fn bead_repo(&self, id: &str) -> Result<String, String>;
    /// `repo_root <name>` — the repo's checkout path, empty if `name` is not in the map.
    fn repo_root(&self, name: &str) -> Result<String, String>;
    /// `spira_landrefs <path>`'s first ref — the base a branch in that repo lands on.
    fn land_base(&self, repo_path: &str) -> Result<String, String>;
    /// `spira-lc held <id> poison` — whether the lifecycle machine already holds this
    /// bead's poison lock (lifecycle_enforce path only).
    fn lc_held_poison(&self, id: &str) -> bool;
}

pub struct LibSeam {
    pub lib_sh: PathBuf,
    registry: OnceCell<spira_config::repos::Registry>,
}

impl LibSeam {
    pub fn new(lib_sh: PathBuf) -> LibSeam {
        LibSeam { lib_sh, registry: OnceCell::new() }
    }

    /// The repo registry (`spira_config::repos`, sp-k6lku "wave 4.13"), built once per
    /// process from a single snapshot of the vars [`spira_config::repos::Registry::new`]
    /// needs, instead of a fresh `bash -c '. lib.sh; repo_root ...'`/`spira_landrefs`
    /// subprocess per call — [`Seam::repo_root`] and [`Seam::land_base`] both read it.
    fn registry(&self) -> &spira_config::repos::Registry {
        self.registry.get_or_init(|| {
            let script = ". \"$0\" >/dev/null 2>&1 || exit 97\n\
                for __v in SPIRA_HOME_REPO SPIRA_REPO SPIRA_REPO_DERIVED SPIRA_REPO_MAP; do \
                printf '%s=%s\\0' \"$__v\" \"${!__v-}\"; done";
            let out = Command::new("bash")
                .arg("-c")
                .arg(script)
                .arg(&self.lib_sh)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output();
            let mut env: BTreeMap<String, String> = BTreeMap::new();
            if let Ok(o) = out {
                for rec in String::from_utf8_lossy(&o.stdout).split('\0') {
                    if let Some((k, v)) = rec.split_once('=') {
                        env.insert(k.to_string(), v.to_string());
                    }
                }
            }
            let map_text = env
                .get("SPIRA_REPO_MAP")
                .filter(|p| !p.is_empty())
                .and_then(|p| std::fs::read_to_string(p).ok());
            let home = self.lib_sh.parent().map(Path::to_path_buf).unwrap_or_default();
            spira_config::repos::Registry::new(map_text.as_deref(), &env, &home)
        })
    }

    fn run(&self, args: &[&str]) -> Result<String, String> {
        let script = r#". "$0" >/dev/null 2>&1 || exit 97; "$@""#;
        let o = Command::new("bash")
            .arg("-c")
            .arg(script)
            .arg(&self.lib_sh)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| format!("lib.sh {}: {e}", args.join(" ")))?;
        if !o.status.success() {
            return Err(format!("lib.sh {} exited {}", args.join(" "), o.status.code().unwrap_or(-1)));
        }
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    }

}

impl Seam for LibSeam {
    fn detect_livelocked(&self) -> Result<String, String> {
        self.run(&["detect_livelocked"])
    }

    fn detect_landed_but_open(&self) -> Result<String, String> {
        self.run(&["detect_landed_but_open"])
    }

    fn detect_closed_unlanded_states(&self) -> Result<String, String> {
        self.run(&["detect_closed_unlanded_states"])
    }

    fn detect_false_blockers(&self, reopened_ids: &str) -> Result<String, String> {
        self.run(&["detect_false_blockers", reopened_ids])
    }

    fn detect_incident_needs_builder(&self) -> Result<String, String> {
        self.run(&["detect_incident_needs_builder"])
    }

    fn bead_reopen(&self, id: &str, cause: &str, note: &str) -> Result<(), String> {
        self.run(&["bead_reopen", id, cause, note]).map(|_| ())
    }

    fn bump_poison_cleared(&self, id: &str, cause: &str) -> Result<(), String> {
        self.run(&["bump_poison_cleared", id, cause]).map(|_| ())
    }

    fn poison_asked_clear(&self, id: &str) -> Result<(), String> {
        self.run(&["poison_asked_clear", id]).map(|_| ())
    }

    fn conf(&self, key: &str) -> Result<String, String> {
        // run_stdin unused here; a plain var read never needs a payload.
        let script = format!(r#". "$0" >/dev/null 2>&1 || exit 97; printf '%s' "${{{key}:-}}""#);
        let o = Command::new("bash")
            .arg("-c")
            .arg(script)
            .arg(&self.lib_sh)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("lib.sh conf {key}: {e}"))?;
        if !o.status.success() {
            return Err(format!("lib.sh conf {key} exited {}", o.status.code().unwrap_or(-1)));
        }
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    }

    fn all_partition_members(&self) -> Result<String, String> {
        self.run(&["all_partition_members"])
    }

    fn bead_repo(&self, id: &str) -> Result<String, String> {
        // bdq (family A) is a compiled binary now (sp-w3h16, "wave 4.14", landed
        // concurrently with this bead) — `bead_repo`'s only non-registry step no longer
        // needs lib.sh sourced at all. `bdq state <id> repo` inherits this process's own
        // SPIRA_DB/SPIRA_BD exactly as the bash shim's `command bdq "$@"` did; `state` is
        // not a create, so it never touches the repo-label fence's registry build either.
        let out = Command::new("bdq").arg("state").arg(id).arg("repo").stdin(Stdio::null()).stderr(Stdio::null()).output();
        let name = match out {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
            _ => String::new(),
        };
        let name = if name.starts_with('(') { String::new() } else { name };
        Ok(if name.is_empty() { self.registry().home_repo().to_string() } else { name })
    }

    fn repo_root(&self, name: &str) -> Result<String, String> {
        // repo_root exits non-zero for an unmapped name; that is "no path", not an error
        // this seam should propagate — the caller reads the empty string the same way.
        // spira_config::repos (sp-k6lku, "wave 4.13") in-process, not a bash seam call.
        Ok(self.registry().root(name).unwrap_or_default())
    }

    fn land_base(&self, repo_path: &str) -> Result<String, String> {
        // spira_landrefs (sp-k6lku, "wave 4.13") in-process, not a bash seam call.
        Ok(spira_config::repos::landrefs(self.registry(), repo_path).map(|(base, _)| base).unwrap_or_default())
    }

    fn lc_held_poison(&self, id: &str) -> bool {
        Command::new("spira-lc").args(["held", id, "poison"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
    }
}

/// `SPIRA_HOME` from the environment, else the first directory holding `lib.sh` among the
/// release and cargo layouts around this executable — the same search `sentinel::locate_home`
/// uses, duplicated rather than shared because it is fifteen lines and every Rust seam onto
/// lib.sh has needed its own copy so far.
pub fn locate_home(env_home: Option<&str>, exe: &Path) -> Option<PathBuf> {
    if let Some(h) = env_home.filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(h));
    }
    let dir = exe.parent()?;
    [dir.join("../spira"), dir.join("../../spira"), dir.join("../../../spira")]
        .into_iter()
        .find(|c| c.join("lib.sh").is_file())
        .map(|c| c.canonicalize().unwrap_or(c))
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct FakeSeam {
        pub livelocked: RefCell<String>,
        pub landed_but_open: RefCell<String>,
        pub closed_unlanded: RefCell<String>,
        pub false_blockers: RefCell<String>,
        pub incident_needs_builder: RefCell<String>,
        pub calls: RefCell<Vec<String>>,
        pub confs: RefCell<std::collections::BTreeMap<String, String>>,
        pub partition_members: RefCell<String>,
        pub repos: RefCell<std::collections::BTreeMap<String, String>>,
        pub roots: RefCell<std::collections::BTreeMap<String, String>>,
        pub bases: RefCell<std::collections::BTreeMap<String, String>>,
        pub held_poison: RefCell<std::collections::BTreeSet<String>>,
    }

    impl FakeSeam {
        pub fn new() -> FakeSeam {
            FakeSeam::default()
        }

        pub fn log(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl Seam for FakeSeam {
        fn detect_livelocked(&self) -> Result<String, String> {
            self.calls.borrow_mut().push("detect_livelocked".into());
            Ok(self.livelocked.borrow().clone())
        }

        fn detect_landed_but_open(&self) -> Result<String, String> {
            self.calls.borrow_mut().push("detect_landed_but_open".into());
            Ok(self.landed_but_open.borrow().clone())
        }

        fn detect_closed_unlanded_states(&self) -> Result<String, String> {
            self.calls.borrow_mut().push("detect_closed_unlanded_states".into());
            Ok(self.closed_unlanded.borrow().clone())
        }

        fn detect_false_blockers(&self, reopened_ids: &str) -> Result<String, String> {
            self.calls.borrow_mut().push(format!("detect_false_blockers {reopened_ids}"));
            Ok(self.false_blockers.borrow().clone())
        }

        fn detect_incident_needs_builder(&self) -> Result<String, String> {
            self.calls.borrow_mut().push("detect_incident_needs_builder".into());
            Ok(self.incident_needs_builder.borrow().clone())
        }

        fn bead_reopen(&self, id: &str, cause: &str, note: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("bead_reopen {id} {cause} {note}"));
            Ok(())
        }

        fn bump_poison_cleared(&self, id: &str, cause: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("bump_poison_cleared {id} {cause}"));
            Ok(())
        }

        fn poison_asked_clear(&self, id: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("poison_asked_clear {id}"));
            Ok(())
        }

        fn conf(&self, key: &str) -> Result<String, String> {
            Ok(self.confs.borrow().get(key).cloned().unwrap_or_default())
        }

        fn all_partition_members(&self) -> Result<String, String> {
            self.calls.borrow_mut().push("all_partition_members".into());
            Ok(self.partition_members.borrow().clone())
        }

        fn bead_repo(&self, id: &str) -> Result<String, String> {
            Ok(self.repos.borrow().get(id).cloned().unwrap_or_default())
        }

        fn repo_root(&self, name: &str) -> Result<String, String> {
            Ok(self.roots.borrow().get(name).cloned().unwrap_or_default())
        }

        fn land_base(&self, repo_path: &str) -> Result<String, String> {
            Ok(self.bases.borrow().get(repo_path).cloned().unwrap_or_default())
        }

        fn lc_held_poison(&self, id: &str) -> bool {
            self.held_poison.borrow().contains(id)
        }
    }
}
