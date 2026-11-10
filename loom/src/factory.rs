//! `GET /api/factory`: the factory floor as one JSON object. Belts are the states of the bead,
//! batch, ask and mending machines and edges are those machines' own legal moves, so a state
//! added to a machine is a belt here with no change to loom. The tables below hold words only;
//! every number is read.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde_json::{json, Value};
use spira_config::lc_call::LcClient;

use spira_runtime::tasks::e::watch_transitions as wt;

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

/// Where each `spira-lc stats` verb is drawn; a verb not named here sits on the control bus.
const VERB_STATIONS: &[(&str, &[&str])] = &[("claim-verdicts", &["bead.claim"]), ("work", &["bead.build"])];
/// The tsd families read, and where each is drawn; the second is the field that holds the wait.
const TSD_SENSORS: &[(&str, &str, &[&str])] = &[("admission", "waited_secs", &["bead.build"]), ("cert-pass", "capacity_wait_s", &["batch.pass", "batch.CI_RUNNING"])];
const BUS: &str = "*";
const RELEASE_FEED: &str = "batch.LANDED";
const NO_BATCH_WATCH: &str = "no watch.transitions sensor on the batch machine";
/// What the model names that nothing measures: id, where, short, reading.
const BLIND_SPOTS: &[(&str, &[&str], &str, &str)] = &[
    ("miss.birth", &["bead.file"], "arrival rate", "filing writes no event, so the arrival rate into READY cannot be measured"),
    ("miss.session", &["bead.build"], "wait vs work", "cold-slot wait, warm pick and jitter sit inside phase session; nothing separates waiting from working"),
    ("miss.pool", &["bead.cut"], "why not cut", "the pool filter is silent: why a SUBMITTED bead is not in a round is not recorded"),
    ("miss.release", &["bead.unclaim"], "why released", "Release carries no evidence, so why a session ended unsubmitted is not on the row"),
    ("miss.attr", &["batch.eject"], "Attributed", "Attributed is never written; per-member attribution is invisible on the batch machine"),
    ("miss.ctrl", &["batch.pass", "bead.cut"], "controller", "batcher.local controller state (Assembling…Done) lives in a runtime stream with no CLI reader"),
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
    /// The bounds and the snapshot `watch.transitions` judges; the refusal when the bounds are refused.
    pub transitions: Result<(wt::Config, wt::Snapshot), String>,
    pub stats: Read,
    /// The last 24 h of transitions counted in VictoriaLogs, per (machine, event, from, to).
    pub counts: Result<Vec<Tally>, String>,
    pub tsd: BTreeMap<&'static str, Result<Vec<Value>, String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tally {
    pub machine: String,
    pub event: String,
    pub from: String,
    pub to: String,
    pub n: i64,
}

fn read_counts(url: &str) -> Result<Vec<Tally>, String> {
    let logs = spira_otel::vlogs::Logs::new(url).map_err(|e| e.to_string())?;
    let groups = logs.count_groups(&spira_otel::vlogs::transitions_window(DAY as u64), &["machine", "event", "from", "to"]).map_err(|e| e.to_string())?;
    Ok(groups
        .into_iter()
        .map(|(k, n)| Tally { machine: k["machine"].clone(), event: k["event"].clone(), from: k["from"].clone(), to: k["to"].clone(), n: n as i64 })
        .collect())
}

/// What the sensors are read with.
pub struct Sensing {
    pub victorialogs_url: String,
    pub transitions: Result<wt::Config, String>,
    pub tsd_endpoint: Option<String>,
    pub now: i64,
}

async fn read(client: &LcClient, args: &[&str], budget: Duration) -> Read {
    crate::lc_answer_json(client.clone(), args, budget).await
}

