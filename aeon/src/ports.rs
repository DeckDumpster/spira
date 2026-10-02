//! The four things this program talks to, as traits, so the run can be driven by fakes:
//! the bead store (`Bd`), lib.sh (`Seam`), git (`Git`) and every other program (`Exec`).
//! The model session has its own port (`session::Launcher`).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::RwLock;
use std::time::Duration;

use crate::util::{self, Out, Spec};

/// `bdq` semantics: `bd -C $SPIRA_DB <args>`.
pub trait Bd: Send + Sync {
    fn bd(&self, args: &[String]) -> Out;
}

/// One lib.sh function through the fixed seam script (seam.rs).
pub trait Seam: Send + Sync {
    fn call(&self, func: &str, args: &[String]) -> Out;
}

/// `git -C <dir> <args>`.
pub trait Git: Send + Sync {
    fn git(&self, dir: &Path, args: &[&str]) -> Out;
}

/// Any other program (spira-claim, world.sh, gate-run.sh, holds.sh, sop, python3, …).
pub trait Exec: Send + Sync {
    fn exec(&self, prog: &str, args: &[String], stdin: Option<Vec<u8>>, cwd: Option<&Path>) -> Out;
}

pub fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

/// The two environments a child can be given, plus what this aeon exported on top.
///
/// A lib.sh function ran INSIDE aeon.sh, so the seam gets the unit's ORIGINAL environment
/// (conf.sh re-derives from it exactly as it did at startup) plus the aeon's exports. A
/// script or the model was a CHILD of aeon.sh, so it gets conf.sh's exported environment
/// (the snapshot) plus the same exports.
pub struct Env {
    pub original: BTreeMap<String, String>,
    pub child_base: BTreeMap<String, String>,
    exports: RwLock<BTreeMap<String, Option<String>>>,
}

impl Env {
    pub fn new(original: BTreeMap<String, String>, child_base: BTreeMap<String, String>) -> Env {
        Env { original, child_base, exports: RwLock::new(BTreeMap::new()) }
    }
    pub fn set(&self, k: &str, v: &str) {
        self.exports.write().unwrap().insert(k.into(), Some(v.into()));
    }
    pub fn unset(&self, k: &str) {
        self.exports.write().unwrap().insert(k.into(), None);
    }
    fn apply(&self, mut base: BTreeMap<String, String>) -> BTreeMap<String, String> {
        for (k, v) in self.exports.read().unwrap().iter() {
            match v {
                Some(v) => {
                    base.insert(k.clone(), v.clone());
                }
                None => {
                    base.remove(k);
                }
            }
        }
        base
    }
    pub fn child(&self) -> BTreeMap<String, String> {
        self.apply(self.child_base.clone())
    }
    pub fn seam(&self) -> BTreeMap<String, String> {
        let mut m = self.apply(self.original.clone());
        // law-a-binary-resolves-the-config-it-reads (sp-kgzql): the lib.sh seam re-derives
        // conf.sh from the unit's ORIGINAL environment (this struct's own doc above) — bare
        // whenever this aeon itself was launched with no release on PATH. Prepend this
        // binary's own release's bin/+spira/ rather than trusting `original`'s PATH alone.
        let release = spira_config::release_env::own_release_root_for_process();
        for (k, v) in spira_config::release_env::child_path_env(release.as_deref(), m.get("PATH").map(String::as_str)) {
            m.insert(k, v);
        }
        m
    }
    pub fn get_export(&self, k: &str) -> Option<String> {
        self.exports.read().unwrap().get(k).cloned().flatten()
    }
}

// ---- real implementations ------------------------------------------------------------

pub struct RealGit<'a> {
    pub env: &'a Env,
}

impl Git for RealGit<'_> {
    fn git(&self, dir: &Path, args: &[&str]) -> Out {
        let mut a = vec!["-C".to_string(), dir.display().to_string()];
        a.extend(args.iter().map(|x| x.to_string()));
        let env = self.env.child();
        util::run(Spec { prog: "git", args: a, env: Some(&env), cwd: None, stdin: None, timeout: None })
    }
}

pub struct RealExec<'a> {
    pub env: &'a Env,
    pub timeout: Option<Duration>,
}

impl Exec for RealExec<'_> {
    fn exec(&self, prog: &str, args: &[String], stdin: Option<Vec<u8>>, cwd: Option<&Path>) -> Out {
        let env = self.env.child();
        util::run(Spec { prog, args: args.to_vec(), env: Some(&env), cwd, stdin, timeout: self.timeout })
    }
}
