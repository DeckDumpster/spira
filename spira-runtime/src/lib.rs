//! The service around the `runtime` crate: its pinned settings, the effect runner, the
//! systemd notifier the dispatch loop kicks, and the OTLP log sink for wide events.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
#[cfg(any(test, feature = "integration"))]
pub mod fixture;
pub mod api_budget;
pub mod broker_host;
pub mod forge;
pub mod classes;
pub mod hunk;
pub mod margin_mail;
pub mod modes;
pub mod notify;
pub mod pr_topics;
pub mod repin;
pub mod reconcile_host;
pub mod settings;
pub mod sink;
pub mod sources;
pub mod status;
pub mod store;
pub mod tasks;
pub mod unpublished;
pub mod watchers;
pub mod wiki_git;
pub mod world;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use runtime::host::EffectRunner;
use runtime::{Engine, EffectSpec, Journal, Marker, Registration, Value, WideSink};

pub use settings::Settings;

/// Every task in `tasks::all()`, in family order, each in the mode `SPIRA_TASK_MODES` gives it,
/// on its row's plane, with its row's signal effects and the shared ones let through a halt.
/// `Settings::from_lookup` refuses a table that does not name every task exactly.
pub fn registrations(s: &Settings) -> Vec<Registration> {
    modes::apply(tasks::all().map(|t| waking(t, s, (t.build)(s).with_plane(t.plane).with_signal_effects(t.signal).with_signal_effects(tasks::SIGNAL_EFFECTS).with_effects(t.effects))).collect(), &s.task_modes).expect("Settings::from_lookup checked the task modes")
}

/// `reg` with the wake its row's reads call for: a row that reads only the store stops polling, one that also reads elsewhere keeps its cadence.
fn waking(t: &tasks::Task, s: &Settings, reg: Registration) -> Registration {
    let wake = |reads: &'static [&'static str], polls| runtime::Wake { reads: reads.to_vec(), min_gap_ms: (t.cadence_ms)(s), polls };
    match t.reads {
        tasks::Reads::Only(p) => reg.with_wake(wake(p, false)),
        tasks::Reads::Polled(p) => reg.with_wake(wake(p, true)),
        tasks::Reads::None | tasks::Reads::Undeclarable(_) => reg,
    }
}

/// The control socket's submit: one `Submit` on the host's channel, answered once the host has
/// queued it or refused it.
pub fn control_submit(tx: tokio::sync::mpsc::Sender<runtime::host::Submit>) -> status::SubmitFn {
    Arc::new(move |id: &str, stream: &str, kind: runtime::EventKind| {
        let (sub, reply) = runtime::host::Submit::with_reply(id, stream, kind);
        tx.blocking_send(sub).map_err(|_| "the scheduler is not taking submissions".to_string())?;
        reply.blocking_recv().map_err(|_| "the scheduler dropped the submission".to_string())?
    })
}

/// Answers `scheduler status|halt|resume` on `path` from its own thread, replacing a socket a
/// previous process left behind.
pub fn spawn_control(path: &std::path::Path, control: status::Control) -> Result<std::thread::JoinHandle<()>, String> {
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| format!("control socket {}: {e}", path.display()))?;
    }
    let listener = std::os::unix::net::UnixListener::bind(path).map_err(|e| format!("control socket {}: {e}", path.display()))?;
    std::thread::Builder::new()
        .name("control".into())
        .spawn(move || {
            if let Err(e) = runtime::socket::answer(&listener, &|argv| control.respond(argv)) {
                eprintln!("spira-runtime: control socket stopped: {e}");
            }
        })
        .map_err(|e| format!("control thread: {e}"))
}

/// The most bytes a record journaled as one carry may serialize to: three quarters of the store's
/// 65,535-byte column, the rest left for an entry that grows once carried.
pub const CARRY_BYTES: usize = 48 * 1024;

/// The gap between one task's first tick and the next at boot, so the tasks do not all fire
/// at the same instant and fill every class at once.
pub const BOOT_STAGGER_MS: u64 = 250;

