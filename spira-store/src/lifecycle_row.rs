//! Every path that creates a bead creates its lifecycle row too: a bead with no
//! lifecycle row can never be claimed.

use lifecycle::bead::BeadEventKind;
use lifecycle::delivery::DeliveryEventKind;
use lifecycle::reason::ReturnedReason;

use crate::lc_call::{LcClient, LcError, Reply, CANNOT_TELL, LC_TIMEOUT, REFUSED};

pub const LC_BIN_ENV: &str = "SPIRA_LC_BIN";

pub fn lc_bin() -> Result<String, String> {
    match std::env::var(LC_BIN_ENV).ok().filter(|v| !v.is_empty()) {
        Some(v) => Ok(v),
        None => spira_config::release_env::tool("spira-lc"),
    }
}

/// An ask is its own lifecycle machine, never a bead row: `spira-lc create-ask` opens it
/// (idempotent), naming the work bead whose `ask` hold its close lifts.
pub fn create_ask_with(bin: &str, id: &str, work_bead: &str) -> Result<(), String> {
    let mut args = vec!["create-ask", id];
    if !work_bead.is_empty() {
        args.extend(["--work-bead", work_bead]);
    }
    lc_status(bin, &args)
}

pub fn create_ask(id: &str, work_bead: &str) -> Result<(), String> {
    create_ask_with(&lc_bin()?, id, work_bead)
}

/// How an ask closes (`spira-lc close-ask --exit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskExit {
    /// The operator's answer; `channel` names where it came from.
    Answered,
    /// Dismissed, the default executed.
    Default,
    /// Moot.
    Withdrawn,
}

impl AskExit {
    fn as_str(self) -> &'static str {
        match self {
            AskExit::Answered => "answered",
            AskExit::Default => "default",
            AskExit::Withdrawn => "withdrawn",
        }
    }
}

/// Close an ask on its own machine, recording who and the quoted words, and lift the `ask`
/// hold on the work bead it names. Exit 1 (the id is no ask: a legacy ask bead) is `Ok(false)`.
pub fn close_ask_with(bin: &str, id: &str, exit: AskExit, quote: &str, actor: &str, channel: &str) -> Result<bool, String> {
    let mut args = vec!["close-ask", id, "--exit", exit.as_str(), "--quote", quote, "--actor", actor];
    if !channel.is_empty() {
        args.extend(["--channel", channel]);
    }
    match run_lc(bin, &args)? {
        (0, _) => Ok(true),
        (1, _) => Ok(false),
        (code, msg) => Err(format!("{bin} close-ask {id} exited {code}: {msg}")),
    }
}

pub fn close_ask(id: &str, exit: AskExit, quote: &str, actor: &str, channel: &str) -> Result<bool, String> {
    close_ask_with(&lc_bin()?, id, exit, quote, actor, channel)
}

/// Whether `id` is an ask on the ask machine: the only authority on ask-ness (not a label).
pub fn is_ask_with(bin: &str, id: &str) -> Result<bool, String> {
    match run_lc(bin, &["show-ask", id])? {
        (0, _) => Ok(true),
        (1, _) => Ok(false),
        (code, msg) => Err(format!("{bin} show-ask {id} exited {code}: {msg}")),
    }
}

pub fn is_ask(id: &str) -> Result<bool, String> {
    is_ask_with(&lc_bin()?, id)
}

fn client(bin: &str) -> Result<LcClient, String> {
    // batch-job: a write through the CLI queues behind a loaded store; the socket keeps its 5 s.
    Ok(LcClient::for_bin(bin)?.within(LC_TIMEOUT))
}

fn run_lc(bin: &str, args: &[&str]) -> Result<(i32, String), String> {
    run_on(&client(bin)?, args)
}

fn run_on(c: &LcClient, args: &[&str]) -> Result<(i32, String), String> {
    let r = c.call(args)?;
    Ok((r.code, r.text()))
}

fn lc_status(bin: &str, args: &[&str]) -> Result<(), String> {
    status_on(&client(bin)?, args).map_err(|e| format!("{bin} {e}"))
}

fn status_on(c: &LcClient, args: &[&str]) -> Result<(), String> {
    match run_on(c, args)? {
        (0, _) => Ok(()),
        (code, msg) => Err(format!("{} exited {code}: {msg}", args.join(" "))),
    }
}

/// `express`/`unexpress` is a caller verb of the CLI; the socket serves its primitives, so a
/// socket client reads the row and sends the event itself, as the CLI does.
pub fn set_express_on(c: &LcClient, id: &str, on: bool) -> Result<(), String> {
    let verb = if on { "express" } else { "unexpress" };
    if !c.is_socket() {
        return status_on(c, &[verb, id]);
    }
    let kind = if on { BeadEventKind::Express } else { BeadEventKind::Unexpress };
    bead_event_on(c, id, &kind, "bead")
}

/// A caller verb is not served by the socket: read the row, then send the primitive event.
pub fn bead_event_on(c: &LcClient, id: &str, kind: &BeadEventKind, actor: &str) -> Result<(), String> {
    let shown = c.call(&["show", id])?;
    if shown.code != 0 {
        return Err(format!("{id}: no lifecycle row to mark: {}", shown.why()));
    }
    let v: serde_json::Value = serde_json::from_str(shown.out.trim()).map_err(|e| format!("{id}: show is not JSON: {e}"))?;
    let (state, version) = (json_field(&v["bead"], "state"), json_field(&v["bead"], "version"));
    if state.is_empty() || version.is_empty() {
        return Err(format!("{id}: no lifecycle row to mark"));
    }
    send_bead_event(c, id, &state, &version, kind, actor)
}

