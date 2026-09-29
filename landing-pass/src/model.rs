//! The data this pass handles (DESIGN.md §3), as typed values with their on-disk and
//! on-the-wire formats.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A repository's land mode as lib.sh `repo_land` reports it (`queue.forge` → `queue`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LandMode {
    Push,
    Pr,
    Hold,
    Queue,
    QueueLocal,
    /// A value repo_land passed through that is none of the above: walked like push (the
    /// shell's `*)` arm), never refreshed.
    Other(String),
}

impl LandMode {
    pub fn parse(s: &str) -> LandMode {
        match s {
            "" | "push" => LandMode::Push,
            "pr" => LandMode::Pr,
            "hold" => LandMode::Hold,
            "queue" | "queue.forge" => LandMode::Queue,
            "queue.local" => LandMode::QueueLocal,
            o => LandMode::Other(o.to_string()),
        }
    }
    /// `repo_land_queued`.
    pub fn queued(&self) -> bool {
        matches!(self, LandMode::Queue | LandMode::QueueLocal)
    }
    pub fn as_str(&self) -> &str {
        match self {
            LandMode::Push => "push",
            LandMode::Pr => "pr",
            LandMode::Hold => "hold",
            LandMode::Queue => "queue",
            LandMode::QueueLocal => "queue.local",
            LandMode::Other(s) => s,
        }
    }
}

/// One repository, resolved by lib.sh (context seam S1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoRow {
    pub name: String,
    /// Empty when `repo_root` has no entry for the name.
    pub path: PathBuf,
    pub mode: LandMode,
    pub landref: Option<String>,
    pub base_fq: Option<String>,
    pub base_remote: Option<String>,
    pub base_branch: String,
    pub forge_ref: Option<String>,
}

/// conf.sh's settings, as the context seam exports them (DESIGN.md §2.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub home: PathBuf,
    pub run: PathBuf,
    pub repo: PathBuf,
    pub db: String,
    pub bd: String,
    pub bd_timeout: u64,
    pub home_repo: String,
    pub id_prefix: String,
    pub land_maxsec: i64,
    pub gate_reserve: i64,
    pub gate_lock_wait: Option<String>,
    pub verdict_ttl: u64,
    pub verdicts: PathBuf,
    pub deferral_escalate_at: u32,
    pub express_label: String,
    pub cutover_label: String,
    pub submitted_label: String,
    pub rebase_escalate_at: u32,
    pub git_name: String,
    pub git_email: String,
    pub incident: PathBuf,
    pub scope_label: String,
    pub queue_bin: Option<PathBuf>,
    pub queue_dir: PathBuf,
    pub lc_bin: Option<PathBuf>,
    pub prod: PathBuf,
    pub halt_grace: u64,
    /// conf.sh's PATH (`SPIRA_PATH` prepended to the box's tail) — what every command
    /// landing.sh ran was looked up in. `halt` runs podman and testenv.sh under it.
    pub path: Option<String>,
    pub bdjson_fixture: Option<PathBuf>,
    pub pr_pass_branch_sh: PathBuf,
    /// The config document conf.sh resolved (`SPIRA_TOML_FILE`), for the lifecycle switch.
    pub toml: Option<PathBuf>,
    /// THE lifecycle switch (DESIGN.md §9), resolved once per invocation by
    /// `lifecycle::lifecycle_on`. OFF: spira-lc is never invoked.
    pub lifecycle_enforce: bool,
}

impl Settings {
    pub fn landstate(&self) -> PathBuf {
        self.run.join("landstate")
    }

    /// Defaults for tests: everything under one run directory.
    pub fn for_run(run: PathBuf) -> Settings {
        Settings {
            home: run.join("home"),
            repo: run.join("repo"),
            db: "db".into(),
            bd: "bd".into(),
            bd_timeout: 180,
            home_repo: "spira".into(),
            id_prefix: "sp".into(),
            land_maxsec: 0,
            gate_reserve: 2700,
            gate_lock_wait: None,
            verdict_ttl: 86_400,
            verdicts: run.join("verdicts"),
            deferral_escalate_at: 5,
            express_label: "express".into(),
            cutover_label: "cutover-round".into(),
            submitted_label: "spira-submitted".into(),
            rebase_escalate_at: 3,
            git_name: "spira".into(),
            git_email: "spira@spira.invalid".into(),
            incident: run.join("home/incident.sh"),
            scope_label: String::new(),
            queue_bin: Some(PathBuf::from("queue")),
            queue_dir: run.join("queue"),
            lc_bin: None,
            prod: run.join("home"),
            halt_grace: 30,
            path: None,
            bdjson_fixture: None,
            pr_pass_branch_sh: run.join("home/pr-pass-branch.sh"),
            toml: None,
            lifecycle_enforce: false,
            run,
        }
    }
}

