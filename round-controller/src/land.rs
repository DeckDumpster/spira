//! The Lander: each GREEN round of a repo, lowest in its stack first, is landed under a fresh
//! lease of its own, or held GREEN with a typed blocker in `batches/<repo>/<id>/status`, or
//! superseded. There is no abandon arm. A held round is tried again only once an input to its
//! refusal changed (law-a-retry-must-change-an-input), and every landing moves the lane's
//! `landing_ref` in the txn that records it, so the next round's base is the ref it landed on.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use state_store::{Compare, Object, Op, Request};

use lifecycle::batch::{apply, BatchEvent, BatchEventKind, BatchState};

use crate::lease::{self, acquire, fenced_txn, holder_of, Lease, LeaseError, Resource};
use crate::orphans::HOLDER;
use crate::outcome::Outcome;
use crate::ports::{Ports, Verb, VerbRefusal};
use crate::refusal::{self, Refusal};
use crate::resources::{round_key, RoundView};
use crate::stack::{self, lane_key, Live, Stack};

pub const STEP: &str = "land";

/// `queue::ops::DEPLOY_FAULT`: the landing stands and the release was not activated.
pub const DEPLOY_FAULT_EXIT: i32 = 3;

/// Why a GREEN round was not landed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "blocker")]
pub enum Blocker {
    /// The round below it in the stack has not landed.
    StackedOnUnlanded {
        parent: String,
        parent_state: Option<String>,
        parent_leased: bool,
    },
    /// `land` refused with a word the Lander does not act on; `text` is its stderr.
    Refused { text: String },
    /// `land` exited with the deploy-fault code: the release was not activated.
    DeployFault { why: String },
}

impl Blocker {
    pub const WORDS: [&'static str; 3] = ["stacked-on-unlanded", "refused", "deploy-fault"];

    pub fn word(&self) -> &'static str {
        match self {
            Blocker::StackedOnUnlanded { .. } => Self::WORDS[0],
            Blocker::Refused { .. } => Self::WORDS[1],
            Blocker::DeployFault { .. } => Self::WORDS[2],
        }
    }
}

/// What a held round's refusal was answered against. Its text is the blocker's; these are the
/// inputs that can change it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inputs {
    pub head: String,
    pub base: String,
    pub parent: Option<String>,
    pub parent_state: Option<String>,
    pub parent_head: Option<String>,
}

/// `batches/<repo>/<id>/status`: the round's blocker, what it was refused against, and when the
/// Lander first found the round GREEN.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    #[serde(flatten)]
    pub blocker: Blocker,
    pub inputs: Inputs,
    pub green_seen_ms: u64,
}

pub fn status_key(repo: &str, batch: &str) -> String {
    format!("{}/status", round_key(repo, batch))
}