/// Hands every task its first tick, in `tasks::all()` order, [`BOOT_STAGGER_MS`] apart: the
/// first now, the last about `BOOT_STAGGER_MS` times the task count later.
pub fn start_tasks(engine: &mut Engine) {
    let now = engine.now_ms();
    for (i, (_, stream, kind)) in tasks::boots().into_iter().enumerate() {
        engine.start_at(stream, kind, now + i as u64 * BOOT_STAGGER_MS);
    }
}

pub const BOOT_RETRY_FIRST_MS: u64 = 250;
pub const BOOT_RETRY_CEILING_MS: u64 = 1_000;

/// `line` under the UTC stamp every line in the service's log carries.
pub fn stamped(line: &str) -> String {
    format!("{} {line}", watchers::iso8601(runtime::ports::Clock::now_ms(&runtime::ports::WallClock) / 1000))
}

fn boot_retry(s: &Settings, bound: Duration) -> runtime::engine::BootRetry {
    runtime::engine::BootRetry {
        first_ms: BOOT_RETRY_FIRST_MS,
        ceiling_ms: BOOT_RETRY_CEILING_MS,
        bound_ms: bound.as_millis() as u64,
        store: format!("the journal store at {}", s.lc_socket.display()),
        // waits-on: the journal store's next answer; cap `bound`
        sleep: Arc::new(|ms| std::thread::sleep(std::time::Duration::from_millis(ms))),
    }
}

/// `bound` is how long the journal may stay unanswered before the open gives up.
pub fn open_engine(s: &Settings, journal: Arc<dyn Journal>, clock: Arc<dyn runtime::ports::Clock>, marker: Arc<dyn Marker>, sink: Arc<dyn WideSink>, extra: Vec<Registration>, bound: Duration) -> runtime::Result<Engine> {
    let mut regs = registrations(s);
    regs.extend(extra);
    let mut engine = Engine::open_with(journal, clock, regs, 0, boot_retry(s, bound))?;
    if s.run_dir.join("world.halted").exists() {
        engine.halt_at_boot("the world's halt stamp is present at boot")?;
    }
    engine
        .with_sink(sink)
        .with_baseline(vec![("runtime.version".into(), Value::from(env!("CARGO_PKG_VERSION"))), ("config.hash".into(), Value::from(s.hash()))])
        .with_stall_limit(s.stall_limit)
        .with_marker(marker)
}

/// Where startup is and what the journal last said, for the line written if it never finishes.
pub struct Startup {
    deadline: Duration,
    step: std::sync::Mutex<&'static str>,
    last_answer: std::sync::Mutex<String>,
}

impl Startup {
    pub fn new(deadline: Duration) -> Self {
        Startup { deadline, step: Default::default(), last_answer: Default::default() }
    }

    fn enter(&self, step: &'static str) {
        *self.step.lock().unwrap_or_else(|p| p.into_inner()) = step;
    }

    fn heard(&self, answer: &str) {
        *self.last_answer.lock().unwrap_or_else(|p| p.into_inner()) = answer.to_string();
    }

    pub fn unfinished(&self) -> String {
        let deadline = self.deadline;
        let step = *self.step.lock().unwrap_or_else(|p| p.into_inner());
        let answer = self.last_answer.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let heard = if answer.is_empty() { "no journal answer yet".to_string() } else { format!("last journal answer: {answer}") };
        stamped(&format!("spira-runtime: startup did not finish within {deadline:?}: still in {step}; {heard}"))
    }
}

struct Overheard {
    inner: Arc<dyn Journal>,
    startup: Arc<Startup>,
}