/// One bead from the bulk scan (`bd show --json`), read the way landing.sh's scan read it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeadRow {
    pub id: String,
    /// bd's status, except a bead carrying the submitted label reads `closed` (sp-qsona).
    pub status: String,
    /// bd's own status, unmapped (the prune asks about this one).
    pub raw_status: String,
    pub repo: String,
    pub labels: Vec<String>,
    pub superseded: bool,
    pub closed_at: String,
    pub priority: i64,
    pub external_ref: Option<String>,
}

impl BeadRow {
    /// Parse one element of `bd show --json`. `None` when it carries no id.
    pub fn from_json(v: &serde_json::Value, home_repo: &str, submitted_label: &str) -> Option<BeadRow> {
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if id.is_empty() {
            return None;
        }
        let raw_status = v.get("status").and_then(|x| x.as_str()).unwrap_or("-").to_string();
        let labels: Vec<String> = v
            .get("labels")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|l| l.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let status = if labels.iter().any(|l| l == submitted_label) { "closed".to_string() } else { raw_status.clone() };
        let repo = labels
            .iter()
            .find_map(|l| l.strip_prefix("repo:").map(String::from))
            .unwrap_or_else(|| home_repo.to_string());
        // `bd show` names the field dependency_type, `bd list` names it type: accept either.
        let superseded = v
            .get("dependencies")
            .and_then(|x| x.as_array())
            .map(|deps| {
                deps.iter().any(|d| {
                    d.get("dependency_type").or_else(|| d.get("type")).and_then(|t| t.as_str()) == Some("supersedes")
                })
            })
            .unwrap_or(false);
        let closed_at = v
            .get("closed_at")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("9999-99-99")
            .to_string();
        let priority = v.get("priority").and_then(|x| x.as_i64()).unwrap_or(9999);
        let external_ref = v
            .get("external_ref")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty() && *s != "-")
            .map(String::from);
        Some(BeadRow { id, status, raw_status, repo, labels, superseded, closed_at, priority, external_ref })
    }

    pub fn has_label(&self, l: &str) -> bool {
        !l.is_empty() && self.labels.iter().any(|x| x == l)
    }
}

/// `$SPIRA_RUN/landstate/<id>`: "<STATE> <tip|none> <epoch> [reason…]", no newline.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandState {
    pub state: String,
    pub tip: String,
    pub at: u64,
    pub reason: String,
}

impl LandState {
    /// `read -r st tip at reason` over the record with its newlines removed.
    pub fn parse(text: &str) -> Option<LandState> {
        let flat: String = text.chars().filter(|c| *c != '\n').collect();
        let t = flat.trim_start();
        let mut it = t.splitn(2, char::is_whitespace);
        let state = it.next().unwrap_or("").to_string();
        if state.is_empty() {
            return None;
        }
        let rest = it.next().unwrap_or("").trim_start();
        let mut it = rest.splitn(2, char::is_whitespace);
        let tip = it.next().unwrap_or("").to_string();
        let rest = it.next().unwrap_or("").trim_start();
        let mut it = rest.splitn(2, char::is_whitespace);
        let at = it.next().unwrap_or("").parse().unwrap_or(0);
        let reason = it.next().unwrap_or("").trim().to_string();
        Some(LandState { state, tip, at, reason })
    }
}

/// `$SPIRA_RUN/submitted/<id>`: "<tip> <epoch> <state> <refreshes>\n" (lib.sh mark_submitted).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submitted {
    pub tip: String,
    pub at: u64,
    pub state: String,
    pub refreshes: u32,
}

impl Submitted {
    pub fn parse(text: &str) -> Option<Submitted> {
        let mut w = text.split_whitespace();
        let tip = w.next()?.to_string();
        let at = w.next()?.parse().ok()?;
        let state = w.next().unwrap_or("").to_string();
        let refreshes = w.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        Some(Submitted { tip, at, state, refreshes })
    }
    pub fn render(&self) -> String {
        format!("{} {} {} {}\n", self.tip, self.at, self.state, self.refreshes)
    }
    /// lib.sh `submitted <id> <tip>`: nothing more to do for this tip — the same tip, and
    /// not a failed submission (a failed one is retried, but not sooner than an hour).
    pub fn settles(&self, tip: &str, now: u64) -> bool {
        if self.tip != tip {
            return false;
        }
        if self.state != "failed" {
            return true;
        }
        now.saturating_sub(self.at) < 3600
    }
}

