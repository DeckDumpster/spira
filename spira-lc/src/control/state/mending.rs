//! `mending pickup|event|mend|tips|sweep|close-ended|list` on the control store, answering as
//! the dolt path does. Every timestamp is the call's clock, never the caller's, so a mender
//! cannot move its own deadline; `sweep` is how the machine applies `Expire` to an overdue row.

use lifecycle::batch::BatchState;
use lifecycle::mending::{
    self, MendFacts, MendingEventKind, MendingRow, MendingState, MEND_CAP_PER_BEAD,
};
use lifecycle::Refusal;
use serde_json::Value;
use state_store::{Backend, Request};

use super::Call;
use crate::control::store::{MendingRecord, Move, Stepped, Store};
use crate::{flag, CANNOT_TELL, REFUSED};

/// The verb this module answers.
pub const VERBS: &[&str] = &["mending"];

type Reply = Result<String, (i32, String)>;

const USAGE: &str = "usage: spira-lc mending pickup <batch> --pass N --suite S --deadline-secs D --actor A | event <batch> --pass N --suite S --kind <json> --actor A | mend <batch> --pass N --suite S --members bead:from:to,... --facts <json> --fences <json> --actor A | tips --beads a,b | sweep --actor A [--deadline-secs D] | close-ended --actor A [--batch B] | list [--batch B] [--live]";

pub fn run<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let m = Mender {
        store,
        now: call.now,
        req: Request::new(call.request, "mending", "lc.mending"),
    };
    let rest = args.get(2..).unwrap_or_default();
    let out = match args.get(1).map(String::as_str) {
        Some("pickup") => m.pickup(rest),
        Some("event") => m.event(rest),
        Some("sweep") => m.sweep(rest),
        Some("list") => m.list(rest),
        Some("close-ended") => m.close_ended(rest),
        Some("mend") => m.mend(rest),
        Some("tips") => m.tips(rest),
        _ => Err((CANNOT_TELL, USAGE.into())),
    };
    match out {
        Ok(s) => (0, s),
        Err(e) => e,
    }
}

fn cannot(e: state_store::Error) -> (i32, String) {
    (CANNOT_TELL, format!("cannot tell: {e}"))
}

fn actor(args: &[String], verb: &str) -> Result<String, (i32, String)> {
    flag(args, "--actor").ok_or((CANNOT_TELL, format!("{verb}: --actor is required")))
}

/// `bead:from:to,...`; none is no mend.
fn moves(spec: &str) -> Option<Vec<Move>> {
    let v: Vec<Move> = spec
        .split(',')
        .filter(|m| !m.is_empty())
        .map(|m| {
            let mut p = m.split(':');
            Some((
                p.next()?.to_string(),
                p.next()?.to_string(),
                p.next()?.to_string(),
            ))
        })
        .collect::<Option<_>>()?;
    let repeated = v
        .iter()
        .enumerate()
        .any(|(i, (b, _, _))| v[..i].iter().any(|(o, _, _)| o == b));
    (!v.is_empty() && !repeated).then_some(v)
}

const ENDED: [BatchState; 4] = [
    BatchState::Landed,
    BatchState::Settled,
    BatchState::Abandoned,
    BatchState::Discarded,
];

struct Mender<'a, B: Backend> {
    store: &'a Store<B>,
    now: i64,
    req: Request,
}