impl Journal for Overheard {
    fn record(&self, ev: &runtime::Event) {
        self.inner.record(ev)
    }
    fn boot(&self) -> runtime::Result<runtime::journal::Boot> {
        self.inner.boot().inspect_err(|e| self.startup.heard(&e.0))
    }
    fn commit(&self, turn: &runtime::journal::Turn) -> runtime::Result<()> {
        self.inner.commit(turn)
    }
    fn poison(&self, p: &runtime::journal::Poison) -> runtime::Result<()> {
        self.inner.poison(p)
    }
    fn poisoned(&self, stream: &runtime::StreamKey) -> runtime::Result<Option<runtime::journal::Poison>> {
        self.inner.poisoned(stream)
    }
    fn shipped(&self) -> Option<spira_otel::vlogs::ShipCounts> {
        self.inner.shipped()
    }
}

/// The installed store views, the table of the revision applied to them, and the store's socket.
pub type StoreViews = (Arc<store_view::Projection>, Arc<store::ChangeTable>, std::path::PathBuf);

/// Everything that must finish before the dispatch loop can start. The journal is asked for at
/// most half of `deadline`, so a store that cannot tell is reported here with time left to print it.
pub fn start_engine(s: &Settings, journal: Arc<dyn Journal>, clock: Arc<dyn runtime::ports::Clock>, marker: Arc<dyn Marker>, sink: Arc<dyn WideSink>, board: Arc<runtime::engine::StatusBoard>, startup: &Arc<Startup>) -> Result<(Engine, Option<StoreViews>), String> {
    startup.enter("journal boot");
    let journal = Arc::new(Overheard { inner: journal, startup: startup.clone() });
    let engine = open_engine(s, journal, clock, marker, sink, vec![], startup.deadline / 2).map_err(|e| stamped(&format!("spira-runtime: startup, journal boot: {e}")))?;
    let mut engine = engine.with_status_board(board);
    startup.enter("start_tasks");
    start_tasks(&mut engine);
    startup.enter("store::start");
    let store = store::start(&s.lc_socket).map(|(views, table)| (views, table, s.lc_socket.clone())).inspect_err(|e| eprintln!("{}", stamped(&format!("spira-runtime: no store views, tasks read the store per tick: {e}")))).ok(); // loop-safe: runs on the startup offload thread
    let engine = match &store {
        Some((_, table, _)) => engine.with_sweep(s.sweep_secs * 1000, table.clone()),
        None => engine,
    };
    Ok((engine, store))
}

/// Runs the effects the service's workflows ask for. HTTP goes to the heartbeat URL and the forge; the watchers read files, the lifecycle socket and (brain-dirty) a bounded git on the wiki checkout, and the operator's mail is delivered in-process.
#[derive(Default)]
pub struct Effects {
    pub heartbeat_url: String,
    pub world: world::World,
    pub forge: forge::Forge,
    pub mail: Option<Arc<mail::env::Env>>,
    pub reconciler: Option<watchers::reconciler::Reconciler>,
    pub landrefs: Arc<reconcile_host::Landrefs>,
    /// What the reconciler has asked git, by the shas that decided the answer.
    pub memo: Arc<std::sync::Mutex<std::collections::BTreeMap<String, String>>>,
    /// The programs the reconciler launches and the bus it reads, when not the installed ones.
    pub seams: Option<reconciler::Seams>,
    pub bus: Option<String>,
    /// The scheduler's status board, which the fleet check reads the sentinel's last pass from.
    pub board: Option<Arc<runtime::engine::StatusBoard>>,
    pub enabled: OnceLock<String>,
    pub reconciling: AtomicBool,
    pub concierge_mail: Option<Arc<mail::env::Env>>,
    /// The unit manager every `unit.*` effect runs on: `SystemdUnits` on Linux, `ProcessUnits`
    /// elsewhere (chosen in `main.rs`), `ScriptedUnits` in tests.
    pub units: Option<Arc<dyn runtime::units::Units>>,
    /// The VM pool the cockpit document's VM rows are read from; none reads as an error row.
    pub vm_pool: Option<Arc<round_vm::pool::Pool>>,
    pub overrides_held: wiki_git::Held,
    /// pr-notify's publisher, built at its first use; `None` when the configuration names no
    /// broker.
    pub pr_topics: OnceLock<Option<pr_topics::PrTopics>>,
}