/// The gate's four outcomes (conf.sh).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateOutcome {
    Pass,
    Fail,
    BaseFail,
    NoVerdict,
}

pub const GATE_NOVERDICT: i32 = 75;
pub const GATE_BASEFAIL: i32 = 76;

impl GateOutcome {
    /// `spira_gate_outcome`.
    pub fn of(rc: i32) -> GateOutcome {
        match rc {
            0 => GateOutcome::Pass,
            GATE_NOVERDICT => GateOutcome::NoVerdict,
            GATE_BASEFAIL => GateOutcome::BaseFail,
            _ => GateOutcome::Fail,
        }
    }
    pub fn word(self) -> &'static str {
        match self {
            GateOutcome::Pass => "PASS",
            GateOutcome::Fail => "FAIL",
            GateOutcome::BaseFail => "BASE_FAIL",
            GateOutcome::NoVerdict => "NO_VERDICT",
        }
    }
    /// `spira_gate_blames_branch`: the one outcome that may reopen and charge.
    pub fn blames_branch(self) -> bool {
        self == GateOutcome::Fail
    }
}

/// One gate run: its status, its whole transcript (stdout and stderr together), and the
/// two fields read from the gate's own machine line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateRun {
    pub rc: i32,
    pub outcome: GateOutcome,
    pub out: String,
    /// `gate: VERDICT=X reason=R …` — the last one; None when the gate printed none.
    pub reason: Option<String>,
    /// `… suite=S …` — the last one; "-" when none.
    pub suite: String,
}

impl GateRun {
    pub fn parse(rc: i32, out: String) -> GateRun {
        let mut reason = None;
        let mut suite = None;
        for line in out.split('\n') {
            if let Some(rest) = line.strip_prefix("gate: VERDICT=") {
                // reason: VERDICT=[A-Z_]* immediately followed by " reason=".
                let word_end = rest.find(|c: char| !(c.is_ascii_uppercase() || c == '_')).unwrap_or(rest.len());
                if let Some(r) = rest[word_end..].strip_prefix(" reason=") {
                    let v: String = r.chars().take_while(|c| *c != ' ').collect();
                    reason = Some(v);
                }
                // suite: the last " suite=" on the line (sed's greedy .*).
                if let Some(i) = rest.rfind(" suite=") {
                    let v: String = rest[i + 7..].chars().take_while(|c| *c != ' ').collect();
                    suite = Some(v);
                }
            }
        }
        let suite = suite.filter(|s| !s.is_empty()).unwrap_or_else(|| "-".to_string());
        GateRun { rc, outcome: GateOutcome::of(rc), out, reason, suite }
    }
    pub fn reason_or(&self, dflt: &str) -> String {
        self.reason.clone().filter(|r| !r.is_empty()).unwrap_or_else(|| dflt.to_string())
    }
}

/// `landing.status`, rewritten on every exit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusFile {
    pub at: u64,
    pub rc: i32,
    pub branches: u64,
    pub moved: u64,
}

impl StatusFile {
    pub fn render(&self) -> String {
        format!(
            "SP_LAND_AT={}\nSP_LAND_RC={}\nSP_LAND_BRANCHES={}\nSP_LAND_MOVED={}\n",
            self.at, self.rc, self.branches, self.moved
        )
    }
}

/// `landing.run`: what a running pass is doing, for `halt`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    pub pid: String,
    pub started: String,
    pub repo: String,
    pub branch: String,
    pub phase: String,
}

impl RunRecord {
    pub fn parse(text: &str) -> RunRecord {
        let mut r = RunRecord::default();
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                match k {
                    "pid" => r.pid = v.into(),
                    "started" => r.started = v.into(),
                    "repo" => r.repo = v.into(),
                    "branch" => r.branch = v.into(),
                    "phase" => r.phase = v.into(),
                    _ => {}
                }
            }
        }
        r
    }
    pub fn render(&self) -> String {
        let mut s = format!("pid={}\nstarted={}\n", self.pid, self.started);
        if !self.repo.is_empty() {
            s.push_str(&format!("repo={}\n", self.repo));
        }
        if !self.branch.is_empty() {
            s.push_str(&format!("branch={}\n", self.branch));
        }
        if !self.phase.is_empty() {
            s.push_str(&format!("phase={}\n", self.phase));
        }
        s
    }
}

/// What `rebase_branch` reports through its globals.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rebase {
    pub ok: bool,
    /// conflict | rebase-refused | no-base | no-branch | no-worktree (empty on success).
    pub failure: String,
    pub conflicts: String,
    pub refused_reason: String,
}

/// What `recut_onto` reports.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recut {
    pub ok: bool,
    pub applied: u32,
    pub conflicts: String,
}
