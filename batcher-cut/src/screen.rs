//! The batcher's use of the `sift` screen. `sift_repo` screens the proto-round on its own pass;
//! `cut_pool` is all a cut does with it: take what already has a pass at its current tip, before the round is selected from it.

use std::collections::BTreeMap;

use batcher::core::Member;
use sift::{Acts, Candidate, FileStore, GitProbe};
use spira_config::process::cfg;

use crate::io::{self, Env, OpenBatch, Repo};

struct Live<'a> {
    env: &'a Env,
    repo: &'a Repo,
    sifted: BTreeMap<String, String>,
}

impl Acts for Live<'_> {
    fn note(&mut self, id: &str, text: &str) -> Result<(), String> {
        io::bead_note(self.env, id, text)
    }
    fn gate_red(&mut self, id: &str, tip: &str, reason: &str) -> Result<(), String> {
        let lc = |a: &[String]| io::lcq(self.env, &a.iter().map(String::as_str).collect::<Vec<_>>());
        if let Err(e) = spira_config::ejection::keep_sift(&lc, &self.env.run, id, reason) {
            eprintln!("batcher: sift: {id} send-back logs not kept: {e}");
        }
        io::lc_gate_red(self.env, id, tip, reason)
    }
    fn send_back(&mut self, id: &str, tip: &str, text: &str) -> Result<(), String> {
        let cause = spira_config::send_back::encode(tip, text);
        io::lcq(self.env, &["fact", "add", id, "--kind", spira_config::send_back::KIND, "--actor", "sift", "--cause", &cause]).map(|_| ())
    }
    fn supersede(&mut self, id: &str, keeper: &str) -> Result<(), String> {
        io::lc_supersede(self.env, id, keeper)
    }
    fn pass(&mut self, id: &str, tip: &str) -> Result<(), String> {
        if self.sifted.get(id).map(String::as_str) == Some(tip) {
            return Ok(());
        }
        io::lc_sifted(self.env, id, tip)
    }
    fn rebase(&mut self, id: &str, _tip: &str) -> i32 {
        io::rebase_stale(self.env, &self.repo.name, id)
    }
    fn tell(&mut self, msg: &str) {
        tell_concierge(inbox_rules::Sink::configured("batcher"), msg, spira_config::vtime::now_epoch());
    }
}

/// What the screen did, said on stdout and in the Concierge inbox.
pub fn tell_concierge(sink: Result<inbox_rules::Sink, String>, msg: &str, now: u64) {
    println!("batcher: {msg}");
    let line = format!("{} [watch:sift] {msg}", io::utc_stamp(now));
    if let Err(e) = sink.and_then(|sink| sink.record(&line)) {
        eprintln!("batcher: sift: concierge not told ({e}): {msg}");
    }
}

/// The members whose current tip carries a pass. A bead with none is not yet screened and waits
/// for the next cut.
pub fn cut_pool(pool: Vec<Member>, sifted: &BTreeMap<String, String>) -> Vec<Member> {
    pool.into_iter()
        .filter(|m| sifted.get(&m.id).map(String::as_str) == Some(m.tip.as_str()))
        .collect()
}

/// Screens every SUBMITTED and CERTIFIED bead of `repo`, recording a pass or sending the bead back.
pub fn sift_repo(
    env: &Env,
    repo: &Repo,
    open: Option<&OpenBatch>,
) -> Result<sift::Outcome, String> {
    let pool = io::certified_pool(env, repo)?;
    if pool.is_empty() {
        return Ok(sift::Outcome::default());
    }
    io::fetch_base(repo).map_err(|e| format!("base not fetched ({e})"))?;
    let work = env.run.join("sift");
    let probe_env = env.clone();
    let probe = GitProbe {
        repo: repo.path.clone(),
        name: repo.name.clone(),
        registered: io::registry(env).names(),
        wiki: cfg("SPIRA_WIKI").unwrap_or_default(),
        identity_projects: cfg("SPIRA_IDENTITY_PROJECTS").unwrap_or_default(),
        base_ref: repo.base.clone(),
        work: work.clone(),
        state: Box::new(move |id| io::lc_state(&probe_env, id)),
        shown: Box::new({
            let env = env.clone();
            move |id| io::bd_show_text(&env, id)
        }),
    };
    let candidates: Vec<Candidate> = pool
        .iter()
        .map(|m| Candidate {
            id: m.id.clone(),
            tip: m.tip.clone(),
        })
        .collect();
    let open_round: Vec<Candidate> = open
        .map(|o| {
            o.members
                .iter()
                .map(|(id, tip)| Candidate {
                    id: id.clone(),
                    tip: tip.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    let mut live = Live {
        env,
        repo,
        sifted: io::sifted_tips(env)?,
    };
    let out = sift::screen(
        &probe,
        &FileStore::new(work),
        &mut live,
        &candidates,
        &open_round,
    );
    for e in &out.errors {
        eprintln!("batcher {}: {e}", repo.name);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: &str, tip: &str) -> Member {
        Member {
            id: id.into(),
            tip: tip.into(),
            title: String::new(),
            priority: None,
            express: false,
            base_fix: false,
            certified_at: 0,
            stack: Default::default(),
            blocked_by: vec![],
        provisional: false,
        }
    }

    #[test_id(t50ltgi)]
    #[test]
    fn a_cut_takes_only_beads_with_a_pass_at_their_current_tip() {
        let sifted: BTreeMap<String, String> = [("sp-a", "t1"), ("sp-b", "old")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let pool = vec![
            member("sp-a", "t1"),
            member("sp-b", "new"),
            member("sp-c", "t3"),
        ];
        let ids: Vec<String> = cut_pool(pool, &sifted).into_iter().map(|m| m.id).collect();
        assert_eq!(
            ids,
            ["sp-a"],
            "a moved tip and an unscreened bead both wait"
        );
    }
}