/// The mail environment operator notes are sent under: no bd (a note needs no tracking bead, and an empty db keeps the mailer from launching one), and the repeat guard overridden because consecutive merges normalise to one subject.
pub fn operator_mail(mut env: mail::env::Env) -> mail::env::Env {
    env.db.clear();
    env.repeat_considered = Some("pr-notify deduplicates by merge sha; subjects differ only in PR number and sha".into());
    env
}

/// The mail environment the Concierge's alerts are sent under: no bd, as for a note.
pub fn concierge_mail(mut env: mail::env::Env) -> mail::env::Env {
    env.db.clear();
    env
}

impl Effects {
    fn concierge_note(&self, input: &str) -> Result<String, String> {
        let env = self.concierge_mail.as_deref().ok_or("no concierge mail environment configured")?;
        let note: watchers::reconciler_flow::Mail = serde_json::from_str(input).map_err(|e| e.to_string())?;
        let args = mail::cmds::SendArgs { mailbox: "concierge", from: Some(reconciler_engine::mail::FROM), subject: &note.subject, kind: "note", default: "", class: "", bead: "", urgent: false, digest: false, dry_run: false, requested_by: "" };
        let bd = mail::bead::BdCli { conn_retries: 1 };
        mail::cmds::send(&bd, env, &args, format!("## Note\n{}\n", note.body)).map(|o| o.delivered_path.display().to_string()).inspect_err(|_| eprintln!("reconciler-flow: concierge alert failed for: {}", note.subject))
    }

    fn pr_topics(&self) -> Option<&pr_topics::PrTopics> {
        let build = || spira_pub::Publisher::new("pr-notify").inspect_err(|e| eprintln!("pr-notify: nothing will be published: {e}")).ok().map(pr_topics::PrTopics::new);
        self.pr_topics.get_or_init(build).as_ref()
    }

    fn pr_observe(&self, input: &str) -> Result<String, String> {
        if let Some(topics) = self.pr_topics() {
            topics.track(input);
        }
        self.world.pr_snapshot(&self.forge, input)
    }

    fn pr_publish(&self, input: &str) -> Result<String, String> {
        self.pr_topics().map_or_else(|| Ok("not published: no broker configured".into()), |t| t.publish(input))
    }

    fn operator_note(&self, input: &str) -> Result<String, String> {
        let env = self.mail.as_deref().ok_or("no mail environment configured")?;
        let note: watchers::pr_notify::Note = serde_json::from_str(input).map_err(|e| e.to_string())?;
        let args = mail::cmds::SendArgs { mailbox: "operator", from: Some("PR Notify <pr-notify@spira>"), subject: &note.subject, kind: "note", default: "", class: "", bead: "", urgent: false, digest: true, dry_run: false, requested_by: "" };
        let bd = mail::bead::BdCli { conn_retries: 1 };
        mail::cmds::send(&bd, env, &args, format!("## Note\n{}\n", note.body)).map(|o| o.delivered_path.display().to_string()).inspect_err(|_| eprintln!("pr-notify: operator mail failed for: {}", note.subject))
    }
}

/// Marks a pass running and frees the mark on drop, so two passes never overlap.
struct Passing<'a>(&'a AtomicBool);

impl Drop for Passing<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Entries older than this are dropped from the effect ledger.
const LEDGER_KEEP_SECS: u64 = 86_400;

