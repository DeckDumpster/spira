//! The bead store: lib.sh `bdq`'s calling convention, and the pass's one snapshot with
//! every set the checks derive from it (DESIGN.md §2.4, G1).

use std::collections::{BTreeSet, HashMap};

use crate::cfg::{Cfg, Partition};
use crate::host::{Host, Out, Spec};
use crate::model::{parse_beads, Bead, LcRow};
use spira_config::lc_state;

/// lib.sh `bdq`: `timeout $BD_TIMEOUT $SPIRA_BD -C $SPIRA_DB …`, retried while the failure
/// is a dropped pooled connection; refuses outright with no SPIRA_DB.
pub struct Bd<'a> {
    pub cfg: &'a Cfg,
}

impl<'a> Bd<'a> {
    pub fn spec(&self, args: &[String]) -> Spec {
        if let Some(fx) = &self.cfg.bd_fixture {
            let mut a = vec![
                self.cfg
                    .home
                    .join("bdsim.py")
                    .to_string_lossy()
                    .into_owned(),
                fx.clone(),
            ];
            a.extend(args.iter().cloned());
            return Spec::args_owned("python3", a);
        }
        let mut a = vec!["-C".to_string(), self.cfg.db.clone()];
        a.extend(args.iter().cloned());
        Spec::args_owned(self.cfg.bd.clone(), a).timeout(self.cfg.bd_timeout)
    }

    pub fn call(&self, h: &Host, args: &[&str], stdin: Option<&str>) -> Out {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        self.call_owned(h, &args, stdin)
    }

    pub fn call_owned(&self, h: &Host, args: &[String], stdin: Option<&str>) -> Out {
        if self.cfg.db.is_empty() && self.cfg.bd_fixture.is_none() {
            return Out {
                rc: 1,
                stdout: String::new(),
                stderr: "bdq: refusing - SPIRA_DB is empty/unset (would fall through to bd auto-discovery)".into(),
            };
        }
        let mut tries = 1;
        loop {
            let mut s = self.spec(args);
            if let Some(i) = stdin {
                s = s.stdin(i.as_bytes().to_vec());
            }
            let o = h.run(s);
            if o.ok() || tries >= self.cfg.bd_tries || !o.stderr.contains("invalid connection") {
                return o;
            }
            tries += 1;
        }
    }

    /// `bdjson`: the call with `--json`, its stdout; Err(rc, stderr) when bd failed.
    pub fn json(&self, h: &Host, args: &[String]) -> Result<String, Out> {
        let mut a = args.to_vec();
        a.push("--json".into());
        let o = self.call_owned(h, &a, None);
        if o.ok() {
            Ok(o.stdout)
        } else {
            Err(o)
        }
    }

    /// A write whose outcome only matters as rc (`bdq … >/dev/null 2>&1`).
    pub fn quiet(&self, h: &Host, args: &[&str], stdin: Option<&str>) -> bool {
        self.call(h, args, stdin).ok()
    }
}

