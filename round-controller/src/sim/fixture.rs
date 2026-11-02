//! The replay's inputs, read from a captured slice of the loop's own history
//! (`sim/fixtures/replay/<slice>/`, the world sim's typed records). Every certification pass the
//! slice holds is one planner request, at the time its pass started, with its members at their
//! tips, the head it ran, the round it was stacked on and the cause it ended with. A record the
//! replay needs and the slice lacks is refused by name, never defaulted.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use spira_sim::replay::{self as captured, BatchState, Record, Slice, Transition};

use crate::ports::Member;

/// How a recorded pass ended, from `cert_pass.ndjson`'s `cause`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Recorded {
    Green,
    Red,
    Infra,
    OverCap,
}

impl Recorded {
    pub const WORDS: [&'static str; 4] = ["green", "red-attributed", "infra", "over-cap"];

    pub fn from_word(w: &str) -> Option<Recorded> {
        let all = [
            Recorded::Green,
            Recorded::Red,
            Recorded::Infra,
            Recorded::OverCap,
        ];
        all.into_iter().find(|r| r.word() == w)
    }

    pub fn word(self) -> &'static str {
        match self {
            Recorded::Green => Self::WORDS[0],
            Recorded::Red => Self::WORDS[1],
            Recorded::Infra => Self::WORDS[2],
            Recorded::OverCap => Self::WORDS[3],
        }
    }
}

/// One recorded certification pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pass {
    pub repo: String,
    pub batch: String,
    /// Epoch milliseconds its pass started (the round's move to CI_RUNNING).
    pub start_ms: u64,
    /// Epoch milliseconds the pass ended, as `cert_pass.ndjson` stamps it.
    pub at_ms: u64,
    pub wall_secs: u64,
    pub cause: Recorded,
    pub members: Vec<Member>,
    /// The merged tree the pass ran: its PassStarted `sha`.
    pub head: String,
    pub parent: Option<String>,
}

/// A bead's live tip from `at_ms` on, from its Submit or Resubmit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tip {
    pub at_ms: u64,
    pub bead: String,
    pub tip: String,
}

/// A round the slice opened that no pass ran on: its members and when it opened and ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub repo: String,
    pub batch: String,
    pub members: Vec<Member>,
    pub opened_ms: u64,
    pub ended_ms: u64,
}

/// A quantity the slice's manifest tallies, counted from the slice itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tally {
    /// `fixture-passes:<repo>:<cause>`: certification passes of `repo` that ended so.
    Passes { repo: String, cause: Recorded },
    /// `fixture-repeat-starts:<bead>@<tip>`: pass starts in the window on that (member, tip).
    RepeatStarts { member: String },
    /// `fixture-stacked:<parent>`: distinct rounds stacked on `parent`.
    Stacked { parent: String },
    /// `fixture-green-members`: distinct beads a pass in the window certified green.
    GreenMembers,
}

impl Tally {
    pub fn parse(s: &str) -> Option<Tally> {
        if s == "fixture-green-members" {
            return Some(Tally::GreenMembers);
        }
        let (what, of) = s.split_once(':')?;
        match what {
            "fixture-passes" => {
                let (repo, cause) = of.split_once(':')?;
                Some(Tally::Passes {
                    repo: repo.into(),
                    cause: Recorded::from_word(cause)?,
                })
            }
            "fixture-repeat-starts" if of.contains('@') => {
                Some(Tally::RepeatStarts { member: of.into() })
            }
            "fixture-stacked" if !of.is_empty() => Some(Tally::Stacked { parent: of.into() }),
            _ => None,
        }
    }
}

pub struct Fixture {
    /// The slice's window, in epoch milliseconds, end exclusive.
    pub window_ms: (u64, u64),
    /// Every recorded pass, in the order they started.
    pub passes: Vec<Pass>,
    pub tips: Vec<Tip>,
    rounds: Vec<(u64, captured::Round)>,
}

fn ms(secs: u64) -> u64 {
    secs * 1000
}

fn members(of: &[captured::Member]) -> Vec<Member> {
    let mut m: Vec<Member> = of
        .iter()
        .map(|m| Member {
            id: m.bead.clone(),
            tip: m.tip.clone(),
        })
        .collect();
    m.sort();
    m
}

impl Fixture {
    /// The slice named `dir` under the world sim's fixtures (`1031`, `1021-1025`).
    pub fn load(dir: &str) -> Result<Fixture, String> {
        let slice = [Slice::Oct31, Slice::HeldOut]
            .into_iter()
            .find(|s| s.dir() == dir)
            .ok_or_else(|| format!("fixture: no captured slice {dir:?}"))?;
        let (from, to) = slice.window();
        Fixture::from_dir(&captured::fixtures_root().join(dir), (ms(from), ms(to)))
    }