impl Effects {
    /// One structural pass: the reads, in this process, as a serialised `Observation`.
    fn reconcile_observe(&self, input: &str) -> Result<String, String> {
        let r = self.reconciler.as_ref().ok_or("no reconciler configured")?;
        let input: watchers::reconciler::ObserveIn = serde_json::from_str(input).map_err(|e| e.to_string())?;
        if self.reconciling.swap(true, Ordering::SeqCst) {
            return Err("a reconciler pass is still running".into());
        }
        let _passing = Passing(&self.reconciling);
        let at = input.at / 1000;
        let run = self.world.run_dir.clone();
        let pinned = |k: &str| input.values.get(k).cloned().ok_or_else(|| format!("{k} unknown"));
        let instance = pinned("SPIRA_INSTANCE")?;
        let enabled = self.enabled.get_or_init(|| install::bootstrap::manifest_from_env(&instance).map(|m| m.enable(&instance).join("\n")).unwrap_or_default()).clone();
        let min_age = pinned("SPIRA_STRANDED_RUNNER_MIN_AGE")?.trim().parse().unwrap_or(0);
        let mut host = reconcile_host::RuntimeHost::new(self.world.clone(), self.forge.clone(), self.mail.clone(), r.home.clone().into(), pinned("SPIRA_RUNES")?, min_age, at, enabled, self.landrefs.clone());
        host.bus = self.bus.clone();
        let starting = host.starts_not_yet_live(&input.starting);
        host.board = self.board.clone();
        let mut cfg = reconciler::Config::from_lookup(&pinned, match self.seams.clone() {
            Some(s) => s,
            None => reconciler::Seams::production()?,
        }, run, r.home.clone(), at, reconciler_engine::clock::iso_of(at))?.with_desired_state().with_host(Arc::new(host));
        cfg = cfg.with_memo(self.memo.clone());
        cfg.skip_units = own_skip_units(&instance);
        cfg.starting = starting;
        let observation = reconciler::observe(&cfg, &input.reminded);
        serde_json::to_string(&observation).map_err(|e| e.to_string())
    }

    /// Runs `act` once per effect key: a restart re-issues an effect that has no outcome, and a
    /// remedy or a mail must not go twice for it.
    fn once(&self, key: &str, act: impl FnOnce() -> Result<String, String>) -> Result<String, String> {
        let dir = self.world.run_dir.join("reconciler-effects.d");
        let safe: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
        if dir.join(&safe).exists() {
            return Ok(format!("effect {key} already ran"));
        }
        let out = act()?;
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(&safe), "");
        let old = std::time::SystemTime::now() - Duration::from_secs(LEDGER_KEEP_SECS);
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < old) {
                let _ = std::fs::remove_file(e.path());
            }
        }
        Ok(out)
    }

    fn log_line(&self, line: &str) {
        use std::io::Write;
        let stamp = reconciler_engine::clock::iso_of(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()));
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.world.run_dir.join("reconciler.log")) {
            let _ = writeln!(f, "{stamp} spira: {line}");
        }
    }

    fn reconcile_record(&self, input: &str) -> Result<String, String> {
        let r = self.reconciler.as_ref().ok_or("no reconciler configured")?;
        let rec: watchers::reconciler::Record = serde_json::from_str(input).map_err(|e| e.to_string())?;
        let endpoint = r.values.get("SPIRA_METRICS_ENDPOINT").cloned().filter(|e| !e.trim().is_empty());
        let store = tsd::store::Pusher::new(endpoint);
        let now_iso = reconciler_engine::clock::iso_of(rec.at / 1000);
        let release = reconciler::release_of(&r.home);
        for row in &rec.status {
            reconciler_engine::io::push_status(&store, &now_iso, row);
        }
        for e in &rec.events {
            reconciler_engine::effect::append_event(&store, &reconciler_engine::effect::RemedyEvent { ts: &now_iso, event: &e.event, kind: reconciler_engine::effect::kind_of(&e.key), key: &e.key, action: &e.action, metric: "gap", before: e.before, after: e.after, release: &release });
        }
        rec.log.iter().for_each(|l| self.log_line(l));
        Ok(format!("{} status, {} events", rec.status.len(), rec.events.len()))
    }

    fn reconcile_remedy(&self, key: &str, input: &str) -> Result<String, String> {
        let job: watchers::reconciler::RemedyJob = serde_json::from_str(input).map_err(|e| e.to_string())?;
        self.once(key, || {
            let mut all_ok = true;
            for (program, args) in &job.steps {
                if let Err(e) = reconciler::run_remedy_step(program, &args.iter().map(String::as_str).collect::<Vec<_>>()) {
                    self.log_line(&e);
                    all_ok = false;
                }
            }
            if let Some(note) = &job.note {
                let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                queue::ops::landing_log(&self.world.run_dir, &format!("QUEUE STRANDED_RERUN {at} {note} ok={all_ok}"));
            }
            Ok(format!("{}: {} step(s)", job.key, job.steps.len()))
        })
    }

    fn reconcile_mail(&self, key: &str, input: &str) -> Result<String, String> {
        let job: watchers::reconciler::MailJob = serde_json::from_str(input).map_err(|e| e.to_string())?;
        self.once(key, || {
            reconcile_host::send_mail(self.mail.as_deref(), &job.mail).inspect_err(|e| self.log_line(&format!("reconciler: {} failed: {e}", job.what)))?;
            if job.remind.is_some() {
                self.log_line(&format!("reconciler: {}", job.what));
            }
            Ok(job.what.clone())
        })
    }
}

