//! `GET /api/factory`: the factory floor as one JSON object. Belts are the states of the bead,
//! batch, ask and mending machines and edges are those machines' own legal moves, so a state
//! added to a machine is a belt here with no change to loom. The tables below hold words only;
//! every number is read.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde_json::{json, Value};
use spira_config::lc_call::LcClient;

use crate::stuck::num;

pub const MACHINES: [&str; 4] = ["bead", "batch", "ask", "mending"];
const GANTT_MACHINES: [&str; 2] = ["bead", "batch"];
const DAY: i64 = 86_400;
/// How far back the retire station's feeders are looked for.
const SEEN_WINDOW: i64 = DAY;

/// The registered config keys a component's queue reads its configured limit from.
pub const LIMIT_KEYS: [&str; 11] = [
    "SPIRA_MAX_LIVE_DWARVES",
    "SPIRA_LANES_MAX_LIVE",
    "SPIRA_STACK_MAX_DEPTH",
    "SPIRA_COLD_PAR",
    "SPIRA_COMPILE_PAR",
    "SPIRA_TEST_PAR",
    "SPIRA_CERTIFY_PAR",
    "SPIRA_QUEUE_ROUNDS_MAX",
    "SPIRA_QUEUE_LOCK_WAIT",
    "SPIRA_ROUND_CERTIFY_WALL_SECS",
    "SPIRA_BATCHER_WALL_SECS",
];

struct StationDef {
    id: &'static str,
    lane: &'static str,
    label: &'static str,
    machine: &'static str,
    /// `machine.Event`, or `machine.Event@FROM` for the moves out of one state only.
    events: &'static [&'static str],
    components: &'static [&'static str],
    live: bool,
    creates: Option<&'static str>,
    back: &'static [&'static str],
    signals: &'static [(&'static str, &'static str)],
    /// Draw an input edge only from belts that sent this station an item in the last day.
    seen_only: bool,
}

const fn station(id: &'static str, lane: &'static str, label: &'static str, machine: &'static str, events: &'static [&'static str]) -> StationDef {
    StationDef { id, lane, label, machine, events, components: &[], live: true, creates: None, back: &[], signals: &[], seen_only: false }
}

const STATIONS: &[StationDef] = &[
    StationDef { creates: Some("bead.READY"), components: &["intent"], ..station("bead.file", "bead", "file", "bead.sh · intake.* · concierge", &[]) },
    station("bead.promote", "bead", "promote", "bead.sh", &["bead.Promote"]),
    StationDef { components: &["dwarf-session", "stack-proposal"], ..station("bead.claim", "bead", "claim", "dwarf → spira-lc CAS", &["bead.Claim"]) },
    StationDef {
        components: &["worktree", "build-dir", "compile", "model-session", "checks", "submit-proof"],
        signals: &[("ask.OPEN", "asks")],
        ..station("bead.build", "bead", "build + submit", "dwarf + work submit broker", &["bead.Phase", "bead.Renew", "bead.Disposition", "bead.Submit"])
    },
    StationDef { back: &["bead.READY", "bead.REWORK"], ..station("bead.unclaim", "bead", "unclaim / reap", "dwarf · sentinel-pass", &["bead.Release", "bead.HolderDead"]) },
    station("bead.finish", "bead", "persona finish", "lane dwarf", &["bead.Done"]),
    StationDef { components: &["gate-verdict"], back: &["bead.REWORK"], ..station("bead.gate", "bead", "tip gate", "gate.worker ← sentinel-pass", &["bead.GatePass", "bead.GateInfra"]) },
    StationDef { components: &["base-fetch"], ..station("bead.sift", "bead", "sift", "task:sift · 60 s", &["bead.Sifted"]) },
    StationDef { back: &["bead.REWORK"], ..station("bead.sendback", "bead", "send back", "queue eject --red · concierge", &["bead.GateRed"]) },
    StationDef {
        components: &["round-tree", "base", "queue-lock"],
        creates: Some("batch.OPEN"),
        ..station("bead.cut", "bead", "round open", "queue round open ← batcher.local", &["bead.Deliver", "batch.MemberAdded"])
    },
    StationDef {
        back: &["bead.SUBMITTED", "bead.CERTIFIED", "bead.REWORK"],
        ..station("bead.release", "bead", "release from round", "batch cascade · reconciler", &["bead.Requeued", "bead.Returned"])
    },
    StationDef {
        components: &["land-lock"],
        ..station("bead.land", "bead", "land", "queue round land ← batcher.local", &["bead.Delivered", "batch.FastForward@GREEN"])
    },
    StationDef {
        seen_only: true,
        ..station("bead.retire", "bead", "retire", "concierge · reconciler · reclassify", &["bead.Drop", "bead.Supersede", "bead.ContentOnBase"])
    },
    StationDef {
        components: &["round-vm", "pass-run"],
        signals: &[("mending.WAITING", "Failed")],
        ..station("batch.pass", "batch", "pass", "round-vm run ← CERTIFY leg", &["batch.CiStarted", "batch.PassStarted", "batch.SuitesStarted", "batch.Failed"])
    },
    StationDef {
        signals: &[("bead.release", "members")],
        ..station("batch.abandon", "batch", "abandon", "batcher-cut · reconciler", &["batch.Abandon"])
    },
    StationDef {
        components: &["pass-verdict"],
        ..station("batch.verdict", "batch", "verdict", "queue round pass-verdict", &["batch.PassGreen", "batch.PassRed", "batch.PassIncomplete", "batch.PassPreempted"])
    },
    StationDef {
        components: &["attribution"],
        back: &["batch.OPEN"],
        ..station("batch.eject", "batch", "eject / rebuild", "queue round eject ← EJECT", &["batch.Eject", "batch.PassRebuilt"])
    },
    station("ask.close", "side", "answer ask", "concierge · close-ask", &["ask.Answer", "ask.TakeDefault", "ask.Withdraw"]),
    station("mending.pickup", "side", "mend pickup", "mender dwarf", &["mending.Pickup"]),
    station(
        "mending.settle",
        "side",
        "mend settle",
        "queue round mended|return",
        &["mending.Mended", "mending.Environment", "mending.Flaky", "mending.Unsure", "mending.Return", "mending.Expire"],
    ),
    StationDef { live: false, creates: Some("batch.STAGED"), ..station("batch.stage", "dead", "stage / promote", "(no producer)", &["batch.Promote", "batch.Discard"]) },
    StationDef { live: false, ..station("batch.landStarted", "dead", "land start", "(no producer)", &["batch.LandStarted", "batch.LandAborted"]) },
    StationDef { live: false, ..station("batch.deploy", "dead", "deploy", "(no producer)", &["batch.FastForward@LANDING", "batch.Deployed", "batch.DeployFaulted"]) },
];