/// Every read the floor is built from, all at once.
pub async fn gather(client: LcClient, budget: Duration, run: &str, sensing: &Sensing) -> Reads {
    let c = &client;
    let release_dir = std::path::Path::new(run).join("release");
    let releases = async move { tokio::task::spawn_blocking(move || release::machine::all(&release_dir)).await.map_err(|e| format!("release records: {e}"))? };
    let observed = async {
        let cfg = sensing.transitions.clone()?;
        let (lc, run, window, releases, cfg_repo, repo_map) = (
            client.within(budget),
            std::path::PathBuf::from(run),
            cfg.window_s(),
            cfg.releases.clone(),
            cfg.release_repo.clone(),
            cfg.repo_map.clone(),
        );
        let snap = tokio::task::spawn_blocking(move || {
            let ask = |argv: &[&str]| -> Result<Value, String> {
                let r = lc.call(argv).map_err(|e| e.to_string())?;
                if r.code != 0 {
                    return Err(format!("exited {}: {}", r.code, r.why().chars().take(300).collect::<String>()));
                }
                serde_json::from_str(&r.out).map_err(|e| e.to_string())
            };
            wt::observe(
                &run,
                &releases,
                &cfg_repo,
                &ask,
                &wt::repo_ancestor(&repo_map, &cfg_repo),
                window,
            )
        })
        .await
        .map_err(|e| format!("watch.transitions observe: {e}"))?;
        Ok((cfg, snap))
    };
    let counts = {
        let url = sensing.victorialogs_url.clone();
        async move { tokio::task::spawn_blocking(move || read_counts(&url)).await.map_err(|e| format!("victorialogs counts: {e}")).and_then(|r| r) }
    };
    let tsd_rows = |family: &'static str| {
        let endpoint = sensing.tsd_endpoint.clone();
        let (from, to) = ((sensing.now - DAY).max(0) as u64, sensing.now.max(0) as u64);
        async move {
            let read = tokio::task::spawn_blocking(move || tsd::store::Reader::new(endpoint.as_deref()).and_then(|r| r.rows_between(family, from, to)).map_err(|e| e.to_string()));
            read.await.map_err(|e| e.to_string()).and_then(|r| r)
        }
    };
    let (g_bead, g_batch, g_ask, g_mending, t_bead, t_batch, live, batches, asks, mending, releases, transitions, stats, counts, admission, cert_pass) = tokio::join!(
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
        observed,
        read(c, &["stats"], budget),
        counts,
        tsd_rows("admission"),
        tsd_rows("cert-pass"),
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
        transitions,
        stats,
        counts,
        tsd: BTreeMap::from([("admission", admission), ("cert-pass", cert_pass)]),
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
        .filter_map(|r| Some(Move { machine, event: text(&r["event"])?, from: text(&r["from_state"])?, at: num(&r["at"])? }))
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
    if let Err(e) = &reads.counts {
        errors.push(format!("24 h counts: {e}"));
    }
    let tallies: &[Tally] = reads.counts.as_deref().unwrap_or(&[]);
    let counted_24h = reads.counts.is_ok();

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

    // Rounds in flight, as the batch machine's own edges say: those of the watched repo not yet at a terminal belt.
    let open_rounds = reads.transitions.as_ref().ok().map(|_| {
        let mut by: BTreeMap<String, u64> = BTreeMap::new();
        for repo in items.iter().filter(|i| i.kind == "batch").filter_map(|i| i.repo.clone()) {
            *by.entry(repo).or_default() += 1;
        }
        by
    });

    // Belts as JSON.
    let in24h_releases = reads.releases.as_ref().ok().map(|rs| rs.iter().flat_map(|r| r.events.iter()).filter(|e| e.to == release::machine::ReleaseState::Activated && e.at as i64 >= now - DAY).count() as i64);
    let mut belts_json: Vec<Value> = Vec::new();
    for (id, lane, label, state, kind) in &belts {
        let machine = id.split_once('.').map_or("", |(m, _)| m);
        let here: Vec<&Item> = items.iter().filter(|i| &i.at == id).collect();
        let counted = *kind != "sink" && machine_read_ok(machine);
        let in24h = (*kind == "sink" && counted_24h).then(|| tallies.iter().filter(|t| t.machine == machine && &t.to == state).map(|t| t.n).sum::<i64>());
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
    if belt_exists(RELEASE_FEED) {
        belts_json.push(json!({"id": "rel.ACTIVATED", "lane": "batch", "kind": "offmachine", "label": "RELEASE LIVE", "machine": "release", "state": "ACTIVATED",
            "now": null, "oldest_s": null, "in24h": in24h_releases, "parts": null}));
        belts_json.push(json!({"id": "pub.MERGED", "lane": "batch", "kind": "offmachine", "label": "ON ORIGIN", "machine": "publish", "state": "MERGED",
            "now": null, "oldest_s": null, "in24h": null, "parts": null}));
    }

    // Stations and edges.
    let tally = |refs: &[EventRef], pick: &dyn Fn(&Tally) -> bool| -> Option<i64> {
        counted_24h.then(|| tallies.iter().filter(|t| refs.iter().any(|r| r.takes(&t.machine, &t.event, &t.from)) && pick(t)).map(|t| t.n).sum())
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
        let per24h = if refs.is_empty() { None } else { tally(&refs, &|_| true) };
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
            let n = tally(&refs, &|t| t.machine == m && t.from == s);
            edges_json.push(json!({"from": b, "to": st.id, "n": n, "kind": live_kind("flow"), "label": null}));
        }
        for o in &outputs {
            let (m, s) = o.split_once('.').unwrap_or(("", ""));
            let created = st.creates.as_deref() == Some(o.as_str()) && !st.legal.iter().any(|&i| belt_id(edges[i].machine, &edges[i].to) == *o);
            let n = if created { None } else { tally(&refs, &|t| t.machine == m && t.to == s) };
            let kind = if st.back.contains(o) { live_kind("back") } else { live_kind("flow") };
            edges_json.push(json!({"from": st.id, "to": o, "n": n, "kind": kind, "label": null}));
        }
        for (to, label) in &st.signals {
            edges_json.push(json!({"from": st.id, "to": to, "n": null, "kind": live_kind("signal"), "label": label}));
        }
    }
    if belt_exists(RELEASE_FEED) {
        edges_json.push(json!({"from": RELEASE_FEED, "to": "rel.ACTIVATED", "n": in24h_releases, "kind": "flow", "label": "release"}));
        edges_json.push(json!({"from": RELEASE_FEED, "to": "pub.MERGED", "n": null, "kind": "flow", "label": "publish PR"}));
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
        "sensors": sensors(reads, now, open_rounds, &stations, &edges, &|id| belt_exists(id), &|st| outs(st)),
    }))
}

