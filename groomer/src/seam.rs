//! The boundary to `all_partition_members` (in-process, via the `strand` crate), the repo
//! lookups (`spira_config::repos`), config (`spira_config::process`, the one door), the
//! poison bookkeeping lib.sh's `bump_poison_cleared`/`poison_asked_clear` did, and the
//! lifecycle hold verbs. No shell is sourced; tests use a recording fake.

use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// One persona's claim predicate: the labels a bead must carry and the labels that exclude it.
#[derive(Clone, Debug, Default)]
pub struct PersonaPredicate {
    pub persona: String,
    pub labels: Vec<String>,
    pub exclude: Vec<String>,
}

fn split_labels(s: &str) -> Vec<String> {
    s.split(',').map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

pub trait Seam {
    /// Every persona's claim predicate, read from the runes; personas with no partition
    /// (and personas whose predicate refuses to resolve) are omitted.
    fn persona_predicates(&self) -> Vec<PersonaPredicate>;
    /// `bump_poison_cleared <id> <cause>`.
    fn bump_poison_cleared(&self, id: &str, cause: &str) -> Result<(), String>;
    /// `poison_asked_clear <id>`.
    fn poison_asked_clear(&self, id: &str) -> Result<(), String>;
    /// A declared `SPIRA_*` config key; an undeclared one is an error, never empty.
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
    /// bead's poison lock.
    fn lc_held_poison(&self, id: &str) -> bool;
    /// `spira-lc unhold <id> <kind> groomer` — lift a hold on the bead's lifecycle row.
    fn lc_unhold(&self, id: &str, kind: &str) -> Result<(), String>;
}

/// A config read: the key's declared value, or why it has none.
pub type CfgFn = Box<dyn Fn(&str) -> Result<String, String>>;
/// One `spira-claim` verb (argv without the program name); its outcome is not read.
pub type ClaimFn = Box<dyn Fn(&[String])>;

pub struct LibSeam {
    pub lib_sh: PathBuf,
    registry: OnceCell<spira_config::repos::Registry>,
    strand_cfg: OnceCell<strand::config::Config>,
    lc: OnceCell<Result<spira_config::lc_call::LcClient, String>>,
    cfg: CfgFn,
    claim: ClaimFn,
}

impl LibSeam {
    pub fn new(lib_sh: PathBuf) -> LibSeam {
        let claim: ClaimFn = Box::new(|argv: &[String]| {
            // FORK-EDGE: needs spira-claim's write-event as a library call (spira_claim::cli::dispatch, owned elsewhere)
            let _ = spira_config::bounded::bounded("spira-claim").args(argv).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
        });
        LibSeam::with(lib_sh, Box::new(spira_config::process::cfg), claim)
    }

    /// [`new`](Self::new) with the config door and the `spira-claim` verbs supplied.
    pub fn with(lib_sh: PathBuf, cfg: CfgFn, claim: ClaimFn) -> LibSeam {
        LibSeam { lib_sh, registry: OnceCell::new(), strand_cfg: OnceCell::new(), lc: OnceCell::new(), cfg, claim }
    }

    /// `strand::config::Config`, resolved once per process exactly as `strand`'s own
    /// binary resolves it (env, then the resolved toml config, then the conf.sh default) —
    /// never from this struct's own `lib_sh` path, since the detectors no longer source it.
    /// The store's client, resolved once per process from the declared route.
    fn lc(&self) -> Result<&spira_config::lc_call::LcClient, String> {
        self.lc.get_or_init(spira_config::lc_call::LcClient::configured).as_ref().map_err(String::clone)
    }

    fn strand_cfg(&self) -> &strand::config::Config {
        self.strand_cfg.get_or_init(|| strand::config::Config::resolve(&strand::config::Live::load()).unwrap_or_else(|e| {
            eprintln!("groomer: {e}");
            std::process::exit(1)
        }))
    }

    /// The repo registry (`spira_config::repos::Registry::from_env`, sp-k6lku "wave
    /// 4.13"), built once per process, in-process — no `bash -c '. lib.sh; ...'`
    /// subprocess at all: conf.sh resolves `SPIRA_HOME_REPO`/`SPIRA_REPO`/
    /// `SPIRA_REPO_DERIVED`/`SPIRA_REPO_MAP` but exports none of them, and `from_env`
    /// resolves them the same way in-process when this (bare, unit-launched) process's
    /// environment lacks them (sp-z3eyk) — [`Seam::repo_root`] and [`Seam::land_base`]
    /// both read the result.
    fn registry(&self) -> &spira_config::repos::Registry {
        self.registry.get_or_init(|| {
            let home = self.lib_sh.parent().map(Path::to_path_buf).unwrap_or_default();
            spira_config::repos::Registry::from_env(std::env::vars().collect(), &home)
        })
    }
}

impl Seam for LibSeam {
    fn persona_predicates(&self) -> Vec<PersonaPredicate> {
        let home = self.lib_sh.parent().map(Path::to_path_buf).unwrap_or_default();
        spira_config::runes::rune_names(&home)
            .into_iter()
            .filter_map(|f| {
                let p = spira_config::runes::rune_predicate(&home, &f).ok()?;
                let labels = split_labels(&p.labels);
                (!labels.is_empty()).then(|| PersonaPredicate { persona: f, labels, exclude: split_labels(&p.exclude_labels) })
            })
            .collect()
    }

    fn bump_poison_cleared(&self, id: &str, cause: &str) -> Result<(), String> {
        // lib.sh's `_bump_write_event`: an empty id writes nothing, and the write's own
        // outcome is never the caller's failure; only an unresolvable store is.
        if id.is_empty() {
            return Ok(());
        }
        let db = (self.cfg)("SPIRA_DB")?;
        let cause = if cause.is_empty() { "unrecorded" } else { cause };
        (self.claim)(&["write-event", id, "poison.cleared", cause, "--db", &db].map(String::from));
        Ok(())
    }

    fn poison_asked_clear(&self, id: &str) -> Result<(), String> {
        // `spira-claim ask-clear`: the asked marker under $SPIRA_RUN/poison-asked, removed
        // best-effort (a missing marker is already clear).
        let run = (self.cfg)("SPIRA_RUN")?;
        if !id.is_empty() {
            let _ = std::fs::remove_file(Path::new(&run).join("poison-asked").join(id));
        }
        Ok(())
    }

    fn conf(&self, key: &str) -> Result<String, String> {
        (self.cfg)(key)
    }

    fn all_partition_members(&self) -> Result<String, String> {
        Ok(strand::detectors::all_partition_members(self.strand_cfg()))
    }

    fn bead_repo(&self, id: &str) -> Result<String, String> {
        // The bead's `repo` state dimension, through the store's content door (`bd state <id>
        // repo`). An unanswered read is "no repo": the home repo, as before.
        let name = match self.lc().and_then(|c| c.call(&["content", "state", id, "repo"]).map_err(String::from)) {
            Ok(r) if r.code == 0 => r.out.trim().to_string(),
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
        self.lc().is_ok_and(|c| c.call(&["held", id, "poison"]).is_ok_and(|r| r.code == 0))
    }

    fn lc_unhold(&self, id: &str, kind: &str) -> Result<(), String> {
        let r = self.lc()?.call(&["unhold", id, kind, "groomer"]).map_err(|e| format!("spira-lc unhold: {e}"))?;
        if r.code == 0 { Ok(()) } else { Err(format!("spira-lc unhold {id} {kind} exited {}: {}", r.code, r.why())) }
    }
}

/// `SPIRA_HOME`, else `$SPIRA_RELEASE/spira` — declared, never searched for around this
/// executable. `None` when neither is set.
pub fn locate_home(env_home: Option<&str>, env_release: Option<&str>) -> Option<PathBuf> {
    match env_home.filter(|h| !h.is_empty()) {
        Some(h) => Some(PathBuf::from(h)),
        None => spira_config::release_env::release_root(env_release).ok().map(|r| r.join("spira")),
    }
}

#[cfg(test)]
mod lib_seam_tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn seam(log: Rc<RefCell<Vec<Vec<String>>>>) -> LibSeam {
        seam_in(log, PathBuf::from("/nonexistent/run"))
    }

    fn seam_in(log: Rc<RefCell<Vec<Vec<String>>>>, run: PathBuf) -> LibSeam {
        let cfg: CfgFn = Box::new(move |k: &str| match k {
            "SPIRA_GROOM_GATE_LABEL" => Ok("gate".into()),
            "SPIRA_DB" => Ok("/fixture/db".into()),
            "SPIRA_RUN" => Ok(run.to_string_lossy().into_owned()),
            other => Err(format!("{other} is not declared")),
        });
        let claim: ClaimFn = Box::new(move |argv: &[String]| log.borrow_mut().push(argv.to_vec()));
        // A lib.sh that does not exist: nothing below may source it.
        LibSeam::with(PathBuf::from("/nonexistent/lib.sh"), cfg, claim)
    }

    #[test]
    fn a_config_key_is_read_through_the_config_door_not_a_shell() {
        let s = seam(Rc::default());
        assert_eq!(s.conf("SPIRA_GROOM_GATE_LABEL").unwrap(), "gate");
        assert!(s.conf("SPIRA_NOT_A_KEY").is_err());
    }

    #[test]
    fn the_poison_cleared_event_is_spira_claims_write_event_with_no_shell() {
        let log: Rc<RefCell<Vec<Vec<String>>>> = Rc::default();
        let s = seam(Rc::clone(&log));
        s.bump_poison_cleared("sp-1", "work-fault-triage").unwrap();
        let want: Vec<Vec<String>> = vec![["write-event", "sp-1", "poison.cleared", "work-fault-triage", "--db", "/fixture/db"].map(String::from).to_vec()];
        assert_eq!(*log.borrow(), want);
    }

    #[test]
    fn the_ask_clear_removes_the_asked_marker_in_process() {
        let dir = testkit::TempDir::new("groomer-ask-clear");
        let run = dir.path().to_path_buf();
        std::fs::create_dir_all(run.join("poison-asked")).unwrap();
        std::fs::write(run.join("poison-asked").join("sp-1"), "").unwrap();
        let log: Rc<RefCell<Vec<Vec<String>>>> = Rc::default();
        let s = seam_in(Rc::clone(&log), run.clone());
        s.poison_asked_clear("sp-1").unwrap();
        assert!(!run.join("poison-asked").join("sp-1").exists());
        assert!(log.borrow().is_empty(), "no spira-claim child for a file removal");
        s.poison_asked_clear("sp-absent").unwrap();
    }
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct FakeSeam {
        pub calls: RefCell<Vec<String>>,
        pub confs: RefCell<std::collections::BTreeMap<String, String>>,
        pub partition_members: RefCell<String>,
        pub repos: RefCell<std::collections::BTreeMap<String, String>>,
        pub roots: RefCell<std::collections::BTreeMap<String, String>>,
        pub bases: RefCell<std::collections::BTreeMap<String, String>>,
        pub held_poison: RefCell<std::collections::BTreeSet<String>>,
        pub predicates: RefCell<Vec<PersonaPredicate>>,
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
        fn persona_predicates(&self) -> Vec<PersonaPredicate> {
            self.predicates.borrow().clone()
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

        fn lc_unhold(&self, id: &str, kind: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("lc_unhold {id} {kind}"));
            if kind == "poison" {
                self.held_poison.borrow_mut().remove(id);
            }
            Ok(())
        }
    }
}