struct QueueDef {
    label: &'static str,
    key: Option<&'static str>,
}

const fn q(label: &'static str) -> QueueDef {
    QueueDef { label, key: None }
}

const fn qk(label: &'static str, key: &'static str) -> QueueDef {
    QueueDef { label, key: Some(key) }
}

struct ComponentDef {
    id: &'static str,
    label: &'static str,
    tag: Option<&'static str>,
    steps: &'static [&'static str],
    queues: &'static [QueueDef],
}

const fn component(id: &'static str, label: &'static str, tag: Option<&'static str>, steps: &'static [&'static str], queues: &'static [QueueDef]) -> ComponentDef {
    ComponentDef { id, label, tag, steps, queues }
}

const COMPONENTS: &[ComponentDef] = &[
    component("intent", "intent", None, &["concierge", "intake.gh", "intake.filings", "trigger.groom"], &[]),
    component(
        "dwarf-session",
        "dwarf session",
        Some("dwf"),
        &["sentinel-call", "unit.start", "dwarf start", "rune-ready → rank"],
        &[qk("fleet ceiling", "SPIRA_MAX_LIVE_DWARVES"), qk("lane cap", "SPIRA_LANES_MAX_LIVE"), q("call cadence")],
    ),
    component("stack-proposal", "stack proposal", Some("stk"), &["spira-claim stack"], &[qk("stack_max_depth", "SPIRA_STACK_MAX_DEPTH")]),
    component("worktree", "worktree + base", Some("wt"), &["fetch land ref", "worktree", "fixture + brief"], &[]),
    component(
        "build-dir",
        "build directory",
        Some("cold"),
        &["warm pick", "cold judge", "cold-build slot", "jitter"],
        &[qk("cold-build slot", "SPIRA_COLD_PAR"), q("warm slots")],
    ),
    component("compile", "compile admission", Some("cc"), &["round yield", "stagger", "compile lease", "sccache"], &[qk("compile_par", "SPIRA_COMPILE_PAR")]),
    component("model-session", "model session", Some("llm"), &["claude code, rune model"], &[]),
    component("checks", "checks verdict", Some("chk"), &["work checks (clippy, spira-lint)", "work review"], &[q("checks budget")]),
    component(
        "submit-proof",
        "submit proof",
        Some("prf"),
        &["broker refusals", "testenv lint", "owed", "proof tiers"],
        &[q("unit pool"), qk("test pool", "SPIRA_TEST_PAR")],
    ),
    component("gate-verdict", "gate verdict", Some("gate"), &["landing-pass enqueue", "gate.worker → gate.sh"], &[qk("certify_par", "SPIRA_CERTIFY_PAR")]),
    component("base-fetch", "base fetch", Some("base"), &["task:sift fetch"], &[]),
    component(
        "round-tree",
        "round tree",
        Some("tree"),
        &["observe", "select", "fetch base", "assemble"],
        &[qk("rounds per repo", "SPIRA_QUEUE_ROUNDS_MAX"), q("pool filter")],
    ),
    component("base", "base commit", Some("sha"), &["land ref"], &[]),
    component("queue-lock", "queue lock", Some("lck"), &["flock, non-blocking"], &[qk("queue_lock_wait", "SPIRA_QUEUE_LOCK_WAIT")]),
    component("land-lock", "land lock", Some("lck"), &["queue lock", "lock.activate"], &[q("activate lock")]),
    component("round-vm", "round VM", Some("vm"), &["EC2 acquire", "boot"], &[q("EC2 capacity")]),
    component(
        "pass-run",
        "pass run",
        Some("run"),
        &["release build", "lint + fences", "tiers", "suites", "salvage"],
        &[qk("round_certify_wall_secs", "SPIRA_ROUND_CERTIFY_WALL_SECS"), qk("batcher_wall_secs", "SPIRA_BATCHER_WALL_SECS")],
    ),
    component("pass-verdict", "pass verdict", Some("res"), &["suite results"], &[]),
    component("attribution", "attribution verdict", Some("attr"), &["batcher attribute on a further VM"], &[]),
    component("release-chain", "release machine", Some("rel"), &["build", "cut → accepted", "activate"], &[q("lock.activate")]),
    component("publish-pr", "publish PR", Some("pr"), &["queue.step", "publish", "queue verdict"], &[q("publish cadence")]),
];

pub type Read = Result<Value, String>;

/// Everything one page needs, read once. A failed read stays an `Err` so `build` can name it.
#[derive(Clone, Debug)]
pub struct Reads {
    pub graphs: BTreeMap<&'static str, Read>,
    pub gantts: BTreeMap<&'static str, Read>,
    pub live: Read,
    pub batches: Read,
    pub asks: Read,
    pub mending: Read,
    pub releases: Result<Vec<release::machine::Release>, String>,
    pub limits: BTreeMap<String, String>,
}

async fn read(client: &LcClient, args: &[&str], budget: Duration) -> Read {
    crate::lc_answer_json(client.clone(), args, budget).await
}

/// Every read the floor is built from, all at once.
pub async fn gather(client: LcClient, budget: Duration, run: &str) -> Reads {
    let c = &client;
    let release_dir = std::path::Path::new(run).join("release");
    let releases = async move { tokio::task::spawn_blocking(move || release::machine::all(&release_dir)).await.map_err(|e| format!("release records: {e}"))? };
    let (g_bead, g_batch, g_ask, g_mending, t_bead, t_batch, live, batches, asks, mending, releases) = tokio::join!(
        read(c, &["ops-graph", "--machine", "bead"], budget),
        read(c, &["ops-graph", "--machine", "batch"], budget),
        read(c, &["ops-graph", "--machine", "ask"], budget),
        read(c, &["ops-graph", "--machine", "mending"], budget),
        read(c, &["ops-gantt", "--machine", "bead"], budget),
        read(c, &["ops-gantt", "--machine", "batch"], budget),
        read(c, &["list", "--live"], budget),
        read(c, &["list", "--batches"], budget),
        read(c, &["list-asks"], budget),
        read(c, &["mending", "list", "--live"], budget),
        releases,
    );
    Reads {
        graphs: BTreeMap::from([("bead", g_bead), ("batch", g_batch), ("ask", g_ask), ("mending", g_mending)]),
        gantts: BTreeMap::from([("bead", t_bead), ("batch", t_batch)]),
        live,
        batches,
        asks,
        mending,
        releases,
        limits: BTreeMap::new(),
    }
}

struct Edge {
    machine: &'static str,
    from: String,
    event: String,
    to: String,
}

struct Move {
    machine: &'static str,
    event: String,
    from: String,
    to: String,
    at: i64,
}

struct EventRef<'a> {
    machine: &'a str,
    event: &'a str,
    from: Option<&'a str>,
}