impl Effects {
    /// One `unit.*` effect (`spec.name`) on the configured unit manager, through
    /// `runtime::units::effect`, so the host and the simulator answer it the same way.
    fn unit(&self, spec: &EffectSpec) -> Result<String, String> {
        let units = self.units.as_deref().ok_or("no unit manager configured")?;
        runtime::units::effect(units, &spec.name, &spec.input)
    }

    fn ping(&self, spec: &EffectSpec) -> Result<String, String> {
        if self.heartbeat_url.is_empty() {
            return Ok("no heartbeat url configured".into());
        }
        let r = ureq::AgentBuilder::new().timeout(Duration::from_millis(spec.deadline_ms)).build().get(&self.heartbeat_url).call().map_err(|e| e.to_string())?;
        Ok(format!("status {}", r.status()))
    }
}

/// An observe that answered is a tick; one that failed is not, so a task whose observe errors
/// every time goes quiet to whoever reads the stamp.
fn stamp_answered_probe(run_dir: &std::path::Path, effect: &str, answered: &Result<String, String>) {
    if let (Ok(_), Some(task)) = (answered, spira_config::ticked::task_of_probe(effect)) {
        spira_config::ticked::stamp(run_dir, task);
    }
}

#[cfg(test)]
mod remedy_note_tests {
    use super::*;