/// Where a repo's landing ref is read from, for a refusal that names it.
pub fn git_key(repo: &str) -> String {
    format!("git/{repo}/landing-ref")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Landing {
    /// `land` returned `Ok` and the landing ref is now `at`.
    Landed {
        at: String,
    },
    /// The landing ref already held the head; nothing was pushed.
    FastForward {
        at: String,
    },
    Held(Blocker),
    /// Held, and no input to its refusal has changed: nothing called, nothing written.
    Unchanged,
    Superseded,
    /// Another holder's lease is live on the round: it is left to them this pass.
    Leased,
    /// Not GREEN by the time its lease was taken.
    NotGreen,
}

/// Every GREEN round of `repo`, lowest in the stack first: each one's [`Landing`]. A repo with no
/// lane record is not the controller's yet. A round no longer grounded is supersede's, so it is
/// skipped; one cut before stacking carries no `tested_base` and is taken at the bottom. The
/// first refusal is answered after every round has had its turn.
pub fn land(p: &Ports, repo: &str) -> Result<Vec<(String, Landing)>, Refusal> {
    let Some(lane) = stack::lane(p, repo)? else {
        return Ok(Vec::new());
    };
    let rounds = stack::live(p, repo)?;
    let stack = Stack::of(&lane, &rounds);
    let mut green: Vec<(usize, &Live)> = rounds
        .iter()
        .filter(|r| r.round.row.state == BatchState::Green)
        .filter_map(|r| match r.round.tested_base {
            None => Some((1, r)),
            Some(_) => stack.height.get(&r.batch).map(|h| (*h, r)),
        })
        .collect();
    green.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.batch.cmp(&b.1.batch)));
    let mut out = Vec::new();
    let mut first = None;
    for (_, r) in green {
        let parent = stack
            .below
            .get(&r.batch)
            .cloned()
            .or_else(|| r.round.row.parent.clone());
        match one(p, repo, &r.batch, parent.as_deref()) {
            Ok(l) => out.push((r.batch.clone(), l)),
            Err(e) => {
                first.get_or_insert(e);
            }
        }
    }
    match first {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

/// `pass`'s land step: [`land`], its refusal told once.
pub fn step(p: &Ports, repo: &str) {
    let r = land(p, repo);
    refusal::note(p, repo, STEP, r.as_ref().err());
}

struct Parent {
    batch: String,
    state: Option<BatchState>,
    head: Option<String>,
}

fn parent_of(p: &Ports, repo: &str, batch: Option<&str>) -> Result<Option<Parent>, Refusal> {
    let Some(batch) = batch else { return Ok(None) };
    let view = RoundView::read(p.store, repo, batch)
        .map_err(|e| Refusal::unreadable(&round_key(repo, batch), e))?;
    Ok(Some(Parent {
        batch: batch.to_string(),
        state: view.as_ref().map(|v| v.round.row.state),
        head: view.and_then(|v| v.round.row.head),
    }))
}

fn leased(p: &Ports, repo: &str, batch: &str) -> Result<bool, Refusal> {
    let res = Resource::round(repo, batch);
    holder_of(p, &res)
        .map(|h| h.is_some())
        .map_err(|e| Refusal::unreadable(&res.lock_key(), e))
}

fn read_status(p: &Ports, key: &str) -> Result<Option<Status>, Refusal> {
    p.store
        .get(key)
        .map_err(|e| Refusal::unreadable(key, e))?
        .map(|i| serde_json::from_value(i.value.data).map_err(|e| Refusal::unreadable(key, e)))
        .transpose()
}

/// One GREEN round: landed, held or superseded, or left as it stands when nothing it was held
/// on has changed.
fn one(p: &Ports, repo: &str, batch: &str, parent: Option<&str>) -> Result<Landing, Refusal> {
    let key = round_key(repo, batch);
    let Some(view) =
        RoundView::read(p.store, repo, batch).map_err(|e| Refusal::unreadable(&key, e))?
    else {
        return Ok(Landing::NotGreen);
    };
    let Some(head) = view
        .round
        .row
        .head
        .clone()
        .filter(|_| view.round.row.state == BatchState::Green)
    else {
        return Ok(Landing::NotGreen);
    };
    let lane = stack::lane(p, repo)?.ok_or_else(|| Refusal::LaneUnknown {
        key: lane_key(repo),
    })?;
    let parent = parent_of(p, repo, parent)?;
    let inputs = Inputs {
        head: head.clone(),
        base: lane.landing_ref.clone(),
        parent: parent.as_ref().map(|q| q.batch.clone()),
        parent_state: parent
            .as_ref()
            .and_then(|q| q.state.map(|s| s.as_str().to_string())),
        parent_head: parent.as_ref().and_then(|q| q.head.clone()),
    };
    let status = read_status(p, &status_key(repo, batch))?;
    if status.as_ref().is_some_and(|s| s.inputs == inputs) {
        return Ok(Landing::Unchanged);
    }
    let lease = match acquire(p, &Resource::round(repo, batch), HOLDER) {
        Ok(l) => l,
        Err(LeaseError::Held(_)) => return Ok(Landing::Leased),
        Err(LeaseError::Terminal) => return Ok(Landing::NotGreen),
        Err(e) => {
            return Err(Refusal::unreadable(
                &Resource::round(repo, batch).lock_key(),
                format!("{e:?}"),
            ))
        }
    };
    let held = Held {
        p,
        repo,
        batch,
        lease: &lease,
        inputs,
        status_existed: status.is_some(),
        green_seen_ms: status.map_or_else(|| p.clock.now_ms(), |s| s.green_seen_ms),
    };
    let still_green = RoundView::read(p.store, repo, batch)
        .map_err(|e| Refusal::unreadable(&key, e))
        .map(|v| v.is_some_and(|v| v.round.row.state == BatchState::Green));
    let r = match still_green {
        Ok(true) => held.decide(&head, parent),
        Ok(false) => Ok(Landing::NotGreen),
        Err(e) => Err(e),
    };
    let _ = lease.release(p);
    r
}

struct Held<'a> {
    p: &'a Ports<'a>,
    repo: &'a str,
    batch: &'a str,
    lease: &'a Lease,
    inputs: Inputs,
    green_seen_ms: u64,
    status_existed: bool,
}