fn event_ref(spec: &str) -> EventRef<'_> {
    let (machine, rest) = spec.split_once('.').unwrap_or(("", spec));
    let (event, from) = match rest.split_once('@') {
        Some((e, f)) => (e, Some(f)),
        None => (rest, None),
    };
    EventRef { machine, event, from }
}

impl EventRef<'_> {
    fn takes(&self, machine: &str, event: &str, from: &str) -> bool {
        self.machine == machine && self.event == event && self.from.is_none_or(|f| f == from)
    }
}

struct Item {
    id: String,
    at: String,
    kind: &'static str,
    p: Option<i64>,
    express: bool,
    since: Option<i64>,
    holds: Vec<String>,
    blocked: Option<String>,
    phase: Option<String>,
    repo: Option<String>,
    title: Option<String>,
    verdict: Option<String>,
}

impl Item {
    fn parked(&self) -> bool {
        !self.holds.is_empty() || self.blocked.is_some()
    }

    fn json(&self) -> Value {
        json!({"id": self.id, "at": self.at, "kind": self.kind, "p": self.p, "express": self.express, "since": self.since,
               "holds": self.holds, "blocked": self.blocked, "phase": self.phase, "repo": self.repo, "title": self.title})
    }
}

fn text(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

fn rows(v: &Value) -> Vec<&Value> {
    v.as_array().map(|a| a.iter().collect()).unwrap_or_default()
}

fn holds_of(v: &Value) -> Vec<String> {
    let list: Vec<Value> = match v {
        Value::Array(a) => a.clone(),
        Value::String(s) => serde_json::from_str(s).unwrap_or_default(),
        _ => Vec::new(),
    };
    list.iter().filter_map(text).collect()
}

fn bead_items(v: &Value) -> Vec<Item> {
    rows(v)
        .into_iter()
        .filter_map(|r| {
            let state = text(&r["state"])?;
            Some(Item {
                id: text(&r["bead_id"])?,
                at: format!("bead.{state}"),
                kind: "bead",
                p: num(&r["priority"]),
                express: num(&r["express"]).is_some_and(|e| e != 0) || r["express"].as_bool() == Some(true),
                since: num(&r["since"]),
                holds: holds_of(&r["holds"]),
                blocked: r["blocked_by"].as_array().and_then(|b| b.iter().find_map(text)),
                phase: text(&r["dwarf_phase"]),
                repo: text(&r["repo"]),
                title: text(&r["title"]),
                verdict: text(&r["claim_verdict"]),
            })
        })
        .collect()
}

fn side_item(id: String, machine: &str, state: &str, kind: &'static str, since: Option<i64>, repo: Option<String>, title: Option<String>) -> Item {
    Item { id, at: format!("{machine}.{state}"), kind, p: None, express: false, since, holds: Vec::new(), blocked: None, phase: None, repo, title, verdict: None }
}

fn batch_items(v: &Value) -> Vec<Item> {
    rows(v)
        .into_iter()
        .filter_map(|r| {
            let members = r["members"].as_array().map_or(0, Vec::len);
            let repo = text(&r["repo"]);
            let title = format!("{} round · pass {} · {members} members", repo.as_deref().unwrap_or("?"), num(&r["pass"]).unwrap_or(0));
            Some(side_item(text(&r["batch_id"])?, "batch", &text(&r["state"])?, "batch", num(&r["phase_since"]).or_else(|| num(&r["opened_at"])), repo, Some(title)))
        })
        .collect()
}

fn ask_items(v: &Value) -> Vec<Item> {
    rows(v)
        .into_iter()
        .filter_map(|r| {
            let title = text(&r["title"]).or_else(|| text(&r["work_bead"]));
            Some(side_item(text(&r["ask_id"])?, "ask", &text(&r["state"])?, "ask", num(&r["opened_at"]), None, title))
        })
        .collect()
}

fn mending_items(v: &Value) -> Vec<Item> {
    rows(v)
        .into_iter()
        .filter_map(|r| {
            let (batch, suite) = (text(&r["batch_id"])?, text(&r["suite"])?);
            let id = format!("{batch}#{}#{suite}", num(&r["pass"]).unwrap_or(0));
            Some(side_item(id, "mending", &text(&r["state"])?, "mending", num(&r["failed_at"]), None, Some(suite)))
        })
        .collect()
}

/// Claim order: express, then REWORK, then priority (unknown last), then oldest first; parked last.
fn claim_order(items: &mut [Item]) {
    items.sort_by(|a, b| {
        let key = |i: &Item| (i.parked(), !i.express, i.at != "bead.REWORK", i.p.unwrap_or(i64::MAX), i.since.unwrap_or(i64::MAX));
        key(a).cmp(&key(b)).then_with(|| a.id.cmp(&b.id))
    });
}

fn moves(machine: &'static str, v: &Value) -> Vec<Move> {
    rows(&v["events"])
        .into_iter()
        .filter_map(|r| Some(Move { machine, event: text(&r["event"])?, from: text(&r["from_state"])?, to: text(&r["to_state"])?, at: num(&r["at"])? }))
        .collect()
}

fn label_of(machine: &str, state: &str) -> String {
    match machine {
        "ask" => format!("ASK {state}"),
        "mending" => format!("MEND {state}"),
        _ => state.to_string(),
    }
}

fn lane_of(machine: &str) -> &'static str {
    match machine {
        "bead" => "bead",
        "batch" => "batch",
        _ => "side",
    }
}