    fn landing_log_after(program: &str) -> String {
        let dir = testkit::TempDir::new("remedy-note");
        let fx = Effects { world: world::World { run_dir: dir.path().to_path_buf(), ..Default::default() }, ..Default::default() };
        let job = watchers::reconciler::RemedyJob { key: "k".into(), steps: vec![(program.into(), vec![])], note: Some(r#"repo=r run=7 job="a b" label=ci-7-1 queued=400s runner=none"#.into()) };
        fx.reconcile_remedy("k", &serde_json::to_string(&job).unwrap()).unwrap();
        std::fs::read_to_string(dir.path().join("landing.log")).unwrap()
    }

    #[test_id(tnr0c01)]
    #[test]
    fn a_noted_remedy_leaves_its_landing_log_line_with_the_outcome_of_its_step() {
        for (program, ok) in [("true", true), ("false", false)] {
            let log = landing_log_after(program);
            let line = log.trim_end();
            assert!(line.starts_with("QUEUE STRANDED_RERUN "), "{line}");
            assert!(line.ends_with(&format!(r#"repo=r run=7 job="a b" label=ci-7-1 queued=400s runner=none ok={ok}"#)), "{line}");
        }
    }

    #[test_id(tqx3gs8)]
    #[test]
    fn a_failing_remedy_step_is_logged_with_its_program_and_args() {
        let dir = testkit::TempDir::new("remedy-step-log");
        let fx = Effects { world: world::World { run_dir: dir.path().to_path_buf(), ..Default::default() }, ..Default::default() };
        let job = watchers::reconciler::RemedyJob { key: "k".into(), steps: vec![("false".into(), vec!["a".into(), "--wait-cancel".into(), "1".into()])], note: Some("repo=r run=7".into()) };
        fx.reconcile_remedy("k", &serde_json::to_string(&job).unwrap()).unwrap();
        let log = std::fs::read_to_string(dir.path().join("reconciler.log")).unwrap();
        assert!(log.contains("remedy step false a --wait-cancel 1 failed: exit 1"), "{log}");
        let landing = std::fs::read_to_string(dir.path().join("landing.log")).unwrap();
        assert!(landing.trim_end().ends_with("repo=r run=7 ok=false"), "{landing}");
    }
}

#[cfg(test)]
mod probe_stamp_tests {
    use super::stamp_answered_probe;

    #[test_id(t1oc6ke)]
    #[test]
    fn an_observe_that_answers_stamps_its_task_and_one_that_errs_or_is_no_probe_does_not() {
        let run = testkit::TempDir::new("probe-stamp");
        stamp_answered_probe(&run, "batcher.local.observe", &Err("store down".into()));
        stamp_answered_probe(&run, "batcher.local.land", &Ok("{}".into()));
        assert_eq!(spira_config::ticked::quiet_secs(&run, "batcher.local"), None);
        for task in ["batcher.local", "gate.worker"] {
            stamp_answered_probe(&run, &format!("{task}.observe"), &Ok("{}".into()));
            assert!(spira_config::ticked::path(&run, task).exists(), "{task}");
        }
    }
}

/// Runs one named effect: `(effects, idempotency key, spec)`.
pub type Runner = fn(&Effects, &str, &EffectSpec) -> Result<String, String>;

fn routes() -> &'static std::collections::BTreeMap<&'static str, Runner> {
    static ROUTES: OnceLock<std::collections::BTreeMap<&'static str, Runner>> = OnceLock::new();
    ROUTES.get_or_init(tasks::routes)
}

impl EffectRunner for Effects {
    fn run(&self, key: &str, spec: &EffectSpec) -> Result<String, String> {
        let answered = match routes().get(spec.name.as_str()) {
            Some(runner) => runner(self, key, spec),
            None => Err(format!("no runner for effect {}", spec.name)),
        };
        stamp_answered_probe(&self.world.run_dir, &spec.name, &answered);
        answered
    }

    fn run_for_task(&self, task: &str, key: &str, spec: &EffectSpec) -> Result<String, String> {
        spira_store::lc_call::as_task(task, || self.run(key, spec))
    }

    /// The class in [`classes::CLASSES`]. A name no task registers has no runner either; it
    /// runs, and fails at once, under Host.
    fn class(&self, spec: &EffectSpec) -> runtime::pools::EffectClass {
        classes::class_of(&spec.name).unwrap_or(runtime::pools::EffectClass::Host)
    }
}

/// The runtime's own unit, which its reconciler must not treat as a subject.
fn own_skip_units(instance: &str) -> Vec<String> {
    vec![spira_config::unit::unit_name("runtime", "service", instance)]
}

#[cfg(test)]
mod own_unit_tests {
    use std::time::Duration;
    use runtime::journal::MemJournal;
    use runtime::ports::VirtualClock;
    use runtime::{Command, Ctx, EffectSpec, Event, EventKindTag, Handler, Projection, Registration};
    use std::sync::Arc;

    #[test_id(tm78dmb)]
    #[test]
    fn the_boot_retry_caps_its_backoff_below_the_bound_so_a_late_store_is_found_soon() {
        let r = super::super::boot_retry(&crate::settings::fixture(), Duration::from_secs(10));
        assert_eq!((r.first_ms, r.ceiling_ms), (super::super::BOOT_RETRY_FIRST_MS, super::super::BOOT_RETRY_CEILING_MS));
        assert!(r.ceiling_ms < r.bound_ms);
    }

    struct Silent;
    impl runtime::Journal for Silent {
        fn record(&self, _: &Event) {}
        fn boot(&self) -> runtime::Result<runtime::journal::Boot> {
            Err(runtime::RuntimeError::cannot_tell("cannot tell: spira-lc took the journal request but its reply was lost"))
        }
        fn commit(&self, _: &runtime::journal::Turn) -> runtime::Result<()> {
            Ok(())
        }
        fn poison(&self, _: &runtime::journal::Poison) -> runtime::Result<()> {
            Ok(())
        }
        fn poisoned(&self, _: &runtime::StreamKey) -> runtime::Result<Option<runtime::journal::Poison>> {
            Ok(None)
        }
    }

    #[test_id(ts0avk9)]
    #[test]
    fn a_startup_over_a_journal_that_cannot_tell_fails_inside_the_deadline_with_a_stamped_line_naming_the_step() {
        let deadline = Duration::from_millis(800);
        let startup = Arc::new(super::super::Startup::new(deadline));
        let err = super::super::start_engine(&crate::settings::fixture(), Arc::new(Silent), Arc::new(VirtualClock::new(1_000)), Arc::new(runtime::marker::NullMarker), Arc::new(runtime::NullSink), Arc::new(runtime::engine::StatusBoard::default()), &startup).err().expect("must fail");
        let stamp = err.split(' ').next().unwrap();
        assert!(stamp.len() == 20 && stamp.ends_with('Z') && stamp.as_bytes()[10] == b'T', "{err}");
        assert!(err.contains("journal boot") && err.contains("reply was lost"), "{err}");
        assert!(startup.unfinished().contains("reply was lost"), "the timeout line carries the journal's last words too");
    }

    struct Writer;
    impl Handler for Writer {
        fn interests(&self) -> &[EventKindTag] {
            &["start"]
        }
        fn handle(&self, _: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
            vec![Command::Effect(EffectSpec::keyed("write", "write", String::new(), 1000))]
        }
    }

    fn boot_under(stamp: bool) -> (Vec<String>, runtime::engine::SchedulerStatus) {
        let dir = testkit::TempDir::new("boot-halt");
        let mut s = crate::settings::fixture();
        s.run_dir = dir.path().to_path_buf();
        if stamp {
            std::fs::write(dir.path().join("world.halted"), "2000-01-01T00:00:00Z\nwhy: planted\n").unwrap();
        }
        let j = MemJournal::new();
        let mut e = super::open_engine(&s, j.clone(), Arc::new(VirtualClock::new(1_000)), Arc::new(runtime::marker::NullMarker), Arc::new(runtime::NullSink), vec![Registration::new("probe", Writer)], Duration::from_secs(10)).unwrap();
        e.start("probe", runtime::EventKind::new("start", ""));
        runtime::Executor::run_until_idle(&mut e).unwrap();
        let tags = runtime::conformance::trace(&*j).into_iter().map(|t| t.2.to_string()).filter(|t| t.starts_with("effect.")).collect();
        (tags, e.status())
    }

    #[test_id(tb6w1vw)]
    #[test]
    fn a_scheduler_booted_under_the_halt_stamp_leaves_world_status_halted_not_degraded() {
        let status = boot_under(true).1;
        let doc = crate::status::document(&status).to_string();
        let parsed = spira_world::scheduler::parse_status(&doc).unwrap();
        assert_eq!(spira_world::scheduler::mismatch(true, &parsed), None, "{doc}");
    }

    #[test_id(t4tgqmz)]
    #[test]
    fn a_boot_under_the_halt_stamp_holds_its_first_work_effect() {
        let tags = boot_under(true).0;
        assert!(tags.iter().any(|t| t == runtime::EFFECT_HELD) && !tags.iter().any(|t| t == runtime::EFFECT_REQUESTED), "{tags:?}");
    }

    #[test_id(tygv5ry)]
    #[test]
    fn a_boot_without_the_halt_stamp_requests_its_first_work_effect() {
        let tags = boot_under(false).0;
        assert!(tags.iter().any(|t| t == runtime::EFFECT_REQUESTED) && !tags.iter().any(|t| t == runtime::EFFECT_HELD), "{tags:?}");
    }

    #[test_id(tixxf4a)]
    #[test]
    fn skip_list_holds_the_installed_runtime_service_name() {
        assert_eq!(super::super::own_skip_units("prod"), vec!["spira-runtime-prod.service".to_string()]);
    }
}