fn send_bead_event(c: &LcClient, id: &str, state: &str, version: &str, kind: &BeadEventKind, actor: &str) -> Result<(), String> {
    let kind = serde_json::to_string(kind).map_err(|e| e.to_string())?;
    status_on(c, &["event", "bead", id, "--expect", state, "--version", version, "--actor", actor, "--kind", &kind])
}

pub fn content_on_base_on(c: &LcClient, id: &str, proof: &str, actor: &str) -> Result<(), String> {
    bead_event_on(c, id, &BeadEventKind::ContentOnBase { proof: proof.into() }, actor)
}

/// What [`landed_record_on`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LandedRecord {
    Recorded,
    /// The row holds no landing to record; `state` is where it is.
    Nothing { state: String },
}

/// `close-on-land`'s record over the socket, which serves no such verb: `ContentOnBase` on a row
/// the builder has handed on and that has not landed or ended. Reaping is the caller's.
pub fn landed_record_on(c: &LcClient, id: &str, sha: &str, actor: &str) -> Result<LandedRecord, String> {
    let nothing = |state: &str| Ok(LandedRecord::Nothing { state: state.into() });
    let shown = c.call(&["show", id])?;
    match shown.code {
        0 => {}
        NO_ROW => return nothing("no row"),
        _ => return Err(format!("{id}: show exited {}: {}", shown.code, shown.why())),
    }
    let v: serde_json::Value = serde_json::from_str(shown.out.trim()).map_err(|e| format!("{id}: show is not JSON: {e}"))?;
    let (state, version) = (json_field(&v["bead"], "state"), json_field(&v["bead"], "version"));
    if state.is_empty() || version.is_empty() {
        return nothing("no row");
    }
    if !matches!(state.as_str(), "SUBMITTED" | "CERTIFIED" | "IN_DELIVERY") {
        return nothing(&state);
    }
    let sha = if sha.is_empty() { "unknown" } else { sha };
    let kind = BeadEventKind::ContentOnBase { proof: format!("landed:{sha}") };
    send_bead_event(c, id, &state, &version, &kind, actor).map(|()| LandedRecord::Recorded)
}

/// [`landed_record_on`], answering whether it wrote.
pub fn record_landed_on(c: &LcClient, id: &str, sha: &str, actor: &str) -> Result<bool, String> {
    landed_record_on(c, id, sha, actor).map(|r| r == LandedRecord::Recorded)
}

/// One exit of the delivery round, with the git proof its caller already computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// `content_on_base`: the branch's content is in the base (`merge-tree` proof, else `gh-merged`);
    /// `in_force`: the proof string (`merge-tree:<sha>` or `ancestry:<sha>`) when the change is in
    /// force on the base, which is all a row with no delivery round is landed on.
    PrMerged { merge_sha: String, content_on_base: bool, in_force: Option<String> },
    PrClosed,
    PushDelivered { sha: String },
    PushRequeued { tip: String },
    PushReturned,
}

impl Delivery {
    pub fn name(&self) -> &'static str {
        match self {
            Delivery::PrMerged { .. } => "pr-merged",
            Delivery::PrClosed => "pr-closed",
            Delivery::PushDelivered { .. } => "push-delivered",
            Delivery::PushRequeued { .. } => "push-requeued",
            Delivery::PushReturned => "push-returned",
        }
    }

    pub fn actor(&self) -> &'static str {
        match self {
            Delivery::PrMerged { .. } | Delivery::PrClosed => "pr-pass-branch",
            _ => "landing.sh",
        }
    }

    fn wanted(&self) -> &'static str {
        match self {
            Delivery::PrMerged { .. } | Delivery::PrClosed => "PR_OPEN",
            _ => "PUSHING",
        }
    }

    fn kind(&self) -> DeliveryEventKind {
        match self {
            Delivery::PrMerged { merge_sha, content_on_base, .. } => DeliveryEventKind::Delivered { merge_sha: merge_sha.clone(), proof: if *content_on_base { "merge-tree" } else { "gh-merged" }.into() },
            Delivery::PrClosed => DeliveryEventKind::Returned { reason: ReturnedReason::PrClosedUnmerged },
            Delivery::PushDelivered { sha } => DeliveryEventKind::Delivered { merge_sha: sha.clone(), proof: "ancestry".into() },
            Delivery::PushRequeued { tip } => DeliveryEventKind::Requeued { tip: tip.clone() },
            Delivery::PushReturned => DeliveryEventKind::Returned { reason: ReturnedReason::PushRejected },
        }
    }
}

/// Why a delivery exit wrote nothing: the row is not where the exit applies, which is not a failed store call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotRecorded {
    NoRow,
    NoDeliveryRow,
    DeliveryNotIn { state: String, want: &'static str },
    NotLive { state: String },
    NotCertified { state: String },
    ProofNotInForce,
}

impl std::fmt::Display for NotRecorded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotRecorded::NoRow => write!(f, "no lifecycle row"),
            NotRecorded::NoDeliveryRow => write!(f, "no delivery row"),
            NotRecorded::DeliveryNotIn { state, want } => write!(f, "delivery is {state}, not {want}"),
            NotRecorded::NotLive { state } => write!(f, "the row is {state}, not a live one"),
            NotRecorded::NotCertified { state } => write!(f, "the row is {state}, not CERTIFIED"),
            NotRecorded::ProofNotInForce => write!(f, "its change is not in force on the landing ref"),
        }
    }
}

/// The one primitive event a delivery exit sends, or why it sends none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Delivery { expect: &'static str, version: String, kind: DeliveryEventKind },
    Bead { expect: String, version: String, kind: BeadEventKind },
    Skip(NotRecorded),
}