struct Sensor {
    id: String,
    kind: &'static str,
    at: Vec<String>,
    status: &'static str,
    short: String,
    reading: String,
    src: &'static str,
    repo: Option<String>,
}

impl Sensor {
    fn new(id: impl Into<String>, kind: &'static str, at: Vec<String>, status: &'static str, src: &'static str) -> Sensor {
        Sensor { id: id.into(), kind, at, status, short: String::new(), reading: String::new(), src, repo: None }
    }

    fn says(mut self, short: impl Into<String>, reading: impl Into<String>) -> Sensor {
        self.short = short.into();
        self.reading = reading.into();
        self
    }

    fn json(&self) -> Value {
        json!({"id": self.id, "kind": self.kind, "at": self.at, "status": self.status, "short": self.short, "reading": self.reading, "src": self.src, "repo": self.repo})
    }
}

fn strings(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

fn secs(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).filter(|x| *x >= 0.0)
}

/// A mean in ms that keeps its fraction under 10, so a fast verb never reads as zero.
fn mean_ms(total: i64, n: i64) -> String {
    let m = total as f64 / n as f64;
    if m >= 10.0 { format!("{}", m.round()) } else { format!("{m:.1}") }
}

fn tsd_reading(rows: &[Value], wait_field: &str) -> Result<(String, String), String> {
    if rows.is_empty() {
        return Err("no row in the last 24 h".into());
    }
    let waits: Vec<f64> = rows.iter().filter_map(|r| secs(&r[wait_field])).collect();
    if waits.is_empty() {
        return Err(format!("{} rows in the last 24 h, none carrying {wait_field}", rows.len()));
    }
    let waited = waits.iter().filter(|w| **w > 0.0).count();
    let at = |p: f64| tsd::query::quantile(&waits, p).map_or(0, |q| q.round() as u64);
    Ok((format!("{} in 24 h", rows.len()), format!("{} in 24 h · {waited} waited · {wait_field} p50 {} s · p90 {} s", rows.len(), at(0.5), at(0.9))))
}