impl Held<'_> {
    fn decide(&self, head: &str, parent: Option<Parent>) -> Result<Landing, Refusal> {
        let (p, repo) = (self.p, self.repo);
        if let Some(q) = parent.filter(|q| q.state.is_some_and(|s| !s.is_terminal())) {
            return self.hold(Blocker::StackedOnUnlanded {
                parent_leased: leased(p, repo, &q.batch)?,
                parent: q.batch,
                parent_state: q.state.map(|s| s.as_str().to_string()),
            });
        }
        match p.git.landed(repo, head) {
            Err(e) => return Err(Refusal::unreadable(&git_key(repo), e)),
            Ok(true) => {
                let at = p
                    .git
                    .landing_sha(repo)
                    .map_err(|e| Refusal::unreadable(&git_key(repo), e))?;
                self.record_landed(head, &at, None)?;
                p.log.write(&Outcome::fast_forward(
                    p.clock.now_ms(),
                    repo,
                    self.batch,
                    self.dwell(),
                ));
                return Ok(Landing::FastForward { at });
            }
            Ok(false) => {}
        }
        let token = lease::token_word(self.lease.token());
        match p.verbs.land(repo, self.batch, Some(&token)) {
            Ok(_) => {
                let at = self.ref_after(head);
                self.record_landed(head, &at, None)?;
                p.log.write(&Outcome::landing(
                    p.clock.now_ms(),
                    repo,
                    self.batch,
                    self.dwell(),
                ));
                Ok(Landing::Landed { at })
            }
            Err(r) => self.refused(head, r, &token),
        }
    }

    /// A refusal is classified by its exit and its first word only (law-a-matcher-reads-code-not-prose).
    fn refused(&self, head: &str, r: VerbRefusal, token: &str) -> Result<Landing, Refusal> {
        let (p, repo, batch) = (self.p, self.repo, self.batch);
        let Some(code) = r.code else {
            return Err(Refusal::Verb {
                batch: Some(batch.into()),
                refusal: r,
            });
        };
        if code == DEPLOY_FAULT_EXIT {
            let why = Blocker::DeployFault {
                why: r.text.trim().to_string(),
            };
            return match p.git.landed(repo, head) {
                Err(e) => Err(Refusal::unreadable(&git_key(repo), e)),
                Ok(true) => {
                    let at = self.ref_after(head);
                    self.record_landed(head, &at, Some(&why))?;
                    self.log_held(&why);
                    Ok(Landing::Landed { at })
                }
                Ok(false) => self.hold(why),
            };
        }
        let word = r.text.split_whitespace().next().unwrap_or("");
        match word.trim_end_matches(':') {
            "stacked" => {
                let parent = parent_of(p, repo, self.inputs.parent.as_deref())?;
                let parent_leased = match &parent {
                    Some(q) => leased(p, repo, &q.batch)?,
                    None => false,
                };
                self.hold(Blocker::StackedOnUnlanded {
                    parent: self.inputs.parent.clone().unwrap_or_default(),
                    parent_state: parent.and_then(|q| q.state.map(|s| s.as_str().to_string())),
                    parent_leased,
                })
            }
            "base-moved" => {
                let s = p.verbs.supersede(repo, batch, Some(token));
                p.log.write(&Outcome::of(
                    p.clock.now_ms(),
                    repo,
                    Some(batch),
                    Verb::Supersede,
                    &s,
                ));
                s.map(|_| Landing::Superseded)
                    .map_err(|refusal| Refusal::Verb {
                        batch: Some(batch.into()),
                        refusal,
                    })
            }
            // `refused`, and any word the Lander does not know, holds the round as it is.
            _ => self.hold(Blocker::Refused {
                text: r.text.trim().to_string(),
            }),
        }
    }

    /// The landing ref after a landing of `head`: git's, when git already shows `head` under it,
    /// else `head`, so a stale read never moves the lane behind what landed.
    fn ref_after(&self, head: &str) -> String {
        let (git, repo) = (self.p.git, self.repo);
        match (git.landed(repo, head), git.landing_sha(repo)) {
            (Ok(true), Ok(sha)) => sha,
            _ => head.to_string(),
        }
    }

    fn dwell(&self) -> u64 {
        self.p.clock.now_ms().saturating_sub(self.green_seen_ms)
    }

    fn status(&self, b: &Blocker) -> Value {
        json!(Status {
            blocker: b.clone(),
            inputs: self.inputs.clone(),
            green_seen_ms: self.green_seen_ms,
        })
    }

    fn request(&self, what: &str) -> Request {
        Request::new(
            &format!("rc-land-{what}-{:016x}", self.p.next_u64()),
            HOLDER,
            &format!("land.{what}"),
        )
    }

    fn log_held(&self, b: &Blocker) {
        self.p.log.write(&Outcome::held(
            self.p.clock.now_ms(),
            self.repo,
            self.batch,
            b.word(),
            &status_key(self.repo, self.batch),
        ));
    }

    /// The round stays GREEN; its blocker and the inputs it was refused against are kept, fenced.
    fn hold(&self, b: Blocker) -> Result<Landing, Refusal> {
        let skey = status_key(self.repo, self.batch);
        let t = fenced_txn(
            self.lease,
            vec![],
            vec![Op::Put(skey.clone(), Object::new(self.status(&b)))],
        );
        match self.p.store.txn(&t, &self.request("hold")) {
            Ok(r) if r.succeeded => {
                self.log_held(&b);
                Ok(Landing::Held(b))
            }
            Ok(_) => Err(Refusal::unreadable(&skey, "the Lander's lease lapsed")),
            Err(e) => Err(Refusal::unreadable(&skey, e)),
        }
    }

    /// One fenced txn: the round LANDED by `FastForward { sha: head }`, the lane's `landing_ref`
    /// moved to `at`, and the status cleared, or kept as `kept` when the landing carries a
    /// blocker of its own. Both records are compared, and re-read when another writer won.
    fn record_landed(&self, head: &str, at: &str, kept: Option<&Blocker>) -> Result<(), Refusal> {
        const TRIES: usize = 3;
        let (p, repo, batch) = (self.p, self.repo, self.batch);
        let (rkey, lkey, skey) = (
            round_key(repo, batch),
            lane_key(repo),
            status_key(repo, batch),
        );
        for _ in 0..TRIES {
            let round = p
                .store
                .get(&rkey)
                .map_err(|e| Refusal::unreadable(&rkey, e))?
                .ok_or_else(|| Refusal::unreadable(&rkey, "the round is gone"))?;
            let lane = p
                .store
                .get(&lkey)
                .map_err(|e| Refusal::unreadable(&lkey, e))?
                .ok_or_else(|| Refusal::LaneUnknown { key: lkey.clone() })?;
            let mut rv = round.value.clone();
            landed_value(&mut rv.data, head).map_err(|e| Refusal::unreadable(&rkey, e))?;
            let mut lv = lane.value.clone();
            lv.data
                .as_object_mut()
                .ok_or_else(|| Refusal::unreadable(&lkey, "the lane is not an object"))?
                .insert("landing_ref".into(), json!(at));
            let mut ops = vec![Op::Put(rkey.clone(), rv), Op::Put(lkey.clone(), lv)];
            match kept {
                Some(b) => ops.push(Op::Put(skey.clone(), Object::new(self.status(b)))),
                None if self.status_existed => ops.push(Op::Delete(skey.clone())),
                None => {}
            }
            let t = fenced_txn(
                self.lease,
                vec![
                    Compare::ModRev(rkey.clone(), Some(round.mod_rev)),
                    Compare::ModRev(lkey.clone(), Some(lane.mod_rev)),
                ],
                ops,
            );
            let r = p
                .store
                .txn(&t, &self.request("record"))
                .map_err(|e| Refusal::unreadable(&rkey, e))?;
            if r.succeeded {
                return Ok(());
            }
            let ours = holder_of(p, self.lease.resource())
                .map_err(|e| Refusal::unreadable(&rkey, e))?
                .is_some_and(|h| h.token == self.lease.token());
            if !ours {
                return Err(Refusal::unreadable(
                    &rkey,
                    "the Lander's lease lapsed before the landing was recorded",
                ));
            }
        }
        Err(Refusal::unreadable(
            &rkey,
            format!("the round or its lane moved under {TRIES} tries to record the landing"),
        ))
    }
}

/// The round's value as LANDED by `FastForward { sha: head }`; one already LANDED stays so.
pub fn landed_value(v: &mut Value, head: &str) -> Result<(), String> {
    let mut round: crate::resources::Round =
        serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
    if round.row.state != BatchState::Landed {
        let ev = BatchEvent {
            expect: round.row.state,
            version: round.row.version,
            kind: BatchEventKind::FastForward { sha: head.into() },
            actor: HOLDER.into(),
        };
        let o = apply(&round.row, &ev);
        if !o.applied {
            return Err(format!("FastForward refused: {:?}", o.refusal));
        }
        round.row = o.row;
    }
    let landed = serde_json::to_value(&round).map_err(|e| e.to_string())?;
    let (Some(obj), Value::Object(fields)) = (v.as_object_mut(), landed) else {
        return Err("the round is not an object".into());
    };
    obj.extend(fields);
    Ok(())
}

#[cfg(test)]
mod tests;