fn json_field(v: &serde_json::Value, k: &str) -> String {
    match &v[k] {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// The rules of `deliver`, over the JSON `show` answers; the CLI verb and the socket client both apply them.
pub fn plan_delivery(d: &Delivery, shown: &serde_json::Value) -> Step {
    let bead = &shown["bead"];
    let (state, version) = (json_field(bead, "state"), json_field(bead, "version"));
    if state.is_empty() || version.is_empty() {
        return Step::Skip(NotRecorded::NoRow);
    }
    let row = &shown["delivery"];
    if row.is_object() {
        let want = d.wanted();
        let at = json_field(row, "state");
        if at != want {
            return Step::Skip(NotRecorded::DeliveryNotIn { state: at, want });
        }
        let version = match &row["version"] {
            serde_json::Value::Null => "0".to_string(),
            _ => json_field(row, "version"),
        };
        return Step::Delivery { expect: want, version, kind: d.kind() };
    }
    let on_base = |proof: String| Step::Bead { expect: state.clone(), version: version.clone(), kind: BeadEventKind::ContentOnBase { proof } };
    match d {
        Delivery::PrMerged { in_force, .. } => {
            if matches!(state.as_str(), "LANDED" | "SUPERSEDED" | "DROPPED" | "DONE") {
                return Step::Skip(NotRecorded::NotLive { state });
            }
            match in_force {
                Some(proof) => on_base(proof.clone()),
                None => Step::Skip(NotRecorded::ProofNotInForce),
            }
        }
        Delivery::PushDelivered { sha } if state == "CERTIFIED" => on_base(format!("ancestry:{sha}")),
        Delivery::PushDelivered { .. } => Step::Skip(NotRecorded::NotCertified { state }),
        _ => Step::Skip(NotRecorded::NoDeliveryRow),
    }
}

const NO_ROW: i32 = 1;

/// What `deliver_on` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivered {
    Recorded,
    NotRecorded(NotRecorded),
}

/// A delivery exit over the socket, which serves no `deliver` verb: read the row, then send the
/// primitive event the verb's rules give. A refused or unreachable event is an error.
pub fn deliver_on(c: &LcClient, id: &str, d: &Delivery, actor: &str) -> Result<Delivered, String> {
    let shown = c.call(&["show", id])?;
    match shown.code {
        0 => {}
        NO_ROW => return Ok(Delivered::NotRecorded(NotRecorded::NoRow)),
        _ => return Err(format!("{id}: show exited {}: {}", shown.code, shown.why())),
    }
    let v: serde_json::Value = serde_json::from_str(shown.out.trim()).map_err(|e| format!("{id}: show is not JSON: {e}"))?;
    let send = |machine: &str, expect: &str, version: &str, kind: String| {
        status_on(c, &["event", machine, id, "--expect", expect, "--version", version, "--actor", actor, "--kind", &kind]).map(|()| Delivered::Recorded)
    };
    match plan_delivery(d, &v) {
        Step::Delivery { expect, version, kind } => send("delivery", expect, &version, serde_json::to_string(&kind).map_err(|e| e.to_string())?),
        Step::Bead { expect, version, kind } => send("bead", &expect, &version, serde_json::to_string(&kind).map_err(|e| e.to_string())?),
        Step::Skip(why) => Ok(Delivered::NotRecorded(why)),
    }
}

/// The id `bd create` printed: `--silent` prints the bare id, `--json` an object (or a
/// one-element array) with `id`.
pub fn created_id(stdout: &str) -> Option<String> {
    let text = stdout.trim();
    if text.starts_with('{') || text.starts_with('[') {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        let obj = if v.is_array() { v.get(0)? } else { &v };
        return obj.get("id").and_then(|i| i.as_str()).map(String::from);
    }
    let last = text.lines().rev().find(|l| !l.trim().is_empty())?.trim();
    (!last.contains(char::is_whitespace)).then(|| last.to_string())
}

/// Every id a bead-creating verb printed, as JSON: `import` lists `ids`, `mol pour|wisp` an
/// `id_mapping` of proto to new ids, `mol bond` a `result_id`. A bare id is accepted as `created_id` does.
pub fn created_ids(stdout: &str) -> Vec<String> {
    let text = stdout.trim();
    if !text.starts_with('{') {
        return created_id(text).into_iter().collect();
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return Vec::new() };
    let mut ids: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        if !ids.iter().any(|i| i == s) {
            ids.push(s.to_string());
        }
    };
    if let Some(a) = v.get("ids").and_then(|a| a.as_array()) {
        a.iter().filter_map(|i| i.as_str()).for_each(&mut push);
    }
    if let Some(i) = v.get("new_epic_id").and_then(|i| i.as_str()) {
        push(i);
    }
    if let Some(m) = v.get("id_mapping").and_then(|m| m.as_object()) {
        m.values().filter_map(|i| i.as_str()).for_each(&mut push);
    }
    if let Some(i) = v.get("result_id").and_then(|i| i.as_str()) {
        push(i);
    }
    if ids.is_empty() {
        return created_id(text).into_iter().collect();
    }
    ids
}

pub fn ensure_rows_with(bin: &str, ids: &[String]) -> Result<(), String> {
    let errs: Vec<String> = ids.iter().filter_map(|id| ensure_row_with(bin, id, None).err()).collect();
    if errs.is_empty() { Ok(()) } else { Err(errs.join("; ")) }
}

/// `create-bead <id> --ready`, naming `repo` when the create's labels did: a row that already
/// exists is left as it is, so the repo must be sent the first time.
fn create_row_args<'a>(id: &'a str, repo: Option<&'a str>) -> Vec<&'a str> {
    let mut args = vec!["create-bead", id, "--ready"];
    args.extend(repo.into_iter().flat_map(|r| ["--repo", r]));
    args
}

/// `Ok(())`: the row exists now.
pub fn ensure_row_with(bin: &str, id: &str, repo: Option<&str>) -> Result<(), String> {
    lc_status(bin, &create_row_args(id, repo))
}