/// `ready_raw_args`: the broadest claimable query (no scope label).
pub fn ready_raw_args(cfg: &Cfg) -> Vec<String> {
    let mut a: Vec<String> = [
        "ready",
        "--limit",
        "0",
        "--exclude-type",
        "epic,event",
        "-u",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if !cfg.no_loop.is_empty() {
        a.push("--exclude-label".into());
        a.push(cfg.no_loop.clone());
    }
    a
}

/// `READY_ARGS`: the claim query every persona's count goes through.
pub fn ready_args(cfg: &Cfg) -> Vec<String> {
    let mut a: Vec<String> = [
        "ready",
        "--limit",
        "0",
        "--exclude-type",
        "epic,event",
        "-u",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if !cfg.scope.is_empty() {
        a.push("--label".into());
        a.push(cfg.scope.clone());
    }
    if !cfg.no_loop.is_empty() {
        a.push("--exclude-label".into());
        a.push(cfg.no_loop.clone());
    }
    a
}

pub fn list_all_args() -> Vec<String> {
    ["list", "--all", "--limit", "0"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// labels ⊇ need.
pub fn has_all(b: &Bead, need: &[String]) -> bool {
    need.iter().all(|n| b.has(n))
}

/// labels ∩ excl = ∅.
pub fn has_none(b: &Bead, excl: &[String]) -> bool {
    !excl.iter().any(|x| b.has(x))
}

/// One read of the store, and the pass's view of it.
#[derive(Debug, Default)]
pub struct Snapshot {
    pub list: Vec<Bead>,
    pub list_raw: String,
    pub ready: Option<Vec<Bead>>,
    pub ready_raw: String,
    index: HashMap<String, usize>,
    /// Each work bead's lifecycle state (`spira-lc list`), keyed by id: the pass's state
    /// view (design §3.4: bd holds content, spira-lc holds state). Empty when the machine
    /// could not be read — then no bead has a state, and nothing is decided from one.
    lc: HashMap<String, String>,
}

impl Snapshot {
    pub fn new(list_raw: String, list: Vec<Bead>, ready: Option<(String, Vec<Bead>)>) -> Snapshot {
        let index = list
            .iter()
            .enumerate()
            .map(|(i, b)| (b.id.clone(), i))
            .collect();
        let (ready_raw, ready) = match ready {
            Some((r, v)) => (r, Some(v)),
            None => (String::new(), None),
        };
        Snapshot {
            list,
            list_raw,
            ready,
            ready_raw,
            index,
            lc: HashMap::new(),
        }
    }

    /// Attach the pass's one lifecycle read.
    pub fn with_lc(mut self, rows: Option<&[LcRow]>) -> Snapshot {
        self.set_lc(rows);
        self
    }

    pub fn set_lc(&mut self, rows: Option<&[LcRow]>) {
        self.lc = rows
            .unwrap_or_default()
            .iter()
            .map(|r| (r.bead_id.clone(), r.state.clone()))
            .collect();
    }

    /// The bead's lifecycle state, or None when the machine has no row for it.
    pub fn lc_state(&self, id: &str) -> Option<&str> {
        self.lc.get(id).map(String::as_str)
    }

    /// The builder still owes the bead work (READY, WORKING, REWORK). A bead with no
    /// lifecycle row is not: every work bead gets its row at creation (CHECK-ROWLESS
    /// backfills a ready one that missed it), so a rowless bead is not live work.
    pub fn lc_open(&self, id: &str) -> bool {
        self.lc_state(id).is_some_and(|s| !lc_state::past_builder(s))
    }

    /// The builder has handed the bead on (what bd `closed` meant for a work bead).
    pub fn lc_past_builder(&self, id: &str) -> bool {
        self.lc_state(id).is_some_and(lc_state::past_builder)
    }

    #[cfg(test)]
    pub fn from_json(list_json: &str, ready_json: Option<&str>) -> Snapshot {
        let list = parse_beads(list_json).unwrap_or_default();
        let ready = ready_json.and_then(|r| parse_beads(r).ok().map(|v| (r.to_string(), v)));
        Snapshot::new(list_json.to_string(), list, ready)
    }

    pub fn get(&self, id: &str) -> Option<&Bead> {
        self.index.get(id).map(|&i| &self.list[i])
    }

    /// The open plan backlog: every bead carrying `<scope,>plan` whose lifecycle row still
    /// owes builder work (READY/WORKING/REWORK) and is work (not an epic or event). Spira works this whole backlog continuously — there is
    /// no goal epic whose children stand for "the work" (sp-k6m1m) — so this is what the
    /// pass reports as `open` and what CHECK 3 and CHECK 8 reason about.
    pub fn plan_open(&self, cfg: &Cfg) -> Vec<String> {
        let need = cfg.plan_labels();
        self.list
            .iter()
            .filter(|b| self.lc_open(&b.id) && !matches!(b.typ(), "epic" | "event") && has_all(b, &need))
            .map(|b| b.id.clone())
            .collect()
    }

    /// ready_count "<scope,>plan" "spira-poison,<ask>" over the ready snapshot.
    pub fn plan_ready(&self, cfg: &Cfg) -> Option<usize> {
        let need = cfg.plan_labels();
        let excl: Vec<String> = ["spira-poison".to_string(), cfg.ask.clone()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        self.ready.as_ref().map(|r| {
            r.iter()
                .filter(|b| has_all(b, &need) && has_none(b, &excl))
                .count()
        })
    }

    /// Plan beads an aeon holds: lifecycle WORKING (was `bd list --status in_progress`).
    pub fn plan_inprog(&self, cfg: &Cfg) -> usize {
        let need = cfg.plan_labels();
        self.list
            .iter()
            .filter(|b| self.lc_state(&b.id).is_some_and(lc_state::is_working) && has_all(b, &need))
            .count()
    }

    fn per_partition<F: Fn(&Bead, &Partition) -> bool>(
        &self,
        parts: &[Partition],
        keep: F,
    ) -> Vec<(String, String)> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for p in parts {
            for b in &self.list {
                if keep(b, p)
                    && has_all(b, &p.labels)
                    && has_none(b, &p.exclude)
                    && seen.insert(b.id.clone())
                {
                    out.push((b.id.clone(), b.labels_csv()));
                }
            }
        }
        out
    }

    /// dispatchable_open: every non-epic/event bead a partition can reach whose lifecycle
    /// row still owes builder work.
    pub fn dispatchable(&self, parts: &[Partition]) -> Vec<(String, String)> {
        self.per_partition(parts, |b, _| {
            self.lc_open(&b.id) && !matches!(b.typ(), "epic" | "event")
        })
    }

    /// check4_closed_branched: beads in a partition the builder has handed on (lifecycle
    /// past WORKING) that carry a `branch:` label.
    pub fn closed_branched(&self, parts: &[Partition]) -> Vec<(String, String)> {
        self.per_partition(parts, |b, _| {
            self.lc_past_builder(&b.id)
                && !matches!(b.typ(), "epic" | "event")
                && b.labels.iter().any(|l| l.starts_with("branch:"))
        })
    }
}

/// The bulk reads, with their failure kept distinct from "empty".
pub struct Reads {
    pub list: Result<(String, Vec<Bead>), String>,
    pub ready: Result<(String, Vec<Bead>), String>,
}

pub fn read_json(bd: &Bd, h: &Host, args: &[String]) -> Result<(String, Vec<Bead>), String> {
    match bd.json(h, args) {
        Ok(s) => match parse_beads(&s) {
            Ok(v) => Ok((crate::model::json_only(&s).to_string(), v)),
            Err(e) => Err(e),
        },
        Err(o) => Err(format!(
            "rc={} {}",
            o.rc,
            o.stderr.lines().next().unwrap_or("")
        )),
    }
}

pub fn bulk(bd: &Bd, h: &Host, ready_args: &[String]) -> Reads {
    Reads {
        list: read_json(bd, h, &list_all_args()),
        ready: read_json(bd, h, ready_args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfg::{Cfg, Context};

    fn cfg(scope: &str) -> Cfg {
        let b = crate::cfg::tests::probe_bytes(
            &[
                ("SPIRA_RUN", "/r"),
                ("SPIRA_SCOPE_LABEL", scope),
                ("SPIRA_ASK_LABEL", "ask"),
                ("SPIRA_NO_LOOP_LABEL", "no-loop"), // literal-ok: test fixture
                ("SPIRA_DB", "/db"),
            ],
            &[],
            &[],
            &[],
            &[],
            None,
        );
        Cfg::from_context(&Context::parse(&b).unwrap(), std::path::Path::new("/h"))
    }

    fn snap() -> Snapshot {
        let list = r#"[
          {"id":"g","status":"open","issue_type":"epic"},
          {"id":"a","status":"open","parent":"g","labels":["spira","plan"]},
          {"id":"b","status":"closed","parent":"g","labels":["spira","plan","branch:spira/b","repo:other"],"issue_type":"task"},
          {"id":"c","status":"in_progress","labels":["spira","plan"],"issue_type":"task"},
          {"id":"d","status":"open","labels":["spira","plan","spira-poison"]},
          {"id":"e","status":"open","issue_type":"epic","labels":["spira","plan"]},
          {"id":"f","status":"closed","labels":["spira","plan"],"issue_type":"task","close_reason":"subsumed by x","dependencies":[{"depends_on_id":"q","type":"supersedes"}]},
          {"id":"i1","status":"open","labels":["incident","ref:abcd1234"]},
          {"id":"i2","status":"closed","labels":["incident","ref:ffff0000"]}
        ]"#;
        let ready = r#"[{"id":"a","labels":["spira","plan"]},{"id":"d","labels":["spira","plan","spira-poison"]},{"id":"z","labels":["plan"]}]"#;
        Snapshot::from_json(list, Some(ready)).with_lc(Some(&lc(&[
            ("a", "READY"),
            ("b", "SUBMITTED"),
            ("c", "WORKING"),
            ("d", "READY"),
            ("e", "READY"),
            ("f", "SUPERSEDED"),
        ])))
    }

    fn lc(rows: &[(&str, &str)]) -> Vec<LcRow> {
        rows.iter()
            .map(|(id, st)| LcRow { bead_id: id.to_string(), state: st.to_string(), ..Default::default() })
            .collect()
    }

    /// sp-mve9i: a work bead's state is its lifecycle row, never bd's `status` — bd says the
    /// opposite of the machine for every bead here, and every set follows the machine.
    #[test]
    fn every_set_reads_the_lifecycle_row_not_bd_status() {
        let list = r#"[
          {"id":"w","status":"closed","labels":["spira","plan","branch:spira/w"],"issue_type":"task"},
          {"id":"s","status":"open","labels":["spira","plan","branch:spira/s"],"issue_type":"task"},
          {"id":"h","status":"open","labels":["spira","plan"],"issue_type":"task"},
          {"id":"n","status":"open","labels":["spira","plan"],"issue_type":"task"}
        ]"#;
        let s = Snapshot::from_json(list, None).with_lc(Some(&lc(&[("w", "WORKING"), ("s", "SUBMITTED"), ("h", "LANDED")])));
        assert_eq!(s.plan_open(&cfg("spira")), vec!["w"], "WORKING is backlog; SUBMITTED/LANDED and a rowless bead are not");
        assert_eq!(s.plan_inprog(&cfg("spira")), 1, "WORKING, though bd says closed");
        let d: Vec<String> = s.dispatchable(&parts()).into_iter().map(|x| x.0).collect();
        assert_eq!(d, vec!["w"]);
        let cb: Vec<String> = s.closed_branched(&parts()).into_iter().map(|x| x.0).collect();
        assert_eq!(cb, vec!["s"], "handed on by the builder, though bd says open");
        let none = Snapshot::from_json(list, None);
        assert!(none.plan_open(&cfg("spira")).is_empty(), "no lifecycle read: no state, no backlog");
    }

    fn parts() -> Vec<Partition> {
        vec![
            Partition {
                labels: vec!["spira".into(), "plan".into()],
                exclude: vec!["spira-poison".into()],
            },
            Partition {
                labels: vec!["spira".into()],
                exclude: vec![],
            },
        ]
    }

    #[test]
    fn state_comes_from_the_snapshot() {
        let s = snap();
        assert_eq!(
            s.plan_open(&cfg("spira")),
            vec!["a", "c", "d"],
            "open and in-progress plan work, poisoned included; closed rows, epics and unlabelled rows are not the backlog"
        );
        assert!(
            s.plan_open(&cfg("other")).is_empty(),
            "another scope's plan beads are not this backlog"
        );
        assert_eq!(
            s.plan_ready(&cfg("spira")),
            Some(1),
            "poisoned and out-of-scope rows do not count"
        );
        assert_eq!(
            s.plan_ready(&cfg("")),
            Some(2),
            "no scope: the scope label is not required"
        );
        assert_eq!(s.plan_inprog(&cfg("spira")), 1);
        assert_eq!(
            Snapshot::from_json("[]", None).plan_ready(&cfg("spira")),
            None,
            "unknown is not zero"
        );
    }

    #[test]
    fn dispatchable_is_the_partition_predicate_deduplicated() {
        let s = snap();
        let d: Vec<String> = s.dispatchable(&parts()).into_iter().map(|x| x.0).collect();
        // partition 1 (excludes poison) gives a, c; partition 2 adds d and i-less rows with "spira"
        assert_eq!(d, vec!["a", "c", "d"]);
        let cb = s.closed_branched(&parts());
        assert_eq!(
            cb,
            vec![(
                "b".to_string(),
                "spira,plan,branch:spira/b,repo:other".to_string()
            )]
        );
    }

    #[test]
    fn ready_args_match_lib_sh() {
        let c = cfg("spira");
        assert_eq!(
            ready_args(&c).join(" "),
            "ready --limit 0 --exclude-type epic,event -u --label spira --exclude-label no-loop" // literal-ok: asserts the argv built from the fixture
        );
        assert_eq!(
            ready_raw_args(&c).join(" "),
            "ready --limit 0 --exclude-type epic,event -u --exclude-label no-loop" // literal-ok: asserts the argv built from the fixture
        );
    }
}