struct Built {
    id: String,
    lane: String,
    label: String,
    machine: String,
    events: Vec<String>,
    components: Vec<String>,
    live: bool,
    creates: Option<String>,
    back: Vec<String>,
    signals: Vec<(String, String)>,
    seen_only: bool,
    legal: Vec<usize>,
}

fn named(def: &StationDef) -> Built {
    Built {
        id: def.id.into(),
        lane: def.lane.into(),
        label: def.label.into(),
        machine: def.machine.into(),
        events: def.events.iter().map(|e| e.to_string()).collect(),
        components: def.components.iter().map(|c| c.to_string()).collect(),
        live: def.live,
        creates: def.creates.map(str::to_string),
        back: def.back.iter().map(|b| b.to_string()).collect(),
        signals: def.signals.iter().map(|(t, l)| (t.to_string(), l.to_string())).collect(),
        seen_only: def.seen_only,
        legal: Vec::new(),
    }
}

fn count<'a>(rows: impl Iterator<Item = &'a Move>) -> i64 {
    rows.count() as i64
}

/// Builds the floor from `reads` as of `now`. A failed `ops-graph` is the one read the floor
/// cannot be drawn without; every other failed read is named in `meta.errors` and its numbers are null.
pub fn build(reads: &Reads, now: i64) -> Result<Value, String> {
    let mut errors: Vec<String> = Vec::new();

    let mut edges: Vec<Edge> = Vec::new();
    for machine in MACHINES {
        let graph = reads.graphs.get(machine).ok_or_else(|| format!("ops-graph --machine {machine}: not read"))?;
        let graph = graph.as_ref().map_err(|e| format!("ops-graph --machine {machine}: {e}"))?;
        for e in rows(graph) {
            if let (Some(from), Some(event), Some(to)) = (text(&e["from"]), text(&e["event"]), text(&e["to"])) {
                edges.push(Edge { machine, from, event, to });
            }
        }
    }

    fn note(errors: &mut Vec<String>, what: &str, r: &Read) {
        if let Err(e) = r {
            errors.push(format!("{what}: {e}"));
        }
    }
    note(&mut errors, "list --live", &reads.live);
    note(&mut errors, "list --batches", &reads.batches);
    note(&mut errors, "list-asks", &reads.asks);
    note(&mut errors, "mending list --live", &reads.mending);
    for m in GANTT_MACHINES {
        match reads.gantts.get(m) {
            Some(r) => note(&mut errors, &format!("ops-gantt --machine {m}"), r),
            None => errors.push(format!("ops-gantt --machine {m}: not read")),
        }
    }
    if let Err(e) = &reads.releases {
        errors.push(format!("release records: {e}"));
    }

    let gantt_ok = |m: &str| reads.gantts.get(m).is_some_and(Result::is_ok);
    let mut all_moves: Vec<Move> = Vec::new();
    for m in GANTT_MACHINES {
        if let Some(Ok(v)) = reads.gantts.get(m) {
            all_moves.extend(moves(m, v));
        }
    }

    // Stations: the table, then one for every event no station names.
    let mut stations: Vec<Built> = STATIONS.iter().map(named).collect();
    let named_events: BTreeSet<String> = STATIONS.iter().flat_map(|s| s.events.iter().map(|e| format!("{}.{}", event_ref(e).machine, event_ref(e).event))).collect();
    let mut derived: BTreeSet<String> = BTreeSet::new();
    for e in &edges {
        let qualified = format!("{}.{}", e.machine, e.event);
        if !named_events.contains(&qualified) && derived.insert(qualified.clone()) {
            stations.push(Built {
                id: qualified.clone(),
                lane: lane_of(e.machine).into(),
                label: e.event.clone(),
                machine: format!("{} machine", e.machine),
                events: vec![qualified],
                components: Vec::new(),
                live: true,
                creates: None,
                back: Vec::new(),
                signals: Vec::new(),
                seen_only: false,
                legal: Vec::new(),
            });
        }
    }
    for st in &mut stations {
        let refs: Vec<EventRef> = st.events.iter().map(|e| event_ref(e)).collect();
        st.legal = edges.iter().enumerate().filter(|(_, e)| refs.iter().any(|r| r.takes(e.machine, &e.event, &e.from))).map(|(i, _)| i).collect();
    }

    let belt_id = |machine: &str, state: &str| format!("{machine}.{state}");
    let mut states: Vec<(&'static str, String)> = Vec::new();
    for e in &edges {
        for s in [&e.from, &e.to] {
            if !states.iter().any(|(m, x)| *m == e.machine && x == s) {
                states.push((e.machine, s.clone()));
            }
        }
    }
    let belt_exists = |id: &str| states.iter().any(|(m, s)| belt_id(m, s) == id);

    let ins = |st: &Built| -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for &i in &st.legal {
            let id = belt_id(edges[i].machine, &edges[i].from);
            if !v.contains(&id) {
                v.push(id);
            }
        }
        v
    };
    let outs = |st: &Built| -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for &i in &st.legal {
            let id = belt_id(edges[i].machine, &edges[i].to);
            if !v.contains(&id) {
                v.push(id);
            }
        }
        if let Some(c) = &st.creates {
            if !v.contains(c) {
                v.push(c.clone());
            }
        }
        v
    };

    // Belts: sources have nothing into them, sinks nothing out, unreachable ones only dead stations in.
    let mut belts: Vec<(String, &'static str, String, String, &'static str)> = Vec::new();
    let mut belts_into_live: BTreeSet<String> = BTreeSet::new();
    let mut belts_into_any: BTreeSet<String> = BTreeSet::new();
    for st in &stations {
        for o in outs(st) {
            belts_into_any.insert(o.clone());
            if st.live {
                belts_into_live.insert(o);
            }
        }
    }
    let with_exit: BTreeSet<String> = edges.iter().map(|e| belt_id(e.machine, &e.from)).collect();
    for (machine, state) in &states {
        let id = belt_id(machine, state);
        let kind = if belts_into_any.contains(&id) && !belts_into_live.contains(&id) {
            "unreachable"
        } else if !belts_into_any.contains(&id) {
            "source"
        } else if !with_exit.contains(&id) {
            "sink"
        } else {
            "belt"
        };
        let lane = if kind == "unreachable" { "dead" } else { lane_of(machine) };
        belts.push((id, lane, label_of(machine, state), state.clone(), kind));
    }
    let kind_of = |id: &str| belts.iter().find(|b| b.0 == id).map(|b| b.4);

    // Items, in claim order.
    let mut items: Vec<Item> = Vec::new();
    if let Ok(v) = &reads.live {
        items.extend(bead_items(v));
    }
    if let Ok(v) = &reads.batches {
        items.extend(batch_items(v));
    }
    if let Ok(v) = &reads.asks {
        items.extend(ask_items(v));
    }
    if let Ok(v) = &reads.mending {
        items.extend(mending_items(v));
    }
    items.retain(|i| kind_of(&i.at) != Some("sink"));
    claim_order(&mut items);
    let machine_read_ok = |m: &str| match m {
        "bead" => reads.live.is_ok(),
        "batch" => reads.batches.is_ok(),
        "ask" => reads.asks.is_ok(),
        _ => reads.mending.is_ok(),
    };

    // Belts as JSON.
    let in24h_releases = reads.releases.as_ref().ok().map(|rs| rs.iter().flat_map(|r| r.events.iter()).filter(|e| e.to == release::machine::ReleaseState::Activated && e.at as i64 >= now - DAY).count() as i64);
    let mut belts_json: Vec<Value> = Vec::new();
    for (id, lane, label, state, kind) in &belts {
        let machine = id.split_once('.').map_or("", |(m, _)| m);
        let here: Vec<&Item> = items.iter().filter(|i| &i.at == id).collect();
        let counted = *kind != "sink" && machine_read_ok(machine);
        let in24h = (*kind == "sink" && gantt_ok(machine)).then(|| count(all_moves.iter().filter(|m| m.machine == machine && &m.to == state)));
        let parts = (id == "bead.READY" && counted).then(|| {
            let claimable_known = here.iter().any(|i| i.verdict.is_some());
            let held = here.iter().filter(|i| !i.holds.is_empty()).count();
            let blocked = here.iter().filter(|i| i.holds.is_empty() && (i.blocked.is_some() || i.verdict.as_deref() == Some("blocked"))).count();
            let claimable = here.iter().filter(|i| i.holds.is_empty() && i.blocked.is_none() && i.verdict.as_deref() == Some("claimable")).count();
            json!({"claimable": claimable_known.then_some(claimable), "held": held, "blocked": blocked})
        });
        belts_json.push(json!({
            "id": id, "lane": lane, "kind": kind, "label": label, "machine": machine, "state": state,
            "now": counted.then_some(here.len()),
            "oldest_s": counted.then(|| here.iter().filter_map(|i| i.since).min().map(|s| now - s)).flatten(),
            "in24h": in24h, "parts": parts,
        }));
    }
    if belt_exists("batch.LANDED") {
        belts_json.push(json!({"id": "rel.ACTIVATED", "lane": "batch", "kind": "offmachine", "label": "RELEASE LIVE", "machine": "release", "state": "ACTIVATED",
            "now": null, "oldest_s": null, "in24h": in24h_releases, "parts": null}));
        belts_json.push(json!({"id": "pub.MERGED", "lane": "batch", "kind": "offmachine", "label": "ON ORIGIN", "machine": "publish", "state": "MERGED",
            "now": null, "oldest_s": null, "in24h": null, "parts": null}));
    }

    // Stations and edges.
    let tally = |refs: &[EventRef], pick: &dyn Fn(&Move) -> bool| -> Option<i64> {
        refs.iter().any(|r| gantt_ok(r.machine)).then(|| count(all_moves.iter().filter(|m| refs.iter().any(|r| r.takes(m.machine, &m.event, &m.from)) && pick(m))))
    };
    let mut stations_json: Vec<Value> = Vec::new();
    let mut edges_json: Vec<Value> = Vec::new();
    let seen_cutoff = now - SEEN_WINDOW;
    for st in &stations {
        let refs: Vec<EventRef> = st.events.iter().map(|e| event_ref(e)).collect();
        let mut inputs = ins(st);
        if st.seen_only && refs.iter().all(|r| gantt_ok(r.machine)) {
            inputs.retain(|b| {
                let (m, s) = b.split_once('.').unwrap_or(("", ""));
                all_moves.iter().any(|mv| mv.machine == m && mv.from == s && mv.at >= seen_cutoff && refs.iter().any(|r| r.takes(mv.machine, &mv.event, &mv.from)))
            });
        }
        let outputs = outs(st);
        let matched: Vec<&Move> = all_moves.iter().filter(|m| refs.iter().any(|r| r.takes(m.machine, &m.event, &m.from))).collect();
        let readable = !refs.is_empty() && refs.iter().any(|r| gantt_ok(r.machine));
        let per24h = readable.then_some(matched.len() as i64);
        let last_s = readable.then(|| matched.iter().map(|m| m.at).max().map(|at| now - at)).flatten();

        let flow_inputs_readable = !inputs.is_empty() && inputs.iter().all(|b| machine_read_ok(b.split_once('.').map_or("", |(m, _)| m)));
        let wait_s = flow_inputs_readable.then(|| {
            let oldest = items.iter().filter(|i| !i.parked() && inputs.contains(&i.at)).filter_map(|i| i.since).min();
            oldest.map_or(0, |s| now - s)
        });

        stations_json.push(json!({
            "id": st.id, "lane": st.lane, "label": st.label, "machine": st.machine, "events": st.events,
            "per24h": per24h, "wait_s": wait_s, "last_s": last_s, "live": st.live, "components": st.components,
            "creates": st.creates, "inputs": inputs, "outputs": outputs,
        }));

        let live_kind = |k: &'static str| if st.live { k } else { "dead" };
        for b in &inputs {
            let (m, s) = b.split_once('.').unwrap_or(("", ""));
            let n = tally(&refs, &|mv| mv.machine == m && mv.from == s);
            edges_json.push(json!({"from": b, "to": st.id, "n": n, "kind": live_kind("flow"), "label": null}));
        }
        for o in &outputs {
            let (m, s) = o.split_once('.').unwrap_or(("", ""));
            let created = st.creates.as_deref() == Some(o.as_str()) && !st.legal.iter().any(|&i| belt_id(edges[i].machine, &edges[i].to) == *o);
            let n = if created { None } else { tally(&refs, &|mv| mv.machine == m && mv.to == s) };
            let kind = if st.back.contains(o) { live_kind("back") } else { live_kind("flow") };
            edges_json.push(json!({"from": st.id, "to": o, "n": n, "kind": kind, "label": null}));
        }
        for (to, label) in &st.signals {
            edges_json.push(json!({"from": st.id, "to": to, "n": null, "kind": live_kind("signal"), "label": label}));
        }
    }
    if belt_exists("batch.LANDED") {
        edges_json.push(json!({"from": "batch.LANDED", "to": "rel.ACTIVATED", "n": in24h_releases, "kind": "flow", "label": "release"}));
        edges_json.push(json!({"from": "batch.LANDED", "to": "pub.MERGED", "n": null, "kind": "flow", "label": "publish PR"}));
    }

    let lanes = json!([
        {"id": "bead", "label": "Bead line"},
        {"id": "batch", "label": "Batch line"},
        {"id": "side", "label": "Side loops"},
        {"id": "dead", "label": "Designed, never built"},
    ]);
    let components: Vec<Value> = COMPONENTS
        .iter()
        .map(|c| {
            let queues: Vec<Value> = c
                .queues
                .iter()
                .map(|qd| {
                    let configured = qd.key.and_then(|k| reads.limits.get(k)).map(|v| v.parse::<i64>().map_or_else(|_| json!(v), |n| json!(n)));
                    json!({"label": qd.label, "key": qd.key, "config": configured})
                })
                .collect();
            json!({"id": c.id, "label": c.label, "tag": c.tag, "steps": c.steps, "queues": queues})
        })
        .collect();

    Ok(json!({
        "meta": {"now": now, "window_s": DAY, "errors": errors},
        "lanes": lanes,
        "belts": belts_json,
        "stations": stations_json,
        "edges": edges_json,
        "items": items.iter().map(Item::json).collect::<Vec<_>>(),
        "components": components,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_config::lc_call::{Reply, Transport};
    use std::sync::Arc;

    const NOW: i64 = 1_800_000_000;

    struct Answers(Vec<(String, i32, String)>);

    impl Transport for Answers {
        fn exchange(&self, argv: &[String], _: Option<&str>, _: Duration) -> Result<Reply, String> {
            let key = argv.join(" ");
            self.0.iter().find(|(k, _, _)| *k == key).map(|(_, code, out)| Reply { code: *code, out: out.clone(), err: String::new() }).ok_or_else(|| format!("unscripted {key}"))
        }
    }

    fn graph(edges: &[(&str, &str, &str)]) -> String {
        Value::Array(edges.iter().map(|(f, e, t)| json!({"from": f, "event": e, "to": t})).collect()).to_string()
    }

    fn bead_graph() -> String {
        graph(&[
            ("OPEN", "Promote", "READY"),
            ("READY", "Claim", "WORKING"),
            ("REWORK", "Claim", "WORKING"),
            ("WORKING", "Release", "READY"),
            ("WORKING", "HolderDead", "READY"),
            ("WORKING", "Submit", "SUBMITTED"),
            ("SUBMITTED", "Deliver", "IN_DELIVERY"),
            ("CERTIFIED", "Deliver", "IN_DELIVERY"),
            ("SUBMITTED", "GatePass", "CERTIFIED"),
            ("IN_DELIVERY", "Delivered", "LANDED"),
            ("IN_DELIVERY", "Drop", "DROPPED"),
            ("READY", "Drop", "DROPPED"),
            ("SUBMITTED", "Drop", "DROPPED"),
        ])
    }

    fn batch_graph() -> String {
        graph(&[
            ("OPEN", "CiStarted", "CI_RUNNING"),
            ("OPEN", "Abandon", "ABANDONED"),
            ("CI_RUNNING", "PassGreen", "GREEN"),
            ("GREEN", "FastForward", "LANDED"),
            ("GREEN", "LandStarted", "LANDING"),
            ("LANDING", "FastForward", "DEPLOYING"),
            ("DEPLOYING", "Deployed", "LANDED"),
        ])
    }

    fn ok(args: &str, out: String) -> (String, i32, String) {
        (args.to_string(), 0, out)
    }

    fn script(batch_graph: String, gantt_bead: &Value, live: Value) -> Vec<(String, i32, String)> {
        vec![
            ok("ops-graph --machine bead", bead_graph()),
            ok("ops-graph --machine batch", batch_graph),
            ok("ops-graph --machine ask", graph(&[("OPEN", "Answer", "ANSWERED")])),
            ok("ops-graph --machine mending", graph(&[("WAITING", "Pickup", "MENDING")])),
            ok("ops-gantt --machine bead", gantt_bead.to_string()),
            ok("ops-gantt --machine batch", json!({"now": NOW, "events": []}).to_string()),
            ok("list --live", live.to_string()),
            ok("list --batches", "[]".into()),
            ok("list-asks", "[]".into()),
            ok("mending list --live", "[]".into()),
        ]
    }

    fn bead(id: &str, state: &str, p: Option<i64>, since: i64) -> Value {
        json!({"bead_id": id, "state": state, "priority": p.map(|p| p.to_string()), "since": since.to_string(), "express": "0", "holds": "[]", "blocked_by": [], "repo": "spira", "title": id, "claim_verdict": null})
    }

    async fn floor(answers: Vec<(String, i32, String)>) -> Result<Value, String> {
        let client = LcClient::over(Arc::new(Answers(answers)), true);
        let reads = gather(client, Duration::from_secs(2), "/nonexistent-run").await;
        build(&reads, NOW)
    }

    fn quiet() -> Value {
        json!({"now": NOW, "events": []})
    }

    fn by_id<'a>(v: &'a Value, list: &str, id: &str) -> &'a Value {
        v[list].as_array().unwrap().iter().find(|x| x["id"] == id).unwrap_or_else(|| panic!("no {id} in {list}"))
    }

    fn ids(v: &Value) -> Vec<&str> {
        v.as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect()
    }

    #[tokio::test]
    async fn a_state_added_to_a_machine_appears_as_a_belt_with_no_change_to_loom() {
        let grown = graph(&[("OPEN", "CiStarted", "CI_RUNNING"), ("CI_RUNNING", "Quarantine", "QUARANTINED")]);
        let f = floor(script(grown, &quiet(), json!([]))).await.unwrap();
        let belt = by_id(&f, "belts", "batch.QUARANTINED");
        assert_eq!((belt["kind"].as_str(), belt["lane"].as_str()), (Some("sink"), Some("batch")));
        assert!(f["stations"].as_array().unwrap().iter().any(|s| s["id"] == "batch.Quarantine"), "an event no station names gets its own");
    }

    #[tokio::test]
    async fn round_open_takes_submitted_and_certified_and_fills_in_delivery_and_creates_a_batch() {
        let f = floor(script(batch_graph(), &quiet(), json!([]))).await.unwrap();
        let st = by_id(&f, "stations", "bead.cut");
        assert_eq!(st["label"], "round open");
        assert_eq!(ids(&st["inputs"]), ["bead.SUBMITTED", "bead.CERTIFIED"]);
        assert_eq!(ids(&st["outputs"]), ["bead.IN_DELIVERY", "batch.OPEN"]);
        assert_eq!(st["creates"], "batch.OPEN");
        let has = |from: &str, to: &str| f["edges"].as_array().unwrap().iter().any(|e| e["from"] == from && e["to"] == to);
        assert!(has("bead.cut", "batch.OPEN") && has("bead.cut", "bead.IN_DELIVERY") && has("bead.SUBMITTED", "bead.cut"));
    }

    #[tokio::test]
    async fn belts_are_sources_sinks_or_unreachable_by_the_stations_that_feed_them() {
        let f = floor(script(batch_graph(), &quiet(), json!([]))).await.unwrap();
        let kind = |id: &str| by_id(&f, "belts", id)["kind"].as_str().unwrap().to_string();
        assert_eq!(kind("bead.OPEN"), "source");
        assert_eq!(kind("bead.LANDED"), "sink");
        assert_eq!(kind("batch.LANDING"), "unreachable");
        assert_eq!(by_id(&f, "belts", "batch.LANDING")["lane"], "dead");
        assert_eq!(kind("batch.OPEN"), "belt", "round open creates it");
        assert_eq!(kind("batch.LANDED"), "sink", "the exits hang off it, off the machine");
        assert_eq!((kind("rel.ACTIVATED"), kind("pub.MERGED")), ("offmachine".into(), "offmachine".into()));
    }

    #[tokio::test]
    async fn claim_order_is_express_then_rework_then_priority_then_oldest_with_parked_last() {
        let mut held = bead("held-p0", "READY", Some(0), 100);
        held["holds"] = json!("[\"manual\"]");
        let mut express = bead("express-p2", "READY", Some(2), 900);
        express["express"] = json!("1");
        let live = json!([
            held,
            bead("p1-new", "READY", Some(1), 500),
            bead("p0", "READY", Some(0), 400),
            bead("p1-old", "READY", Some(1), 200),
            bead("rework-p1", "REWORK", Some(1), 800),
            express,
            bead("no-p", "READY", None, 100),
        ]);
        let f = floor(script(batch_graph(), &quiet(), live)).await.unwrap();
        let order: Vec<&str> = f["items"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap()).collect();
        assert_eq!(order, ["express-p2", "rework-p1", "p0", "p1-old", "p1-new", "no-p", "held-p0"]);
    }

    #[tokio::test]
    async fn a_stations_wait_is_the_age_of_the_oldest_unparked_item_on_its_inputs() {
        let mut held = bead("held-old", "SUBMITTED", Some(1), NOW - 9000);
        held["holds"] = json!("[\"manual\"]");
        let live = json!([held, bead("newer", "SUBMITTED", Some(1), NOW - 120)]);
        let f = floor(script(batch_graph(), &quiet(), live)).await.unwrap();
        assert_eq!(by_id(&f, "stations", "bead.cut")["wait_s"], 120);
        assert_eq!(by_id(&f, "belts", "bead.SUBMITTED")["oldest_s"], 9000, "the belt's oldest counts parked items");
    }

    #[tokio::test]
    async fn unclaim_counts_release_and_holder_dead_rows_together() {
        let row = |seq: i64, event: &str, at: i64| json!({"seq": seq, "bead_id": "sp-a", "event": event, "from_state": "WORKING", "to_state": "READY", "at": at});
        let gantt = json!({"now": NOW, "events": [row(1, "Release", NOW - 60), row(2, "Release", NOW - 600), row(3, "HolderDead", NOW - 6000), {"seq": 4, "bead_id": "sp-a", "event": "Submit", "from_state": "WORKING", "to_state": "SUBMITTED", "at": NOW - 5}]});
        let f = floor(script(batch_graph(), &gantt, json!([]))).await.unwrap();
        let st = by_id(&f, "stations", "bead.unclaim");
        assert_eq!((st["per24h"].as_i64(), st["last_s"].as_i64()), (Some(3), Some(60)));
        let edge = f["edges"].as_array().unwrap().iter().find(|e| e["from"] == "bead.WORKING" && e["to"] == "bead.unclaim").unwrap();
        assert_eq!(edge["n"], 3);
        assert_eq!(by_id(&f, "stations", "bead.build")["per24h"], 1);
    }

    #[tokio::test]
    async fn retire_draws_an_input_only_from_belts_that_sent_it_an_item_in_the_last_day() {
        let row = json!({"seq": 1, "bead_id": "sp-a", "event": "Drop", "from_state": "READY", "to_state": "DROPPED", "at": NOW - 100});
        let f = floor(script(batch_graph(), &json!({"now": NOW, "events": [row]}), json!([]))).await.unwrap();
        assert_eq!(ids(&by_id(&f, "stations", "bead.retire")["inputs"]), ["bead.READY"]);
        let quiet_floor = floor(script(batch_graph(), &quiet(), json!([]))).await.unwrap();
        assert!(by_id(&quiet_floor, "stations", "bead.retire")["inputs"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_refused_list_live_is_named_and_its_numbers_are_null_not_zero() {
        let mut answers = script(batch_graph(), &quiet(), json!([]));
        for a in answers.iter_mut().filter(|a| a.0 == "list --live") {
            *a = ("list --live".into(), 3, "refused".into());
        }
        let f = floor(answers).await.unwrap();
        let errors = f["meta"]["errors"].as_array().unwrap();
        assert!(errors.iter().any(|e| e.as_str().unwrap().starts_with("list --live")), "{errors:?}");
        let ready = by_id(&f, "belts", "bead.READY");
        assert!(ready["now"].is_null() && ready["oldest_s"].is_null() && ready["parts"].is_null());
        assert!(by_id(&f, "stations", "bead.cut")["wait_s"].is_null());
        assert_eq!(by_id(&f, "belts", "batch.OPEN")["now"], 0, "a read that answered stays a number");
    }

    #[tokio::test]
    async fn a_failed_ops_graph_is_no_floor_at_all() {
        let mut answers = script(batch_graph(), &quiet(), json!([]));
        for a in answers.iter_mut().filter(|a| a.0 == "ops-graph --machine ask") {
            a.1 = 2;
        }
        let e = floor(answers).await.unwrap_err();
        assert!(e.contains("ops-graph --machine ask"), "{e}");
    }

    #[tokio::test]
    async fn ready_parts_come_from_the_claim_verdict_the_holds_and_the_blockers() {
        let mut held = bead("held", "READY", Some(1), 1);
        held["holds"] = json!("[\"manual\"]");
        let mut blocked = bead("blocked", "READY", Some(1), 2);
        blocked["blocked_by"] = json!(["sp-x"]);
        let mut free = bead("free", "READY", Some(1), 3);
        free["claim_verdict"] = json!("claimable");
        let f = floor(script(batch_graph(), &quiet(), json!([held, blocked, free]))).await.unwrap();
        assert_eq!(by_id(&f, "belts", "bead.READY")["parts"], json!({"claimable": 1, "held": 1, "blocked": 1}));
        assert_eq!(by_id(&f, "belts", "bead.READY")["now"], 3);
    }

    #[test]
    fn every_event_the_table_names_is_one_its_machine_names() {
        let names: BTreeMap<&str, Vec<&str>> = BTreeMap::from([
            ("bead", lifecycle::bead::event_names()),
            ("batch", lifecycle::batch::event_names()),
            ("ask", lifecycle::ask::event_names()),
            ("mending", lifecycle::mending::event_names()),
        ]);
        for st in STATIONS {
            for spec in st.events {
                let r = event_ref(spec);
                assert!(names.get(r.machine).is_some_and(|n| n.contains(&r.event)), "{}: {spec} is no event of its machine", st.id);
            }
        }
        let belts: BTreeSet<&str> = STATIONS.iter().flat_map(|s| s.creates.iter().chain(s.back).copied().chain(s.signals.iter().map(|(t, _)| *t))).collect();
        let stations: BTreeSet<&str> = STATIONS.iter().map(|s| s.id).collect();
        for target in belts {
            let (m, s) = target.split_once('.').unwrap();
            let is_belt = MACHINES.contains(&m) && s.chars().all(|c| c.is_ascii_uppercase() || c == '_');
            assert!(is_belt || stations.contains(target), "{target} is neither a belt nor a station");
        }
    }

    #[test]
    fn every_limit_key_is_a_registered_config_key() {
        let conf_d = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/conf.d");
        for key in LIMIT_KEYS {
            assert!(conf_d.join(key).is_file(), "{key} is not registered in spira/conf.d");
        }
    }

    #[tokio::test]
    async fn an_event_qualified_by_its_from_state_takes_only_the_moves_out_of_that_state() {
        let f = floor(script(batch_graph(), &quiet(), json!([]))).await.unwrap();
        let land = by_id(&f, "stations", "bead.land");
        assert_eq!(ids(&land["inputs"]), ["bead.IN_DELIVERY", "batch.GREEN"]);
        assert_eq!(ids(&land["outputs"]), ["bead.LANDED", "batch.LANDED"]);
        assert_eq!(ids(&by_id(&f, "stations", "batch.deploy")["inputs"]), ["batch.LANDING", "batch.DEPLOYING"]);
    }

    #[test]
    fn every_component_a_station_names_exists_and_every_limit_key_is_read() {
        let ids: BTreeSet<&str> = COMPONENTS.iter().map(|c| c.id).collect();
        for st in STATIONS {
            for c in st.components {
                assert!(ids.contains(c), "{}: no component {c}", st.id);
            }
        }
        let used: BTreeSet<&str> = COMPONENTS.iter().flat_map(|c| c.queues.iter().filter_map(|qd| qd.key)).collect();
        assert_eq!(used, LIMIT_KEYS.iter().copied().collect::<BTreeSet<_>>());
    }

    #[tokio::test]
    async fn a_queue_reads_the_limit_its_registered_key_carries() {
        let client = LcClient::over(Arc::new(Answers(script(batch_graph(), &quiet(), json!([])))), true);
        let mut reads = gather(client, Duration::from_secs(2), "/nonexistent-run").await;
        reads.limits.insert("SPIRA_COMPILE_PAR".into(), "7".into());
        let f = build(&reads, NOW).unwrap();
        let compile = by_id(&f, "components", "compile");
        assert_eq!(compile["queues"][0]["config"], 7);
        assert!(by_id(&f, "components", "checks")["queues"][0]["config"].is_null());
    }

    #[tokio::test]
    async fn activated_releases_of_the_last_day_are_the_release_exits_count() {
        let t = testkit::TempDir::new("loom-factory-releases");
        let mut r = release::machine::Release::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        for (to, at) in [(release::machine::ReleaseState::Cut, 1), (release::machine::ReleaseState::Accepted, 2), (release::machine::ReleaseState::Published, 3), (release::machine::ReleaseState::Activated, NOW as u64 - 100)] {
            r.record(to, "t", at).unwrap();
        }
        release::machine::save(&t.path().join("release"), &r).unwrap();
        let client = LcClient::over(Arc::new(Answers(script(batch_graph(), &quiet(), json!([])))), true);
        let reads = gather(client, Duration::from_secs(2), t.path().to_str().unwrap()).await;
        let f = build(&reads, NOW).unwrap();
        assert_eq!(by_id(&f, "belts", "rel.ACTIVATED")["in24h"], 1);
        assert!(by_id(&f, "belts", "pub.MERGED")["in24h"].is_null());
    }
}