/// Record `Express`/`Unexpress` on the bead's lifecycle row: express is lifecycle state.
pub fn set_express_with(bin: &str, id: &str, on: bool) -> Result<(), String> {
    set_express_on(&client(bin)?, id, on).map_err(|e| format!("{bin} {e}"))
}

pub fn set_express(id: &str, on: bool) -> Result<(), String> {
    set_express_with(&lc_bin()?, id, on)
}

/// Filing has finished: the row born OPEN before the bead's edges were written becomes READY.
pub fn promote_with(bin: &str, id: &str) -> Result<(), String> {
    lc_status(bin, &["promote-filed", id])
}

pub fn promote(id: &str) -> Result<(), String> {
    promote_with(&lc_bin()?, id)
}

fn row_on(c: &LcClient, id: &str, repo: Option<&str>, retry: &Retry) -> Result<(), String> {
    write_idempotent(c, &create_row_args(id, repo), retry)
}

pub fn ensure_row(id: &str, repo: Option<&str>) -> Result<(), String> {
    row_on(&client(&lc_bin()?)?, id, repo, &Retry::configured()?)
}

/// Config key: seconds an idempotent row write is retried through a store that cannot tell.
pub const IDEMPOTENT_RETRY_KEY: &str = "SPIRA_LC_IDEMPOTENT_RETRY_SECS";

const BACKOFF_START: std::time::Duration = std::time::Duration::from_millis(250);
const BACKOFF_CAP: std::time::Duration = std::time::Duration::from_secs(5);

/// The clock a retry reads and sleeps on; a test swaps in a virtual one.
pub trait Pace: Send + Sync {
    fn now(&self) -> std::time::Duration;
    fn sleep(&self, d: std::time::Duration);
}

struct RealPace(std::time::Instant);

impl Pace for RealPace {
    fn now(&self) -> std::time::Duration {
        self.0.elapsed()
    }
    fn sleep(&self, d: std::time::Duration) {
        std::thread::sleep(d);
    }
}

/// How long an idempotent write is retried: until `bound` has passed, whatever the attempts.
pub struct Retry {
    bound: std::time::Duration,
    pace: std::sync::Arc<dyn Pace>,
}

impl Retry {
    pub fn within(bound: std::time::Duration) -> Self {
        Retry { bound, pace: std::sync::Arc::new(RealPace(std::time::Instant::now())) }
    }

    /// One attempt, no retry.
    pub fn once() -> Self {
        Self::within(std::time::Duration::ZERO)
    }

    /// The bound the configuration declares.
    pub fn configured() -> Result<Self, String> {
        let secs: u32 = spira_config::process::cfg_parse(IDEMPOTENT_RETRY_KEY)?;
        Ok(Self::within(std::time::Duration::from_secs(u64::from(secs))))
    }

    pub fn with_pace(mut self, pace: std::sync::Arc<dyn Pace>) -> Self {
        self.pace = pace;
        self
    }
}

/// Sends a request until the store answers with anything but "cannot tell", and returns that
/// reply. "Cannot tell" and no answer are sent again until `retry`'s bound runs out; a refusal
/// is final, since asking again cannot change it. The resend is safe only when the write is
/// idempotent or carries a request id the store dedupes on.
pub fn call_idempotent<S: AsRef<str>>(c: &LcClient, argv: &[S], stdin: Option<&str>, retry: &Retry) -> Result<Reply, String> {
    call_idempotent_noting(c, argv, stdin, retry, &mut || {})
}

/// [`call_idempotent`], calling `resent` before each send after the first.
pub fn call_idempotent_noting<S: AsRef<str>>(c: &LcClient, argv: &[S], stdin: Option<&str>, retry: &Retry, resent: &mut dyn FnMut()) -> Result<Reply, String> {
    let said = argv.iter().map(AsRef::as_ref).collect::<Vec<_>>().join(" ");
    let start = retry.pace.now();
    let mut backoff = BACKOFF_START;
    let mut first = true;
    loop {
        if !first {
            resent();
        }
        first = false;
        let sent = match stdin {
            Some(b) => c.call_stdin(argv, b),
            None => c.call(argv),
        };
        let unanswered = match sent {
            Ok(r) if r.code == CANNOT_TELL => format!("{said} exited {}: {}", r.code, r.text()),
            Ok(r) => return Ok(r),
            Err(LcError::Refused(m)) => return Err(m),
            Err(e) => e.to_string(),
        };
        if retry.pace.now().saturating_sub(start) + backoff > retry.bound {
            return Err(unanswered);
        }
        retry.pace.sleep(backoff);
        backoff = (backoff * 2).min(BACKOFF_CAP);
    }
}

/// Sends an idempotent write until the store applies it or `retry`'s bound runs out; any
/// answer but "cannot tell" ends it, and only exit 0 is success.
pub fn write_idempotent<S: AsRef<str>>(c: &LcClient, argv: &[S], retry: &Retry) -> Result<(), String> {
    let r = call_idempotent(c, argv, None, retry)?;
    if r.code == 0 {
        return Ok(());
    }
    let said = argv.iter().map(AsRef::as_ref).collect::<Vec<_>>().join(" ");
    Err(format!("{said} exited {}: {}", r.code, r.text()))
}

/// Call after a successful `bd create` whose stdout is `created_stdout`. A failure is
/// reported on stderr and returned; the bead exists, so the caller must not retry the create.
pub fn after_create(who: &str, created_stdout: &str) -> Result<(), String> {
    let Some(id) = created_id(created_stdout) else {
        let e = format!("cannot read the created bead's id from {created_stdout:?}");
        eprintln!("{who}: LIFECYCLE: {e}; the new bead has no lifecycle row and cannot be claimed");
        return Err(e);
    };
    ensure_row(&id, None).map_err(|e| {
        eprintln!("{who}: LIFECYCLE: {id} was created but has NO lifecycle row and cannot be claimed: {e}");
        e
    })
}