/// Every sensor of the floor. Nothing here is a zero that was not read: what cannot be read is
/// BLIND and says why, what is not measured at all is MISSING.
fn sensors(reads: &Reads, now: i64, open_rounds: Option<BTreeMap<String, u64>>, stations: &[Built], edges: &[Edge], belt_exists: &dyn Fn(&str) -> bool, outs: &dyn Fn(&Built) -> Vec<String>) -> Vec<Value> {
    let mut out: Vec<Sensor> = Vec::new();
    let anywhere = |ids: Vec<String>| if ids.is_empty() { vec![BUS.to_string()] } else { ids };
    let at_edge = |machine: &str, from: &str, to: &str| -> Vec<String> {
        anywhere(stations.iter().filter(|st| st.legal.iter().any(|&i| edges[i].machine == machine && edges[i].from == from && edges[i].to == to)).map(|st| st.id.clone()).collect())
    };
    let feeders = || anywhere(stations.iter().filter(|st| st.live && outs(st).iter().any(|o| o == RELEASE_FEED)).map(|st| st.id.clone()).collect());

    let row_at = |name: &str| {
        let belt = if name == "ASK" { "ask.OPEN".to_string() } else { format!("bead.{name}") };
        if belt_exists(&belt) { belt } else { BUS.to_string() }
    };
    let edge_at = |edge: &str| match edge {
        "LANDED→ACTIVATED" => feeders(),
        "ASK→ANSWERED" => at_edge("ask", "OPEN", "ANSWERED"),
        _ => edge.split_once('→').map_or_else(|| vec![BUS.to_string()], |(from, to)| at_edge("bead", from, to)),
    };
    let blind_why = match &reads.transitions {
        Err(e) => Some(e.clone()),
        Ok((_, snap)) if !snap.readable => Some(snap.why.clone()),
        Ok(_) => None,
    };
    if let Some(why) = &blind_why {
        for name in wt::ROWS {
            out.push(Sensor::new(format!("wt.{name}"), "transitions", vec![row_at(name)], "BLIND", "watch.transitions").says("BLIND", format!("BLIND {name}: {why}")));
        }
        for edge in wt::EDGES {
            out.push(Sensor::new(format!("wt.{edge}"), "transitions", edge_at(edge), "BLIND", "watch.transitions").says("BLIND", format!("BLIND {edge}: {why}")));
        }
    } else if let Ok((cfg, snap)) = &reads.transitions {
        let now = now.max(0) as u64;
        let mut snap = snap.clone();
        snap.open_rounds = open_rounds.filter(|_| reads.batches.is_ok());
        let snap = &snap;
        let table = wt::table(snap, wt::ALL, now);
        for name in wt::ROWS {
            let reading = wt::row_text(name, &table[name]);
            out.push(Sensor::new(format!("wt.{name}"), "transitions", vec![row_at(name)], "WAIT", "watch.transitions").says(reading.clone(), reading));
        }
        let stalled = wt::stalls(snap, now, cfg);
        for edge in wt::EDGES {
            let at = edge_at(edge);
            let found: Vec<&wt::Stall> = stalled.iter().filter(|s| s.edge == edge).collect();
            if found.is_empty() {
                out.push(Sensor::new(format!("wt.{edge}"), "transitions", at.clone(), "WAIT", "watch.transitions").says("WAIT", format!("WAIT {edge}: inside its bound")));
            }
            for s in found {
                let mut sensor = Sensor::new(format!("wt.{edge}:{}", s.scope), "transitions", at.clone(), "STALL", "watch.transitions").says(format!("STALL {}", s.scope), format!("STALL {edge} ({}): {}", s.scope, s.evidence));
                sensor.repo = Some(s.scope.clone());
                out.push(sensor);
            }
        }
    }

    match &reads.stats {
        Err(e) => out.push(Sensor::new("perf.stats", "perf", vec![BUS.into()], "BLIND", "spira-lc stats").says("BLIND", format!("BLIND: {e}"))),
        Ok(stats) => {
            let verbs = stats["verbs"].as_object();
            let mut named = BTreeSet::new();
            for (verb, at) in VERB_STATIONS {
                named.insert(*verb);
                let id = format!("perf.{verb}");
                match verbs.and_then(|v| v.get(*verb)).and_then(|v| Some((num(&v["count"])?, num(&v["total_ms"])?))).filter(|(n, _)| *n > 0) {
                    Some((n, ms)) => out.push(Sensor::new(id, "perf", strings(at), "OK", "spira-lc stats").says(format!("mean {} ms", mean_ms(ms, n)), format!("{n} calls · mean {} ms", mean_ms(ms, n)))),
                    None => out.push(Sensor::new(id, "perf", strings(at), "BLIND", "spira-lc stats").says("no calls", format!("BLIND: no {verb} call counted since lc-serve started"))),
                }
            }
            for (verb, v) in verbs.into_iter().flatten().filter(|(k, _)| !named.contains(k.as_str())) {
                if let (Some(n), Some(ms)) = (num(&v["count"]).filter(|n| *n > 0), num(&v["total_ms"])) {
                    out.push(Sensor::new(format!("perf.{verb}"), "perf", vec![BUS.into()], "OK", "spira-lc stats").says(format!("mean {} ms", mean_ms(ms, n)), format!("{verb}: {n} calls · mean {} ms", mean_ms(ms, n))));
                }
            }
            let writes = &stats["writes"];
            match (num(&writes["writes"]), num(&writes["commits"]), num(&writes["groups"])) {
                (Some(w), Some(c), Some(g)) => out.push(Sensor::new("perf.writes", "perf", vec![BUS.into()], "OK", "spira-lc stats").says(format!("{w} writes"), format!("{w} writes · {c} commits · {g} groups since lc-serve started"))),
                _ => out.push(Sensor::new("perf.writes", "perf", vec![BUS.into()], "BLIND", "spira-lc stats").says("BLIND", "BLIND: stats answered no write counters")),
            }
        }
    }

    for (family, wait_field, at) in TSD_SENSORS {
        let sensor = Sensor::new(format!("tsd.{family}"), "tsd", strings(at), "OK", "tsd");
        let read = reads.tsd.get(family).ok_or_else(|| "not read".to_string()).and_then(|r| r.clone()).and_then(|rows| tsd_reading(&rows, wait_field));
        out.push(match read {
            Ok((short, reading)) => sensor.says(short, format!("{family}: {reading}")),
            Err(e) => Sensor { status: "BLIND", ..sensor.says("BLIND", format!("BLIND {family}: {e}")) },
        });
    }

    let non_terminal: BTreeSet<&str> = edges.iter().filter(|e| e.machine == "batch").map(|e| e.from.as_str()).collect();
    for state in non_terminal {
        let id = format!("batch.{state}");
        out.push(Sensor::new(format!("miss.{id}"), "missing", vec![id], "MISSING", "(none)").says("no sensor", NO_BATCH_WATCH));
    }
    for (id, at, short, reading) in BLIND_SPOTS {
        out.push(Sensor::new(*id, "missing", strings(at), "MISSING", "(none)").says(*short, *reading));
    }
    out.iter().map(Sensor::json).collect()
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

    fn bounds() -> wt::Config {
        wt::Config {
            period_s: 300,
            working_min: 1,
            submit_bound_s: 3600,
            cut_bound_s: 600,
            deliver_bound_s: 3600,
            activate_bound_s: 3600,
            ask_bound_s: 3600,
            release_repo: "spira".into(),
            heartbeat_bound_s: 300,
            releases: "/nonexistent-releases".into(),
            repo_map: "/nonexistent-map".into(),
        }
    }

    fn sensing() -> Sensing {
        Sensing { victorialogs_url: String::new(), transitions: Ok(bounds()), tsd_endpoint: None, now: NOW }
    }

    use spira_otel::vlogs::fake::{FakeLogs, Script};

    fn tally_lines(rows: &[(&str, &str, &str, &str, i64)]) -> String {
        rows.iter().map(|(m, e, f, t, n)| json!({"machine": m, "event": e, "from": f, "to": t, "n": n.to_string()}).to_string() + "\n").collect()
    }

    /// A VictoriaLogs holding these 24 h transition counts, and the sensing that reads it.
    fn counted(rows: &[(&str, &str, &str, &str, i64)]) -> (FakeLogs, Sensing) {
        let logs = FakeLogs::start();
        logs.answer("stats by", Script { status: 200, body: tally_lines(rows), hang: false });
        let s = Sensing { victorialogs_url: logs.url(), ..sensing() };
        (logs, s)
    }

    async fn floor(answers: Vec<(String, i32, String)>) -> Result<Value, String> {
        floor_with(answers, "/nonexistent-run", &sensing()).await
    }

    async fn floor_with(answers: Vec<(String, i32, String)>, run: &str, sensing: &Sensing) -> Result<Value, String> {
        let client = LcClient::over(Arc::new(Answers(answers)), true);
        let reads = gather(client, Duration::from_secs(2), run, sensing).await;
        build(&reads, NOW)
    }

    /// A run directory holding one release activated at `NOW - 100`.
    fn run_with_a_release() -> testkit::TempDir {
        let t = testkit::TempDir::new("loom-factory-sensors");
        let mut r = release::machine::Release::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        for (to, at) in [(release::machine::ReleaseState::Cut, 1), (release::machine::ReleaseState::Accepted, 2), (release::machine::ReleaseState::Published, 3), (release::machine::ReleaseState::Activated, NOW as u64 - 100)] {
            r.record(to, "t", at).unwrap();
        }
        release::machine::save(&t.path().join("release"), &r).unwrap();
        t
    }

    fn dwell_key() -> String {
        format!("dwell --window {} --landed-after {}", bounds().window_s(), NOW - 100)
    }

    fn sensor<'a>(f: &'a Value, id: &str) -> &'a Value {
        by_id(f, "sensors", id)
    }

    fn of_kind<'a>(f: &'a Value, kind: &str) -> Vec<&'a Value> {
        f["sensors"].as_array().unwrap().iter().filter(|s| s["kind"] == kind).collect()
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
        let gantt = json!({"now": NOW, "events": [row(1, "Release", NOW - 60), row(2, "Release", NOW - 600), row(3, "HolderDead", NOW - 6000)]});
        let (_logs, s) = counted(&[("bead", "Release", "WORKING", "READY", 2), ("bead", "HolderDead", "WORKING", "READY", 1), ("bead", "Submit", "WORKING", "SUBMITTED", 1)]);
        let f = floor_with(script(batch_graph(), &gantt, json!([])), "/nonexistent-run", &s).await.unwrap();
        let st = by_id(&f, "stations", "bead.unclaim");
        assert_eq!((st["per24h"].as_i64(), st["last_s"].as_i64()), (Some(3), Some(60)));
        let edge = f["edges"].as_array().unwrap().iter().find(|e| e["from"] == "bead.WORKING" && e["to"] == "bead.unclaim").unwrap();
        assert_eq!(edge["n"], 3);
        assert_eq!(by_id(&f, "stations", "bead.build")["per24h"], 1);
    }

    #[tokio::test]
    async fn a_sinks_in24h_is_the_logsql_count_into_it_and_an_unreadable_store_is_null_with_an_error() {
        let (logs, s) = counted(&[("bead", "Drop", "READY", "DROPPED", 2), ("bead", "Drop", "SUBMITTED", "DROPPED", 3)]);
        let f = floor_with(script(batch_graph(), &quiet(), json!([])), "/nonexistent-run", &s).await.unwrap();
        assert_eq!(by_id(&f, "belts", "bead.DROPPED")["in24h"], 5);
        assert!(logs.queries().iter().any(|q| q.starts_with(&spira_otel::vlogs::transitions_window(86_400))), "{:?}", logs.queries());

        let down = FakeLogs::start();
        down.answer("stats by", Script { status: 503, body: "busy".into(), hang: false });
        let s = Sensing { victorialogs_url: down.url(), ..sensing() };
        let f = floor_with(script(batch_graph(), &quiet(), json!([])), "/nonexistent-run", &s).await.unwrap();
        assert!(by_id(&f, "belts", "bead.DROPPED")["in24h"].is_null());
        assert!(by_id(&f, "stations", "bead.build")["per24h"].is_null());
        let errors = f["meta"]["errors"].as_array().unwrap();
        assert!(errors.iter().any(|e| e.as_str().unwrap().starts_with("24 h counts")), "{errors:?}");
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
        let mut reads = gather(client, Duration::from_secs(2), "/nonexistent-run", &sensing()).await;
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
        let reads = gather(client, Duration::from_secs(2), t.path().to_str().unwrap(), &sensing()).await;
        let f = build(&reads, NOW).unwrap();
        assert_eq!(by_id(&f, "belts", "rel.ACTIVATED")["in24h"], 1);
        assert!(by_id(&f, "belts", "pub.MERGED")["in24h"].is_null());
    }

    #[tokio::test]
    async fn submitted_work_with_no_cut_inside_the_bound_is_a_stall_sensor_at_round_open_for_its_repo() {
        let run = run_with_a_release();
        let dwell = json!({"beads": [{"id": "sp-a", "repo": "spira", "state": "SUBMITTED", "since": NOW - 7200}], "horizon": NOW - 4200, "last": {"spira": {}}, "landed_after": {}});
        let mut answers = script(batch_graph(), &quiet(), json!([]));
        answers.push(ok(&dwell_key(), dwell.to_string()));
        let f = floor_with(answers, run.path().to_str().unwrap(), &sensing()).await.unwrap();
        let stalls: Vec<&Value> = of_kind(&f, "transitions").into_iter().filter(|s| s["status"] == "STALL").collect();
        assert_eq!(stalls.len(), 1, "{stalls:?}");
        assert_eq!((stalls[0]["repo"].as_str(), stalls[0]["at"].clone()), (Some("spira"), json!(["bead.cut"])));
        assert!(stalls[0]["reading"].as_str().unwrap().contains("no Cut"), "{}", stalls[0]["reading"]);
        assert_eq!(sensor(&f, "wt.SUBMITTED")["status"], "WAIT");
        assert!(sensor(&f, "wt.SUBMITTED")["reading"].as_str().unwrap().starts_with("SUBMITTED 1 oldest 2h00m"));
        assert_eq!(sensor(&f, "wt.WORKING→SUBMITTED")["status"], "WAIT");
        assert_eq!(sensor(&f, "wt.LANDED→ACTIVATED")["at"], json!(["bead.land"]));
        assert_eq!(sensor(&f, "wt.ASK→ANSWERED")["at"], json!(["ask.close"]));
    }

    #[tokio::test]
    async fn submitted_work_behind_a_full_set_of_open_rounds_is_queued_not_a_stall_sensor() {
        let run = run_with_a_release();
        let dwell = json!({"beads": [{"id": "sp-a", "repo": "spira", "state": "SUBMITTED", "since": NOW - 7200}], "horizon": NOW - 4200, "last": {"spira": {}}, "landed_after": {}});
        let round = |id: &str| json!({"batch_id": id, "state": "OPEN", "repo": "spira", "members": ["sp-x"], "pass": 1, "opened_at": NOW - 60});
        let mut answers = script(batch_graph(), &quiet(), json!([]));
        answers.retain(|a| a.0 != "list --batches");
        answers.push(ok("list --batches", json!([round("b-1"), round("b-2")]).to_string()));
        answers.push(ok(&dwell_key(), dwell.to_string()));
        let f = floor_with(answers, run.path().to_str().unwrap(), &sensing()).await.unwrap();
        assert!(of_kind(&f, "transitions").iter().all(|s| s["status"] != "STALL"), "{:?}", of_kind(&f, "transitions"));
    }

    #[tokio::test]
    async fn a_round_open_in_another_repo_leaves_this_repos_stall_standing() {
        let run = run_with_a_release();
        let dwell = json!({"beads": [{"id": "sp-a", "repo": "spira", "state": "SUBMITTED", "since": NOW - 7200}], "horizon": NOW - 4200, "last": {"spira": {}}, "landed_after": {}});
        let round = json!({"batch_id": "b-1", "state": "ATTRIBUTING", "repo": "elsewhere", "members": ["sp-x"], "pass": 1, "opened_at": NOW - 60});
        let mut answers = script(batch_graph(), &quiet(), json!([]));
        answers.retain(|a| a.0 != "list --batches");
        answers.push(ok("list --batches", json!([round]).to_string()));
        answers.push(ok(&dwell_key(), dwell.to_string()));
        let f = floor_with(answers, run.path().to_str().unwrap(), &sensing()).await.unwrap();
        let stalls: Vec<&Value> = of_kind(&f, "transitions").into_iter().filter(|s| s["status"] == "STALL").collect();
        assert!(stalls.iter().any(|s| s["repo"] == "spira" && s["reading"].as_str().unwrap().contains("no Cut")), "{stalls:?}");
    }

    #[tokio::test]
    async fn a_refused_dwell_makes_every_transitions_sensor_blind_and_names_the_refusal() {
        let run = run_with_a_release();
        let mut answers = script(batch_graph(), &quiet(), json!([]));
        answers.push((dwell_key(), 3, "refused: store sealed".into()));
        let f = floor_with(answers, run.path().to_str().unwrap(), &sensing()).await.unwrap();
        let all = of_kind(&f, "transitions");
        assert!(all.len() > wt::EDGES.len(), "{all:?}");
        for s in all {
            assert_eq!(s["status"], "BLIND", "{s}");
            assert!(s["reading"].as_str().unwrap().contains("store sealed"), "{s}");
        }
    }

    #[tokio::test]
    async fn pinned_bounds_that_are_refused_make_every_transitions_sensor_blind_and_name_the_key() {
        let refused = Sensing { transitions: Err("SPIRA_TRANSITIONS_CUT_BOUND: unset".into()), ..sensing() };
        let f = floor_with(script(batch_graph(), &quiet(), json!([])), "/nonexistent-run", &refused).await.unwrap();
        let all = of_kind(&f, "transitions");
        assert!(!all.is_empty());
        assert!(all.iter().all(|s| s["status"] == "BLIND" && s["reading"].as_str().unwrap().contains("SPIRA_TRANSITIONS_CUT_BOUND")), "{all:?}");
    }

    #[tokio::test]
    async fn no_release_record_is_blind_not_an_empty_wait() {
        let f = floor(script(batch_graph(), &quiet(), json!([]))).await.unwrap();
        assert_eq!(sensor(&f, "wt.READY")["status"], "BLIND");
    }

    #[tokio::test]
    async fn every_non_terminal_batch_belt_has_a_missing_sensor_including_one_for_an_added_state() {
        let grown = graph(&[("OPEN", "CiStarted", "CI_RUNNING"), ("CI_RUNNING", "Hold", "HELD"), ("HELD", "PassGreen", "GREEN"), ("GREEN", "FastForward", "LANDED")]);
        let f = floor(script(grown, &quiet(), json!([]))).await.unwrap();
        let missing: BTreeSet<&str> = of_kind(&f, "missing").into_iter().map(|s| s["id"].as_str().unwrap()).collect();
        let belts = f["belts"].as_array().unwrap();
        let non_terminal: Vec<&str> = belts.iter().filter(|b| b["machine"] == "batch" && b["kind"] != "sink").map(|b| b["id"].as_str().unwrap()).collect();
        assert!(non_terminal.contains(&"batch.HELD"), "{non_terminal:?}");
        for id in non_terminal {
            let s = sensor(&f, &format!("miss.{id}"));
            assert_eq!((s["status"].as_str(), s["reading"].as_str(), s["at"].clone()), (Some("MISSING"), Some(NO_BATCH_WATCH), json!([id])));
        }
        assert!(!missing.contains("miss.batch.LANDED"), "a terminal belt has nothing to watch");
        for (id, ..) in BLIND_SPOTS {
            assert!(missing.contains(id), "{id}");
        }
    }

    #[tokio::test]
    async fn a_stats_answer_is_the_count_and_mean_at_the_stations_its_verb_is_declared_for() {
        let mut answers = script(batch_graph(), &quiet(), json!([]));
        let stats = json!({"verbs": {"work": {"count": 4, "total_ms": 4000}, "renew": {"count": 2, "total_ms": 30}, "peek": {"count": 10, "total_ms": 4}}, "callers": [], "writes": {"writes": 9, "commits": 3, "groups": 2}});
        answers.push(ok("stats", stats.to_string()));
        let f = floor(answers).await.unwrap();
        let work = sensor(&f, "perf.work");
        assert_eq!((work["reading"].as_str(), work["at"].clone(), work["status"].as_str()), (Some("4 calls · mean 1000 ms"), json!(["bead.build"]), Some("OK")));
        assert_eq!(sensor(&f, "perf.renew")["at"], json!(["*"]));
        assert_eq!(sensor(&f, "perf.peek")["reading"], "peek: 10 calls · mean 0.4 ms", "a fast verb is not a zero mean");
        assert_eq!(sensor(&f, "perf.claim-verdicts")["status"], "BLIND", "a verb never called is not a zero");
        assert!(sensor(&f, "perf.writes")["reading"].as_str().unwrap().starts_with("9 writes · 3 commits · 2 groups"));
    }

    #[tokio::test]
    async fn an_unreadable_stats_and_an_unreadable_tsd_are_blind_and_name_why() {
        let f = floor(script(batch_graph(), &quiet(), json!([]))).await.unwrap();
        assert_eq!(sensor(&f, "perf.stats")["status"], "BLIND");
        assert!(sensor(&f, "perf.stats")["reading"].as_str().unwrap().contains("stats"));
        for family in ["admission", "cert-pass"] {
            let s = sensor(&f, &format!("tsd.{family}"));
            assert_eq!(s["status"], "BLIND", "{s}");
            assert!(s["reading"].as_str().unwrap().len() > "BLIND admission: ".len(), "{s}");
        }
        assert_eq!(sensor(&f, "tsd.cert-pass")["at"], json!(["batch.pass", "batch.CI_RUNNING"]));
    }

    #[test]
    fn a_tsd_reading_is_the_count_how_many_waited_and_the_wait_percentiles() {
        let rows: Vec<Value> = [0, 0, 5, 10].iter().map(|w| json!({"pool": "build", "waited_secs": w, "held_secs": 3})).collect();
        let (short, reading) = tsd_reading(&rows, "waited_secs").unwrap();
        assert_eq!(short, "4 in 24 h");
        assert_eq!(reading, "4 in 24 h · 2 waited · waited_secs p50 3 s · p90 9 s");
        assert!(tsd_reading(&[], "waited_secs").unwrap_err().contains("no row"));
        assert!(tsd_reading(&[json!({"pool": "build"})], "waited_secs").unwrap_err().contains("waited_secs"));
    }
}