    pub fn from_dir(dir: &Path, window_ms: (u64, u64)) -> Result<Fixture, String> {
        let records = captured::load(dir).map_err(|e| format!("fixture: {e}"))?;
        let mut rounds = Vec::new();
        let mut certs = Vec::new();
        let mut shas: BTreeMap<(String, u32), String> = BTreeMap::new();
        let mut tips = Vec::new();
        for (at, r) in records {
            match r {
                Record::Round(r) => rounds.push((at, r)),
                Record::CertPass(c) => certs.push((at, c)),
                Record::Transition(Transition::Batch {
                    id,
                    event,
                    sha: Some(sha),
                    n: Some(n),
                    ..
                }) if event == "PassStarted" => {
                    shas.insert((id, n), sha);
                }
                // A Submit the capture keeps with no tip names no live tip, so it moves none.
                Record::Transition(Transition::Bead {
                    id,
                    event,
                    tip: Some(tip),
                    ..
                }) if event == "Submit" || event == "Resubmit" => tips.push(Tip {
                    at_ms: ms(at),
                    bead: id,
                    tip,
                }),
                _ => {}
            }
        }
        let mut starts: BTreeMap<&str, Vec<(u64, &captured::Round)>> = BTreeMap::new();
        let mut parents: BTreeMap<&str, &str> = BTreeMap::new();
        for (at, r) in &rounds {
            if r.state_to == BatchState::CiRunning {
                starts.entry(&r.batch).or_default().push((*at, r));
            }
            if let Some(p) = &r.parent {
                parents.insert(&r.batch, p);
            }
        }
        let mut passes = Vec::new();
        for (at, c) in &certs {
            let named = format!("cert_pass {} pass {:?}", c.batch, c.pass);
            let cause = Recorded::from_word(&c.cause).ok_or_else(|| {
                format!(
                    "fixture: {named}: cause {:?} is none of {:?}",
                    c.cause,
                    Recorded::WORDS
                )
            })?;
            let of = starts
                .get(c.batch.as_str())
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let start = match c.pass {
                Some(n) => n.checked_sub(1).and_then(|i| of.get(i as usize)),
                None => of.iter().rev().find(|(s, _)| s <= at),
            }
            .ok_or_else(|| format!("fixture: {named}: no CI_RUNNING round record"))?;
            let n = c
                .pass
                .unwrap_or(of.iter().position(|s| s == start).unwrap_or(0) as u32 + 1);
            let head = shas
                .get(&(c.batch.clone(), n))
                .cloned()
                .ok_or_else(|| format!("fixture: {named}: no PassStarted sha for pass {n}"))?;
            passes.push(Pass {
                repo: c.repo.clone(),
                batch: c.batch.clone(),
                start_ms: ms(start.0),
                at_ms: ms(*at),
                wall_secs: c.wall_secs,
                cause,
                members: members(&start.1.members),
                head,
                parent: parents.get(c.batch.as_str()).map(|p| p.to_string()),
            });
        }
        passes.sort_by(|a, b| a.start_ms.cmp(&b.start_ms).then(a.batch.cmp(&b.batch)));
        Ok(Fixture {
            window_ms,
            passes,
            tips,
            rounds,
        })
    }

    /// A round no pass ran on, as the slice opened and ended it.
    pub fn span(&self, batch: &str) -> Result<Span, String> {
        if self.passes.iter().any(|p| p.batch == batch) {
            return Err(format!(
                "fixture: round {batch} ran a pass, so it is a request, not a span"
            ));
        }
        let of: Vec<&(u64, captured::Round)> = self
            .rounds
            .iter()
            .filter(|(_, r)| r.batch == batch)
            .collect();
        let opened = of
            .iter()
            .find(|(_, r)| r.state_from == BatchState::None)
            .ok_or_else(|| format!("fixture: round {batch}: no record of its open"))?;
        let ended = of
            .iter()
            .find(|(_, r)| r.state_to.terminal())
            .ok_or_else(|| format!("fixture: round {batch}: no record of its end"))?;
        Ok(Span {
            repo: opened.1.repo.clone(),
            batch: batch.into(),
            members: members(&opened.1.members),
            opened_ms: ms(opened.0),
            ended_ms: ms(ended.0),
        })
    }

    /// Every bead a pass in the window certified green.
    pub fn certified_green(&self) -> BTreeSet<String> {
        self.passes
            .iter()
            .filter(|p| p.cause == Recorded::Green && self.in_window(p.at_ms))
            .flat_map(|p| p.members.iter().map(|m| m.id.clone()))
            .collect()
    }

    fn in_window(&self, at_ms: u64) -> bool {
        (self.window_ms.0..self.window_ms.1).contains(&at_ms)
    }