/// The bead a `bd close <id> ...` argv closes: the first operand, when it is one.
pub fn closed_id(args: &[String]) -> Option<&str> {
    match (args.first().map(String::as_str), args.get(1)) {
        (Some("close"), Some(id)) if !id.starts_with('-') => Some(id),
        _ => None,
    }
}

/// Move the bead's lifecycle row to the terminal state its bd close earned
/// (`spira-lc reconcile-closed`), so a closed bead never stays READY and claimable.
pub fn terminalize_closed_with(bin: &str, id: &str) -> Result<(), String> {
    lc_status(bin, &["reconcile-closed", "--apply", id])
}

/// Call after a successful `bd close`. A failure is reported and returned; the bead is
/// closed, so the caller must not retry the close.
pub fn after_close(who: &str, args: &[String]) -> Result<(), String> {
    let Some(id) = closed_id(args) else { return Ok(()) };
    terminalize_closed_with(&lc_bin()?, id).map_err(|e| {
        eprintln!("{who}: LIFECYCLE: {id} was closed but its lifecycle row is not terminal and may read as claimable: {e}");
        e
    })
}

/// Close a bead through the lifecycle machine (`spira-lc close`, sp-3fue0j): the row's end is
/// recorded first, then the store is closed. Every Rust closer calls this rather than handing
/// bd a `close` (lifecycle-guard's `bd-close-rust` refuses that at the gate). `actor` names who
/// closed it on the event; `superseded_by` names a successor. The reason goes on stdin, so
/// any length or quoting survives.
pub fn close_with(bin: &str, id: &str, reason: &str, actor: &str, superseded_by: Option<&str>) -> Result<(), String> {
    close_args(bin, id, reason, actor, superseded_by.filter(|b| !b.is_empty()).map(|b| vec!["--superseded-by", b]).unwrap_or_default())
}

/// The landing path's close (`spira-lc close --landing`): the delivery records LANDED on the
/// row itself, so this writes no event; bd closes once the row reads LANDED, and spira-lc
/// refuses it for a row not in delivery.
pub fn close_landed(id: &str, reason: &str, actor: &str) -> Result<(), String> {
    close_args(&lc_bin()?, id, reason, actor, vec!["--landing"])
}

fn close_args(bin: &str, id: &str, reason: &str, actor: &str, extra: Vec<&str>) -> Result<(), String> {
    let c = client(bin)?;
    close_on(&c, id, reason, actor, &extra).map_err(|e| match e {
        LcError::Refused(m) => format!("{bin} close {id} exited {REFUSED}: {m}"),
        LcError::Fault(m) | LcError::ReadEnded(m) | LcError::NotRunning(m) => format!("{bin} close {id}: {m}"),
    })
}

/// A close, typed: a refusal (exit 3) and a fault (anything else, or no answer) are variants.
pub fn close_on(c: &LcClient, id: &str, reason: &str, actor: &str, extra: &[&str]) -> Result<(), LcError> {
    let mut args = vec!["close", id, "--reason-file", "-", "--actor", actor];
    args.extend(extra);
    let r = c.call_stdin(&args, reason)?;
    match r.code {
        0 => Ok(()),
        REFUSED => Err(LcError::Refused(r.text())),
        _ => Err(LcError::Fault(format!("exited {}: {}", r.code, r.text()))),
    }
}

/// A close by `actor` through the configured `spira-lc`, typed: the caller reads refusal from the variant.
pub fn close_as(id: &str, reason: &str, actor: &str) -> Result<(), LcError> {
    let c = lc_bin().and_then(|b| client(&b)).map_err(LcError::Fault)?;
    close_on(&c, id, reason, actor, &[])
}

/// [`close_with`] through the configured `spira-lc`.
pub fn close(id: &str, reason: &str, actor: &str, superseded_by: Option<&str>) -> Result<(), String> {
    close_with(&lc_bin()?, id, reason, actor, superseded_by)
}

/// Return a bead to its builder through the lifecycle machine (`spira-lc reopen`, sp-swh8b8):
/// the row records the event its own state implies. No row (exit 1) is nothing to reopen and is
/// `Ok`; a terminal row (3) or an unreachable machine (2) is an error. Every Rust reopener
/// calls this rather than handing bd a `reopen` (lifecycle-guard's `bd-reopen-rust`).
pub fn reopen_with(bin: &str, id: &str, cause: &str, actor: &str) -> Result<(), String> {
    reopen_on(&client(bin)?, bin, id, cause, actor)
}

fn reopen_on(c: &LcClient, bin: &str, id: &str, cause: &str, actor: &str) -> Result<(), String> {
    match run_on(c, &["reopen", id, cause, actor])? {
        (0 | 1, _) => Ok(()),
        (2, msg) => Err(format!("spira-lc could not tell whether {id} was reopened (exit 2: {msg})")),
        (3, msg) => Err(format!("spira-lc refused the reopen of {id} (exit 3: {msg})")),
        (code, msg) => Err(format!("{bin} reopen {id} exited {code}: {msg}")),
    }
}

/// Point the bead's lifecycle row at `repo`: the row is what a claim reads to pick a checkout.
pub fn set_repo_with(bin: &str, id: &str, repo: &str) -> Result<(), String> {
    lc_status(bin, &["set-repo", id, repo])
}

pub fn set_repo(id: &str, repo: &str) -> Result<(), String> {
    set_repo_with(&lc_bin()?, id, repo)
}

/// [`reopen_with`] through the configured `spira-lc`.
pub fn reopen(id: &str, cause: &str, actor: &str) -> Result<(), String> {
    reopen_with(&lc_bin()?, id, cause, actor)
}

