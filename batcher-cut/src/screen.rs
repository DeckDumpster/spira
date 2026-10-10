//! The batcher's use of the `sift` screen: what the pool is narrowed to before a round is cut.

use std::fs::OpenOptions;
use std::io::Write;

use batcher::core::Member;
use sift::{Acts, Candidate, FileStore, GitProbe};
use spira_config::process::cfg;

use crate::io::{self, Env, OpenBatch, Repo};

struct Live<'a> {
    env: &'a Env,
}

impl Acts for Live<'_> {
    fn note(&mut self, id: &str, text: &str) -> Result<(), String> {
        io::bead_note(self.env, id, text)
    }
    fn gate_red(&mut self, id: &str, tip: &str, reason: &str) -> Result<(), String> {
        io::lc_gate_red(self.env, id, tip, reason)
    }
    fn supersede(&mut self, id: &str, keeper: &str) -> Result<(), String> {
        io::lc_supersede(self.env, id, keeper)
    }
    fn tell(&mut self, msg: &str) {
        println!("batcher: {msg}");
        let line = format!("{} [watch:sift] {msg}\n", io::utc_stamp(spira_config::vtime::now_epoch()));
        let wrote = cfg("SPIRA_CONCIERGE_INBOX")
            .and_then(|p| OpenOptions::new().create(true).append(true).open(p).and_then(|mut f| f.write_all(line.as_bytes())).map_err(|e| e.to_string()));
        if let Err(e) = wrote {
            eprintln!("batcher: sift: concierge not told ({e}): {msg}");
        }
    }
}

/// The pool narrowed to what passes the screen. When the screen cannot run, the pool comes
/// back whole and the error is said.
pub fn screened(env: &Env, repo: &Repo, pool: Vec<Member>, open: Option<&OpenBatch>) -> Vec<Member> {
    if pool.is_empty() {
        return pool;
    }
    if let Err(e) = io::fetch_base(repo) {
        eprintln!("batcher {}: sift: base not fetched ({e}); cutting unfiltered", repo.name);
        return pool;
    }
    let work = env.run.join("sift");
    let probe_env = env.clone();
    let probe = GitProbe {
        repo: repo.path.clone(),
        base_ref: repo.base.clone(),
        work: work.clone(),
        lint: std::env::var_os("SIM_WORLD").is_none_or(|v| v.is_empty()),
        state: Box::new(move |id| io::lc_state(&probe_env, id)),
    };
    let candidates: Vec<Candidate> = pool.iter().map(|m| Candidate { id: m.id.clone(), tip: m.tip.clone() }).collect();
    let open_round: Vec<Candidate> = open.map(|o| o.members.iter().map(|(id, tip)| Candidate { id: id.clone(), tip: tip.clone() }).collect()).unwrap_or_default();
    let out = sift::screen(&probe, &FileStore::new(work), &mut Live { env }, &candidates, &open_round);
    for e in &out.errors {
        eprintln!("batcher {}: {e}", repo.name);
    }
    sift::filter(pool, |m| m.id.as_str(), &out)
}