    /// The tally the manifest states for the same quantity, counted the manifest's way.
    pub fn tally(&self, t: &Tally) -> u64 {
        match t {
            Tally::Passes { repo, cause } => self
                .passes
                .iter()
                .filter(|p| p.repo == *repo && p.cause == *cause)
                .count() as u64,
            Tally::RepeatStarts { member } => self
                .rounds
                .iter()
                .filter(|(at, r)| {
                    r.state_from == BatchState::Open
                        && r.state_to == BatchState::CiRunning
                        && self.in_window(ms(*at))
                })
                .flat_map(|(_, r)| r.members.iter())
                .filter(|m| format!("{}@{}", m.bead, m.tip) == *member)
                .count() as u64,
            Tally::Stacked { parent } => self
                .rounds
                .iter()
                .filter(|(_, r)| r.parent.as_deref() == Some(parent.as_str()))
                .map(|(_, r)| r.batch.as_str())
                .collect::<BTreeSet<_>>()
                .len() as u64,
            Tally::GreenMembers => self.certified_green().len() as u64,
        }
    }

    /// Rounds the slice opened in `repo` between `from_ms` and `until_ms` holding any of
    /// `held`, `except` aside: what was cut again while another round held them.
    pub fn opened_holding(
        &self,
        repo: &str,
        held: &[Member],
        from_ms: u64,
        until_ms: u64,
        except: &str,
    ) -> Vec<Span> {
        self.rounds
            .iter()
            .filter(|(_, r)| {
                r.repo == repo && r.batch != except && r.state_from == BatchState::None
            })
            .filter(|(at, _)| (from_ms..until_ms).contains(&ms(*at)))
            .filter(|(_, r)| {
                r.members
                    .iter()
                    .any(|m| held.iter().any(|x| x.id == m.bead))
            })
            .map(|(at, r)| Span {
                repo: r.repo.clone(),
                batch: r.batch.clone(),
                members: members(&r.members),
                opened_ms: ms(*at),
                ended_ms: ms(*at),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice(lines: &[(&str, &str)]) -> testkit::TempDir {
        let dir = testkit::TempDir::new("rc-fixture");
        for (f, _) in captured::FILES {
            let text: String = lines
                .iter()
                .filter(|(file, _)| *file == f)
                .map(|(_, l)| format!("{l}\n"))
                .collect();
            std::fs::write(dir.path().join(f), text).unwrap();
        }
        dir
    }

    const WINDOW: (u64, u64) = (1_793_404_800_000, 1_793_491_200_000);
    const OPEN: (&str, &str) = (
        "round.ndjson",
        r#"{"at":"2026-10-31T01:00:00Z","repo":"r","batch":"b1","state_from":"NONE","state_to":"OPEN","members":[{"bead":"b-2","tip":"t2"}],"cause":"MemberAdded"}"#,
    );
    const START: (&str, &str) = (
        "round.ndjson",
        r#"{"at":"2026-10-31T01:00:10Z","repo":"r","batch":"b1","state_from":"OPEN","state_to":"CI_RUNNING","members":[{"bead":"b-2","tip":"t2"},{"bead":"b-1","tip":"t1"}],"parent":"b0","cause":"PassStarted"}"#,
    );
    const SHA: (&str, &str) = (
        "transition.ndjson",
        r#"{"at":"2026-10-31T01:00:10Z","kind":"batch","id":"b1","from":"OPEN","to":"CI_RUNNING","event":"PassStarted","sha":"h1","n":1}"#,
    );
    const CERT: (&str, &str) = (
        "cert_pass.ndjson",
        r#"{"at":"2026-10-31T01:05:00Z","repo":"r","batch":"b1","pass":1,"pass_key":"k","cause":"green","place":"ec2","wall_secs":290}"#,
    );

    #[test]
    fn a_certified_pass_is_a_request_with_its_members_head_parent_and_cause() {
        let tip = (
            "transition.ndjson",
            r#"{"at":"2026-10-31T00:59:00Z","kind":"bead","id":"b-1","from":"WORKING","to":"SUBMITTED","event":"Submit","tip":"t1"}"#,
        );
        let f = Fixture::from_dir(slice(&[OPEN, START, SHA, CERT, tip]).path(), WINDOW).unwrap();
        let m = |id: &str, tip: &str| Member {
            id: id.into(),
            tip: tip.into(),
        };
        assert_eq!(
            f.passes,
            [Pass {
                repo: "r".into(),
                batch: "b1".into(),
                start_ms: 1_793_408_410_000,
                at_ms: 1_793_408_700_000,
                wall_secs: 290,
                cause: Recorded::Green,
                members: vec![m("b-1", "t1"), m("b-2", "t2")],
                head: "h1".into(),
                parent: Some("b0".into()),
            }]
        );
        assert_eq!(
            f.tips,
            [Tip {
                at_ms: 1_793_408_340_000,
                bead: "b-1".into(),
                tip: "t1".into()
            }]
        );
        assert_eq!(
            f.certified_green(),
            ["b-1".to_string(), "b-2".to_string()].into()
        );
        assert_eq!(f.tally(&Tally::parse("fixture-passes:r:green").unwrap()), 1);
        assert_eq!(
            f.tally(&Tally::parse("fixture-repeat-starts:b-1@t1").unwrap()),
            1
        );
        assert_eq!(f.tally(&Tally::parse("fixture-stacked:b0").unwrap()), 1);
        assert_eq!(Tally::parse("fixture-passes:r:blue"), None);
        assert_eq!(Tally::parse("fixture-stacked:"), None);
        assert_eq!(f.tally(&Tally::parse("fixture-green-members").unwrap()), 2);
    }

    #[test]
    fn a_pass_the_slice_cannot_place_is_refused_by_name() {
        let e = Fixture::from_dir(slice(&[OPEN, START, CERT]).path(), WINDOW)
            .err()
            .unwrap();
        assert_eq!(
            e,
            "fixture: cert_pass b1 pass Some(1): no PassStarted sha for pass 1"
        );
        let e = Fixture::from_dir(slice(&[OPEN, SHA, CERT]).path(), WINDOW)
            .err()
            .unwrap();
        assert_eq!(
            e,
            "fixture: cert_pass b1 pass Some(1): no CI_RUNNING round record"
        );
        let odd = (
            "cert_pass.ndjson",
            r#"{"at":"2026-10-31T01:05:00Z","repo":"r","batch":"b1","pass":1,"pass_key":"k","cause":"teal","place":"ec2","wall_secs":290}"#,
        );
        let e = Fixture::from_dir(slice(&[OPEN, START, SHA, odd]).path(), WINDOW)
            .err()
            .unwrap();
        assert!(e.contains("cause \"teal\" is none of"), "{e}");
        let unnumbered = (
            "cert_pass.ndjson",
            r#"{"at":"2026-10-31T01:05:00Z","repo":"r","batch":"b1","cause":"infra","place":"ec2","wall_secs":290}"#,
        );
        let f = Fixture::from_dir(slice(&[OPEN, START, SHA, unnumbered]).path(), WINDOW).unwrap();
        assert_eq!(
            (f.passes[0].head.as_str(), f.passes[0].cause),
            ("h1", Recorded::Infra)
        );
        assert!(Fixture::load("1999")
            .err()
            .unwrap()
            .contains("no captured slice \"1999\""));
    }

    #[test]
    fn a_span_is_a_round_no_pass_ran_on_and_one_without_its_end_is_refused() {
        let end = (
            "round.ndjson",
            r#"{"at":"2026-10-31T02:00:00Z","repo":"r","batch":"s1","state_from":"OPEN","state_to":"ABANDONED","members":[],"cause":"Abandon"}"#,
        );
        let open = (
            "round.ndjson",
            r#"{"at":"2026-10-31T01:30:00Z","repo":"r","batch":"s1","state_from":"NONE","state_to":"OPEN","members":[{"bead":"b-3","tip":"t3"}],"cause":"MemberAdded"}"#,
        );
        let f =
            Fixture::from_dir(slice(&[OPEN, START, SHA, CERT, open, end]).path(), WINDOW).unwrap();
        let s = f.span("s1").unwrap();
        assert_eq!(
            (s.opened_ms, s.ended_ms, s.members.len()),
            (1_793_410_200_000, 1_793_412_000_000, 1)
        );
        assert!(f.span("b1").err().unwrap().contains("ran a pass"));
        assert!(f
            .span("zz")
            .err()
            .unwrap()
            .contains("round zz: no record of its open"));
        let held = [Member {
            id: "b-3".into(),
            tip: "t0".into(),
        }];
        let (from, until) = (1_793_409_000_000, 1_793_412_000_000);
        let cuts = f.opened_holding("r", &held, from, until, "s0");
        assert_eq!(
            cuts.iter().map(|c| c.batch.as_str()).collect::<Vec<_>>(),
            ["s1"]
        );
        assert!(f.opened_holding("r", &held, from, until, "s1").is_empty());
        assert!(f
            .opened_holding("r", &held, until, until + 1, "s0")
            .is_empty());
        assert!(f
            .opened_holding("other", &held, from, until, "s0")
            .is_empty());
        let f = Fixture::from_dir(slice(&[open]).path(), WINDOW).unwrap();
        assert!(f.span("s1").err().unwrap().contains("no record of its end"));
    }
}