/// `after_create` for a verb that may create many beads (`import`, `mol pour|wisp|bond`),
/// which must have been run with `--json`. `repo` is the create's `repo:` label, if any.
pub fn after_create_ids(who: &str, created_stdout: &str, repo: Option<&str>) -> Result<(), String> {
    let c = client(&lc_bin()?)?;
    after_create_ids_on(&c, who, created_stdout, repo, &Retry::configured()?)
}

/// [`after_create_ids`] through `c`, so a test can run it against an in-process service.
pub fn after_create_ids_on(c: &LcClient, who: &str, created_stdout: &str, repo: Option<&str>, retry: &Retry) -> Result<(), String> {
    let ids = created_ids(created_stdout);
    if ids.is_empty() {
        let e = format!("cannot read any created bead id from {created_stdout:?}");
        eprintln!("{who}: LIFECYCLE: {e}; the new beads have no lifecycle row and cannot be claimed");
        return Err(e);
    }
    let errs: Vec<String> = ids.iter().filter_map(|id| row_on(c, id, repo, retry).err()).collect();
    if errs.is_empty() {
        return Ok(());
    }
    let e = errs.join("; ");
    eprintln!("{who}: LIFECYCLE: some created beads have NO lifecycle row and cannot be claimed: {e}");
    Err(e)
}

/// Writes the claim inputs of each of `ids` onto its lifecycle record (`spira-lc
/// sync-claim-inputs`). A failure is reported with its repair and returned; the bd write it
/// follows stands.
pub fn sync_claim_inputs_on(c: &LcClient, who: &str, ids: &[String], retry: &Retry) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut argv = vec!["sync-claim-inputs"];
    argv.extend(ids.iter().map(String::as_str));
    write_idempotent(c, &argv, retry).inspect_err(|failed| {
        eprintln!("{who}: LIFECYCLE: {failed}; claim inputs not written; repair: spira-lc sync-claim-inputs {}", ids.join(" "));
    })
}