impl<B: Backend> Mender<'_, B> {
    fn target(&self, args: &[String]) -> Result<MendingRow, (i32, String)> {
        let pass = flag(args, "--pass").and_then(|p| p.parse::<u32>().ok());
        let (Some(batch), Some(pass), Some(suite)) = (args.first(), pass, flag(args, "--suite"))
        else {
            return Err((
                CANNOT_TELL,
                "mending: <batch>, --pass N and --suite S are required".into(),
            ));
        };
        crate::mending::check_widths(batch, pass, &suite)?;
        self.store
            .mending(batch, pass, &suite)
            .map_err(cannot)?
            .map(|r| r.row)
            .ok_or((
                CANNOT_TELL,
                format!("mending: no row for {batch} pass {pass} {suite}"),
            ))
    }

    fn apply(&self, row: &MendingRow, kind: MendingEventKind, actor: &str) -> Reply {
        let stepped = self
            .store
            .mending_event(row, kind, actor, self.now, &self.req)
            .map_err(cannot)?;
        match stepped {
            Stepped::Applied(s) => Ok(s.as_str().to_string()),
            Stepped::Refused(r) => Err((REFUSED, format!("refused: {:?}", Some(r)))),
            Stepped::LostRace | Stepped::NoRow => {
                Err((REFUSED, "refused: lost the race to another writer".into()))
            }
        }
    }

    fn pickup(&self, args: &[String]) -> Reply {
        let row = self.target(args)?;
        let deadline_s = flag(args, "--deadline-secs")
            .and_then(|d| d.parse::<u64>().ok())
            .ok_or((
                CANNOT_TELL,
                "mending pickup: --deadline-secs N is required".to_string(),
            ))?;
        let actor = actor(args, "mending")?;
        let kind = MendingEventKind::Pickup {
            at: self.now,
            deadline_s,
        };
        self.apply(&row, kind, &actor)
    }

    fn event(&self, args: &[String]) -> Reply {
        let row = self.target(args)?;
        let actor = actor(args, "mending")?;
        let mut kind: Value = flag(args, "--kind")
            .and_then(|k| serde_json::from_str(&k).ok())
            .ok_or((
                CANNOT_TELL,
                "mending event: --kind must be a JSON mending event".to_string(),
            ))?;
        let body = kind
            .as_object_mut()
            .and_then(|m| m.values_mut().next())
            .and_then(Value::as_object_mut);
        if let Some(body) = body.filter(|b| b.contains_key("at")) {
            body.insert("at".into(), Value::from(self.now));
        }
        let kind: MendingEventKind = serde_json::from_value(kind).map_err(|e| {
            (
                CANNOT_TELL,
                format!("mending event: --kind did not parse: {e}"),
            )
        })?;
        if matches!(
            kind,
            MendingEventKind::Pickup { .. }
                | MendingEventKind::Expire { .. }
                | MendingEventKind::Resume { .. }
        ) {
            return Err((
                REFUSED,
                "mending event: pickup, resume and expire are the machine's own, not a mender's"
                    .into(),
            ));
        }
        if matches!(kind, MendingEventKind::Mended { .. }) {
            return Err((
                REFUSED,
                "mending event: a mend goes through `mending mend`, which assesses the tip".into(),
            ));
        }
        self.apply(&row, kind, &actor)
    }

    fn ask_open(&self, ask: &str) -> Result<bool, (i32, String)> {
        Ok(self
            .store
            .ask(ask)
            .map_err(cannot)?
            .is_some_and(|a| !a.row.state.is_terminal()))
    }

    fn rows(
        &self,
        keep: impl Fn(&MendingRecord) -> bool,
    ) -> Result<Vec<MendingRow>, (i32, String)> {
        let mut rows: Vec<MendingRecord> = self.store.mendings().map_err(cannot)?;
        rows.retain(keep);
        rows.sort_by(|a, b| {
            let k = |r: &MendingRecord| {
                (
                    r.failed_at,
                    r.row.batch_id.clone(),
                    r.row.pass,
                    r.row.suite.clone(),
                )
            };
            k(a).cmp(&k(b))
        });
        Ok(rows.into_iter().map(|r| r.row).collect())
    }

    fn sweep(&self, args: &[String]) -> Reply {
        let actor = actor(args, "mending sweep")?;
        if let Some(deadline_s) = flag(args, "--deadline-secs").and_then(|d| d.parse::<u64>().ok())
        {
            let waiting = self
                .rows(|r| r.row.state == MendingState::Mending && r.row.awaiting_ask.is_some())?;
            for row in waiting {
                let ask = row.awaiting_ask.clone().unwrap_or_default();
                if !self.ask_open(&ask)? {
                    let kind = MendingEventKind::Resume {
                        at: self.now,
                        deadline_s,
                    };
                    let _ = self.apply(&row, kind, &actor);
                }
            }
        }
        let now = self.now;
        let overdue = self.rows(|r| {
            r.row.state == MendingState::Mending
                && r.row.awaiting_ask.is_none()
                && r.row.deadline.is_some_and(|d| d <= now)
        })?;
        let expired = overdue
            .iter()
            .filter(|row| {
                self.apply(row, MendingEventKind::Expire { at: now }, &actor)
                    .is_ok()
            })
            .count();
        Ok(expired.to_string())
    }

    fn close_ended(&self, args: &[String]) -> Reply {
        let actor = actor(args, "mending close-ended")?;
        let only = flag(args, "--batch");
        let ended = |id: &str| -> Result<bool, (i32, String)> {
            Ok(match self.store.batch(id).map_err(cannot)? {
                Some((_, r)) => ENDED.contains(&r.value.row.state),
                None => false,
            })
        };
        let mut open = Vec::new();
        for row in self.rows(|r| {
            matches!(r.row.state, MendingState::Waiting | MendingState::Mending)
                && only.as_deref().is_none_or(|b| b == r.row.batch_id)
        })? {
            if ended(&row.batch_id)? {
                open.push(row);
            }
        }
        let mut closed = 0;
        let now = self.now;
        for mut row in open {
            if row.overdue(now) {
                closed += usize::from(
                    self.apply(&row, MendingEventKind::Expire { at: now }, &actor)
                        .is_ok(),
                );
                continue;
            }
            if row.triage_ended_at.is_none()
                && self
                    .apply(&row, MendingEventKind::TriageEnded { at: now }, &actor)
                    .is_ok()
            {
                let reread = self.store.mending(&row.batch_id, row.pass, &row.suite);
                row = reread.map_err(cannot)?.map_or(row, |r| r.row);
            }
            let why = "the batch closed; the failure ended with it";
            let diagnosis = row
                .diagnosis
                .as_ref()
                .map_or(why.to_string(), |d| format!("{d}\n{why}"));
            let kind = MendingEventKind::Return { at: now, diagnosis };
            closed += usize::from(self.apply(&row, kind, &actor).is_ok());
        }
        Ok(closed.to_string())
    }

    fn list(&self, args: &[String]) -> Reply {
        let only = flag(args, "--batch");
        let live = args.iter().any(|a| a == "--live");
        let mut rows: Vec<MendingRecord> = self.store.mendings().map_err(cannot)?;
        rows.retain(|r| {
            only.as_deref().is_none_or(|b| b == r.row.batch_id)
                && (!live || matches!(r.row.state, MendingState::Waiting | MendingState::Mending))
        });
        rows.sort_by(|a, b| {
            let k = |r: &MendingRecord| {
                (
                    r.failed_at,
                    r.row.batch_id.clone(),
                    r.row.pass,
                    r.row.suite.clone(),
                )
            };
            k(a).cmp(&k(b))
        });
        let mut out = Vec::new();
        for r in rows {
            let mut v = serde_json::to_value(&r.row).unwrap_or(Value::Null);
            v["state"] = Value::from(r.row.state.as_str());
            v["failed_at"] = Value::from(r.failed_at);
            let waiting = match &r.row.awaiting_ask {
                Some(a) => self.ask_open(a)?,
                None => false,
            };
            v["ask_open"] = Value::from(waiting);
            out.push(v);
        }
        Ok(Value::Array(out).to_string())
    }

    fn refuse_logged(&self, row: &MendingRow, refusal: Refusal, actor: &str) -> (i32, String) {
        let refused = self
            .store
            .refuse_mend(row, refusal.clone(), actor, self.now, &self.req);
        match refused {
            Ok(()) => (REFUSED, format!("refused: {refusal:?}")),
            Err(e) => cannot(e),
        }
    }

    /// A mend the machine will not take returns the failure, the reason its diagnosis.
    fn return_with(&self, row: &MendingRow, why: &str, actor: &str) -> Reply {
        let diagnosis = match &row.diagnosis {
            Some(d) => format!("{d}\n{why}"),
            None => why.to_string(),
        };
        let kind = MendingEventKind::Return {
            at: self.now,
            diagnosis,
        };
        self.apply(row, kind, actor)
    }

    fn mend(&self, args: &[String]) -> Reply {
        let row = self.target(args)?;
        let actor = actor(args, "mending")?;
        let members = flag(args, "--members").and_then(|m| moves(&m)).ok_or((
            CANNOT_TELL,
            "mending mend: --members bead:from:to,... is required".to_string(),
        ))?;
        let facts: MendFacts = flag(args, "--facts")
            .and_then(|f| serde_json::from_str(&f).ok())
            .ok_or((
                CANNOT_TELL,
                "mending mend: --facts must be the mend's JSON facts".to_string(),
            ))?;
        let fences: Vec<String> = flag(args, "--fences")
            .and_then(|f| serde_json::from_str(&f).ok())
            .ok_or((
                CANNOT_TELL,
                "mending mend: --fences must be a JSON list of fence paths".to_string(),
            ))?;
        let may_mend = row.state == MendingState::Mending
            || (row.state == MendingState::Waiting && mending::may_settle_unpicked(&actor));
        if !may_mend {
            let illegal = Refusal::IllegalTransition {
                state: row.state.as_str().into(),
                event: "Mended".into(),
            };
            return Err(self.refuse_logged(&row, illegal, &actor));
        }
        for (bead, from, _) in &members {
            let prior = self.store.mends_of(bead).map_err(cannot)?;
            if prior.len() >= MEND_CAP_PER_BEAD {
                let diagnoses: Vec<&str> = prior
                    .iter()
                    .filter_map(|p| p.diagnosis.as_deref())
                    .collect();
                let why = format!(
                    "mend cap: {bead} was already mended {} times; diagnoses: {}",
                    prior.len(),
                    diagnoses.join(" | ")
                );
                self.return_with(&row, &why, &actor)?;
                let cap = Refusal::MendCap {
                    bead: bead.clone(),
                    mends: prior.len(),
                };
                return Err((
                    REFUSED,
                    format!("refused: {cap:?}; the failure is returned"),
                ));
            }
            if let Some(current) = prior.last().map(|p| p.tip.clone()) {
                if &current != from {
                    let why = format!("fix no longer applies: {bead} moved to {current} under this mend, which was built on {from}");
                    self.return_with(&row, &why, &actor)?;
                    let stale = Refusal::StaleTip {
                        bead: bead.clone(),
                        current,
                        given: from.clone(),
                    };
                    return Err((
                        REFUSED,
                        format!("refused: {stale:?}; the failure is returned"),
                    ));
                }
            }
        }
        if let Some(ask) = facts.ask.as_deref().filter(|a| !a.is_empty()) {
            if self.store.ask(ask).map_err(cannot)?.is_none() {
                let missing = Refusal::MendRefused {
                    reasons: vec![format!("ask {ask} does not exist")],
                };
                return Err(self.refuse_logged(&row, missing, &actor));
            }
        }
        if let Err(refusal) = mending::assess(&facts, &fences) {
            return Err(self.refuse_logged(&row, refusal, &actor));
        }
        let stepped = self
            .store
            .record_mend(
                &row,
                &members,
                facts.ask.as_deref(),
                &facts.checked,
                &actor,
                self.now,
                &self.req,
            )
            .map_err(cannot)?;
        match stepped {
            Stepped::Applied(s) => Ok(s.as_str().to_string()),
            Stepped::Refused(r) => Err((REFUSED, format!("refused: {r:?}"))),
            Stepped::LostRace | Stepped::NoRow => {
                Err((REFUSED, "refused: lost the race to another writer".into()))
            }
        }
    }

    /// Each bead's current mended tip, or null when it has none: what a second mender builds on.
    fn tips(&self, args: &[String]) -> Reply {
        let beads = flag(args, "--beads").ok_or((
            CANNOT_TELL,
            "mending tips: --beads a,b is required".to_string(),
        ))?;
        let mut out = serde_json::Map::new();
        for bead in beads.split(',').filter(|b| !b.is_empty()) {
            let tip = self
                .store
                .mends_of(bead)
                .map_err(cannot)?
                .pop()
                .map(|m| m.tip);
            out.insert(bead.to_string(), tip.map_or(Value::Null, Value::from));
        }
        Ok(Value::Object(out).to_string())
    }
}