/// [`sync_claim_inputs_on`] through the configured `spira-lc`.
pub fn sync_claim_inputs(who: &str, ids: &[String]) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    let c = client(&lc_bin()?)?;
    sync_claim_inputs_on(&c, who, ids, &Retry::configured()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lc_call::{Reply, Transport};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct Virtual(Mutex<Duration>);

    impl Pace for Virtual {
        fn now(&self) -> Duration {
            *self.0.lock().unwrap()
        }
        fn sleep(&self, d: Duration) {
            *self.0.lock().unwrap() += d;
        }
    }

    /// Answers each call from `script`, repeating its last entry; every attempt costs the
    /// 5 s deadline a real one would, on the virtual clock.
    struct Scripted {
        script: Vec<i32>,
        seen: Mutex<Vec<Vec<String>>>,
        bodies: Mutex<Vec<Option<String>>>,
        pace: Arc<Virtual>,
    }

    impl Transport for Scripted {
        fn exchange(&self, argv: &[String], stdin: Option<&str>, _d: Duration) -> Result<Reply, String> {
            self.bodies.lock().unwrap().push(stdin.map(str::to_string));
            let mut seen = self.seen.lock().unwrap();
            seen.push(argv.to_vec());
            *self.pace.0.lock().unwrap() += Duration::from_secs(5);
            let code = *self.script.get(seen.len() - 1).or(self.script.last()).unwrap();
            let out = if code == 2 { "cannot tell: deadline" } else { "" };
            Ok(Reply { code, out: out.into(), err: String::new(), read_ended: false })
        }
    }

    fn scripted(script: &[i32], bound_secs: u64) -> (LcClient, Arc<Scripted>, Retry) {
        let pace = Arc::new(Virtual(Mutex::new(Duration::ZERO)));
        let t = Arc::new(Scripted { script: script.to_vec(), seen: Mutex::new(Vec::new()), bodies: Mutex::new(Vec::new()), pace: pace.clone() });
        (LcClient::over(t.clone(), true), t, Retry::within(Duration::from_secs(bound_secs)).with_pace(pace))
    }

    fn sent(t: &Scripted) -> usize {
        t.seen.lock().unwrap().len()
    }

    #[test_id(tsc1cfv)]
    #[test]
    fn reopen_says_could_not_tell_for_exit_2_and_refused_for_exit_3() {
        let (c, _, _) = scripted(&[2], 5);
        let e = reopen_on(&c, "spira-lc", "sp-x", "eject", "queue").unwrap_err();
        assert!(e.contains("could not tell") && !e.contains("refused"), "{e}");
        let (c, _, _) = scripted(&[3], 5);
        let e = reopen_on(&c, "spira-lc", "sp-x", "eject", "queue").unwrap_err();
        assert!(e.contains("refused the reopen of sp-x") && !e.contains("could not tell"), "{e}");
    }

    #[test_id(tepeo3d)]
    #[test]
    fn call_idempotent_resends_a_cannot_tell_and_returns_the_first_applied_reply() {
        let (c, t, retry) = scripted(&[2, 2, 0], 60);
        let mut resends = 0;
        let r = call_idempotent_noting(&c, &["content", "create"], Some("body"), &retry, &mut || resends += 1).unwrap();
        assert_eq!((r.code, sent(&t), resends), (0, 3, 2));
        assert_eq!(*t.bodies.lock().unwrap(), vec![Some("body".to_string()); 3]);
    }

    #[test_id(t4gqy3j)]
    #[test]
    fn call_idempotent_returns_a_refusal_reply_without_resending() {
        let (c, t, retry) = scripted(&[3], 60);
        assert_eq!(call_idempotent(&c, &["content", "create"], None, &retry).unwrap().code, 3);
        assert_eq!(sent(&t), 1);
    }

    #[test_id(tcckduv)]
    #[test]
    fn call_idempotent_stops_at_the_bound() {
        let (c, t, retry) = scripted(&[2], 12);
        let e = call_idempotent(&c, &["content", "create"], None, &retry).unwrap_err();
        assert!(e.contains("cannot tell"), "{e}");
        assert_eq!(sent(&t), 3, "sends at 0 s, 5.25 s and 10.75 s; the next backoff passes 12 s");
    }

    #[test_id(tvrz8s7)]
    #[test]
    fn a_create_the_store_cannot_tell_twice_is_sent_a_third_time_and_succeeds() {
        let (c, t, retry) = scripted(&[2, 2, 0], 60);
        assert_eq!(row_on(&c, "sp-1", None, &retry), Ok(()));
        assert_eq!(sent(&t), 3);
        assert_eq!(t.seen.lock().unwrap()[2], ["create-bead", "sp-1", "--ready"]);
    }

    #[test_id(tqxuypd)]
    #[test]
    fn a_store_down_for_longer_than_three_attempts_is_waited_out_within_the_bound() {
        let (c, t, retry) = scripted(&[2, 2, 2, 2, 2, 0], 60);
        assert_eq!(row_on(&c, "sp-1", None, &retry), Ok(()));
        assert_eq!(sent(&t), 6);
    }

    #[test_id(tgypl65)]
    #[test]
    fn a_store_that_never_answers_reports_after_the_bound_and_not_before() {
        let (c, t, retry) = scripted(&[2], 60);
        let e = row_on(&c, "sp-1", None, &retry).unwrap_err();
        assert!(e.contains("cannot tell"), "{e}");
        assert!(sent(&t) > 3, "{} attempts", sent(&t));
        assert!(t.pace.now() <= Duration::from_secs(60 + 5), "never runs far past the bound");
    }

    #[test_id(tlt55zt)]
    #[test]
    fn a_bound_of_zero_makes_one_attempt() {
        let (c, t, retry) = scripted(&[2, 0], 0);
        assert!(row_on(&c, "sp-1", None, &retry).is_err());
        assert_eq!(sent(&t), 1);
    }

    #[test_id(tmyas8u)]
    #[test]
    fn a_refusal_is_final_and_sent_once() {
        for code in [1, 3] {
            let (c, t, retry) = scripted(&[code, 0], 60);
            assert!(row_on(&c, "sp-1", None, &retry).is_err());
            assert_eq!(sent(&t), 1, "exit {code}");
        }
    }

    #[test_id(tu9nano)]
    #[test]
    fn claim_inputs_are_retried_the_same_way() {
        let (c, t, retry) = scripted(&[2, 2, 0], 60);
        assert_eq!(sync_claim_inputs_on(&c, "t", &["sp-1".to_string()], &retry), Ok(()));
        assert_eq!(sent(&t), 3);
    }


    /// The store as the real socket answers it: `deliver` is refused with usage, `show` is the row.
    struct Socket {
        row: String,
        event: i32,
        seen: Mutex<Vec<String>>,
    }

    impl Transport for Socket {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: Duration) -> Result<Reply, String> {
            self.seen.lock().unwrap().push(argv.join(" "));
            let (code, out) = match argv[0].as_str() {
                "show" if self.row.is_empty() => (1, String::new()),
                "show" => (0, self.row.clone()),
                "event" => (self.event, if self.event == 0 { String::new() } else { "refused".into() }),
                _ => (2, "usage: spira-lc show <bead-id> | …".into()),
            };
            Ok(Reply { code, out, err: String::new(), read_ended: false })
        }
    }

    fn row(bead: &str, delivery: Option<&str>) -> String {
        let d = delivery.map_or("null".to_string(), |s| format!(r#"{{"state":"{s}","version":5}}"#));
        format!(r#"{{"bead":{{"state":"{bead}","version":12}},"delivery":{d}}}"#)
    }

    fn delivered_via(row: String, event: i32, d: &Delivery) -> (Result<Delivered, String>, Vec<String>) {
        let t = Arc::new(Socket { row, event, seen: Mutex::new(Vec::new()) });
        let r = deliver_on(&LcClient::over(t.clone(), true), "sp-a", d, d.actor());
        let seen = t.seen.lock().unwrap().clone();
        (r, seen)
    }

    fn merged(in_force: Option<&str>) -> Delivery {
        Delivery::PrMerged { merge_sha: "m1".into(), content_on_base: true, in_force: in_force.map(String::from) }
    }

    
    #[test_id(tqsnxxz)]
    #[test]
    fn each_delivery_exit_sends_its_event_on_a_delivery_row_in_its_wanted_state() {
        let exits = [
            (merged(None), "PR_OPEN", "pr-pass-branch", r#"{"Delivered":{"merge_sha":"m1","proof":"merge-tree"}}"#),
            (Delivery::PrClosed, "PR_OPEN", "pr-pass-branch", r#"{"Returned":{"reason":"pr-closed-unmerged"}}"#),
            (Delivery::PushDelivered { sha: "s1".into() }, "PUSHING", "landing.sh", r#"{"Delivered":{"merge_sha":"s1","proof":"ancestry"}}"#),
            (Delivery::PushRequeued { tip: "t1".into() }, "PUSHING", "landing.sh", r#"{"Requeued":{"tip":"t1"}}"#),
            (Delivery::PushReturned, "PUSHING", "landing.sh", r#"{"Returned":{"reason":"push-rejected"}}"#),
        ];
        for (d, want, actor, kind) in exits {
            let (r, seen) = delivered_via(row("SUBMITTED", Some(want)), 0, &d);
            assert_eq!(r, Ok(Delivered::Recorded), "{d:?}");
            assert_eq!(seen[1], format!("event delivery sp-a --expect {want} --version 5 --actor {actor} --kind {kind}"), "{d:?}");
            assert!(seen.iter().all(|l| !l.starts_with("deliver")));
        }
    }

    
    #[test_id(tyiqjov)]
    #[test]
    fn a_pr_with_no_delivery_row_lands_the_bead_on_proof_and_a_push_only_when_certified() {
        let (r, seen) = delivered_via(row("SUBMITTED", None), 0, &merged(Some("merge-tree:m1")));
        assert_eq!(r, Ok(Delivered::Recorded));
        assert_eq!(seen[1], r#"event bead sp-a --expect SUBMITTED --version 12 --actor pr-pass-branch --kind {"ContentOnBase":{"proof":"merge-tree:m1"}}"#);
        let (r, seen) = delivered_via(row("CERTIFIED", None), 0, &Delivery::PushDelivered { sha: "s1".into() });
        assert_eq!(r, Ok(Delivered::Recorded));
        assert_eq!(seen[1], r#"event bead sp-a --expect CERTIFIED --version 12 --actor landing.sh --kind {"ContentOnBase":{"proof":"ancestry:s1"}}"#);
    }

    
    #[test_id(taj0vey)]
    #[test]
    fn a_terminal_row_an_unproved_change_and_the_ordinary_no_row_case_send_no_event() {
        let cases = [
            (row("LANDED", None), merged(Some("merge-tree:m1")), NotRecorded::NotLive { state: "LANDED".into() }),
            (row("SUBMITTED", None), merged(None), NotRecorded::ProofNotInForce),
            (row("SUBMITTED", None), Delivery::PushDelivered { sha: "s".into() }, NotRecorded::NotCertified { state: "SUBMITTED".into() }),
            (row("SUBMITTED", None), Delivery::PrClosed, NotRecorded::NoDeliveryRow),
            (row("SUBMITTED", Some("QUEUED")), Delivery::PrClosed, NotRecorded::DeliveryNotIn { state: "QUEUED".into(), want: "PR_OPEN" }),
            (String::new(), Delivery::PrClosed, NotRecorded::NoRow),
        ];
        for (shown, d, why) in cases {
            let (r, seen) = delivered_via(shown, 0, &d);
            assert_eq!(r, Ok(Delivered::NotRecorded(why)), "{d:?}");
            assert_eq!(seen, vec!["show sp-a"], "{d:?}");
        }
    }

    
    #[test_id(tpa643h)]
    #[test]
    fn a_refused_event_is_an_error_not_a_quiet_miss() {
        let (r, _) = delivered_via(row("SUBMITTED", Some("PR_OPEN")), 3, &Delivery::PrClosed);
        assert!(r.unwrap_err().contains("refused"));
    }

    fn landed_via(row: String, sha: &str) -> (Result<bool, String>, Vec<String>) {
        let t = Arc::new(Socket { row, event: 0, seen: Mutex::new(Vec::new()) });
        let r = record_landed_on(&LcClient::over(t.clone(), true), "sp-a", sha, "hel");
        let seen = t.seen.lock().unwrap().clone();
        (r, seen)
    }

    #[test_id(tly8w45)]
    #[test]
    fn a_landing_is_recorded_only_on_a_row_the_builder_has_handed_on_and_that_has_not_ended() {
        for (state, sha, proof) in [("SUBMITTED", "abc", "landed:abc"), ("CERTIFIED", "abc", "landed:abc"), ("IN_DELIVERY", "", "landed:unknown")] {
            let (r, seen) = landed_via(row(state, None), sha);
            assert_eq!(r, Ok(true), "{state}");
            let event = format!(r#"event bead sp-a --expect {state} --version 12 --actor hel --kind {{"ContentOnBase":{{"proof":"{proof}"}}}}"#);
            assert_eq!(seen, vec!["show sp-a".to_string(), event], "{state}");
        }
        for shown in [row("LANDED", None), row("READY", None), row("DROPPED", None), String::new()] {
            let (r, seen) = landed_via(shown.clone(), "abc");
            assert_eq!(r, Ok(false), "{shown}");
            assert_eq!(seen, vec!["show sp-a"], "{shown}");
        }
    }

    #[test_id(tu4p3nx)]
    #[test]
    fn created_id_reads_silent_and_json() {
        assert_eq!(created_id("sp-abc\n").as_deref(), Some("sp-abc"));
        assert_eq!(created_id("{\"id\":\"sp-j\",\"title\":\"t\"}").as_deref(), Some("sp-j"));
        assert_eq!(created_id("[{\"id\":\"sp-k\"}]").as_deref(), Some("sp-k"));
        assert_eq!(created_id("  \n"), None);
        assert_eq!(created_id("warning: something odd"), None);
    }

    #[test_id(tflmn0v)]
    #[test]
    fn created_ids_reads_import_and_mol_json() {
        let import = "{\"created\":2,\"ids\":[\"sp-a\",\"sp-b\"],\"skipped\":0}";
        assert_eq!(created_ids(import), vec!["sp-a", "sp-b"]);
        let pour = "{\"created\":2,\"id_mapping\":{\"p\":\"sp-mol-1\",\"p.c\":\"sp-mol-1.c\"},\"new_epic_id\":\"sp-mol-1\"}";
        assert_eq!(created_ids(pour), vec!["sp-mol-1", "sp-mol-1.c"]);
        let bond = "{\"result_id\":\"sp-x\",\"result_type\":\"compound_molecule\"}";
        assert_eq!(created_ids(bond), vec!["sp-x"]);
        assert_eq!(created_ids("sp-abc\n"), vec!["sp-abc"]);
        assert!(created_ids("{\"error\":\"boom\"}").is_empty());
        assert!(created_ids("Imported 2 issues").is_empty());
    }
}
