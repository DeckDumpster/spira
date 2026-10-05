//! Orchestration: the dedupe scan, filing a new bead or bumping a recurrence, the Sin
//! escalation and the undeclared-repo ask. Pure decision-making lives in `decide.rs`; this
//! module only sequences calls to the `Bd`/`Mailer`/`Clock` ports and never contains a
//! judgement call `decide.rs` could have made instead — table-driven parity against the
//! bash is easiest when every "should this fire" answer has exactly one place it is typed.

use crate::decide::{self, DedupHit, Scope};
use crate::ports::{self, Bd, Clock, Mailer};

pub struct FileConfig<'a> {
    pub db: &'a str,
    pub kind: &'a str,
    pub priority: &'a str,
    pub actor: Option<&'a str>,
    pub cause: &'a str,
    pub sin_at: u32,
    pub sin_exempt: bool,
    pub watcher_interval_s: i64,
    pub dedup_lookback_days: i64,
    pub repo_declared: Option<&'a str>,
    pub delivers_pref: Option<&'a str>,
    pub sop_ledger: &'a str,
    pub spira_run: &'a str,
    pub home_repo: &'a str,
    pub known_repos: &'a [String],
    pub ask_label: &'a str,
    pub provenance: &'a str,
}

pub enum FileOutcome {
    Filed(String),
    /// The database could not be reached; the caller must keep the spool entry.
    Unreachable,
    /// The database was reachable but the `bd create` guards refused the filing outright
    /// (a bad repo: label or destructive/schema-delete text) — also kept spooled, since
    /// retrying an unreachable-looking failure is safer than silently dropping the event,
    /// and the guard message is on the log for a human to fix the caller.
    Refused,
}

fn since_date(now: i64, lookback_days: i64) -> String {
    let secs = lookback_days.max(0) * 86_400;
    let epoch = (now - secs).max(0);
    epoch_to_date(epoch)
}

/// `date -u -d @<epoch> +%Y-%m-%d`, reimplemented without a libc TZ dependency: days since
/// the epoch via the standard proleptic Gregorian calendar (civil_from_days, Howard
/// Hinnant's algorithm), which is what every other UTC-date site in this codebase would
/// compute too — verified against `date -u` for several sample epochs while writing this.
pub fn epoch_to_date(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// `_dedup_incident`: the four-pass scan (open label-keyed, open fallback, closed
/// label-keyed, closed fallback). `None` means "no existing incident, and the database
/// answered" (a real "nothing found"); `Some(Err(()))` means the database could not be
/// reached at all — the two are distinguished by a final reachability probe, exactly as
/// the bash does, because an empty dedup scan and an unreachable store print identically
/// from `bd list`'s own `2>/dev/null`.
fn dedup_incident(bd: &dyn Bd, db: &str, reference: &str, lookback_days: i64, now: i64) -> Result<Option<DedupHit>, ()> {
    let ref_label = format!("ref:{}", decide::ref_hash(reference));
    let since = since_date(now, lookback_days);
    // Open/closed is each bead's lifecycle row (sp-jgjvh: incident beads are work beads);
    // the closed lookback keeps bd's `closed_at` as content. A pass that could not read —
    // bd or the machine — is not "nothing found": the event stays spooled.
    let passes: [(Scope, Option<&str>, Option<&str>, bool); 4] = [
        (Scope::Unfinished, Some(ref_label.as_str()), None, false),
        (Scope::Unfinished, None, None, true),
        (Scope::HandedOn, Some(ref_label.as_str()), Some(since.as_str()), false),
        (Scope::HandedOn, None, Some(since.as_str()), true),
    ];
    let mut blind = false;
    for (scope, label, closed_after, skip_ref_labeled) in passes {
        match bd.list(db, scope, label, closed_after) {
            Ok(rows) => {
                if let Some(hit) = decide::dedup_scan(&rows, scope == Scope::Unfinished, skip_ref_labeled, reference) {
                    return Ok(Some(hit));
                }
            }
            Err(_) => blind = true,
        }
    }
    if !blind && bd.reachable(db) {
        Ok(None)
    } else {
        Err(())
    }
}

/// `file_one`: create or dedupe. `labels` is the full label string a fresh filing would
/// carry (`SPIRA_INCIDENT_LABELS`, e.g. "spira,incident"); the dedupe scan itself never
/// filters on it (see `dedup_incident` — only `ref:<hash>` is a safe prefilter, per
/// incident.sh's own comment: two filers declaring different labels must still find each
/// other's bead).
#[allow(clippy::too_many_arguments)]
pub fn file_one(
    bd: &dyn Bd,
    mailer: &dyn Mailer,
    clock: &dyn Clock,
    cfg: &FileConfig,
    reference: &str,
    title: &str,
    payload: &[u8],
    labels: &str,
    log: &mut Vec<String>,
) -> FileOutcome {
    let now = clock.now();
    let hit = match dedup_incident(bd, cfg.db, reference, cfg.dedup_lookback_days, now) {
        Err(()) => {
            log.push(format!("database unreachable — {reference} stays spooled"));
            return FileOutcome::Unreachable;
        }
        Ok(h) => h,
    };

    match hit {
        Some(DedupHit::Open { id }) | Some(DedupHit::Closed { id, closed_at: None }) => {
            bump_and_note(bd, mailer, clock, cfg, &id, reference, title, payload, false, now, log)
        }
        Some(DedupHit::Closed { id, closed_at: Some(closed_at) }) => {
            let closed_ts = parse_iso8601(&closed_at);
            let cause = closed_ts.map(|ts| decide::reopen_cause(ts, now, cfg.watcher_interval_s)).unwrap_or("recurrence");
            let _ = cause;
            bd.reopen(cfg.db, &id);
            let note = format!(
                "Recurrence at {} — same failure fingerprint, dedup within {}-day window",
                iso_now_public(now),
                cfg.dedup_lookback_days
            );
            bd.note(cfg.db, &id, &note);
            bump_and_note(bd, mailer, clock, cfg, &id, reference, title, payload, true, now, log)
        }
        // sp-nmlna: a terminal row has no move out, so reopening the bead in bd would only
        // pile notes under a row that stays LANDED. The recurrence is a new incident: a fresh
        // bead (and, through `Bd::create`, a fresh row) citing the closed one, linked to it;
        // the closed bead and its row are left exactly as they are.
        Some(DedupHit::Terminal { id: pred, closed_at }) => {
            let predecessor = Predecessor { id: &pred, closed_at: closed_at.as_deref() };
            file_new(bd, cfg, reference, title, payload, labels, Some(predecessor), log)
        }
        None => file_new(bd, cfg, reference, title, payload, labels, None, log),
    }
}

/// The terminal incident a fresh filing recurs (sp-nmlna).
struct Predecessor<'a> {
    id: &'a str,
    closed_at: Option<&'a str>,
}

#[allow(clippy::too_many_arguments)]
fn bump_and_note(
    bd: &dyn Bd,
    mailer: &dyn Mailer,
    _clock: &dyn Clock,
    cfg: &FileConfig,
    id: &str,
    reference: &str,
    title: &str,
    payload: &[u8],
    was_reopened: bool,
    now: i64,
    log: &mut Vec<String>,
) -> FileOutcome {
    let ev_n = ports::recurs_of(bd, cfg.db, id);
    let events_unknown = ev_n.is_none();
    let n = ev_n.unwrap_or(0) + 1;

    bd.label_add(cfg.db, id, &format!("ref:{}", decide::ref_hash(reference)));

    let prev_hash = bd.label_list(cfg.db, id).into_iter().find(|l| l.starts_with("payload-hash:"));
    let (note_suffix, new_hash) = decide::recur_note_body(prev_hash.as_deref(), payload, was_reopened);
    let reopen_note = if was_reopened { "\nReopened by dedup — same external ref seen again within the lookback window." } else { "" };
    let note = format!("Recurrence {n} at {}.{reopen_note}{}", iso_now_public(now), note_suffix.clone().unwrap_or_default());
    bd.note(cfg.db, id, &note);
    if note_suffix.is_some() {
        if let Some(prev) = &prev_hash {
            bd.label_remove(cfg.db, id, prev);
        }
        bd.label_add(cfg.db, id, &format!("payload-hash:{new_hash}"));
    }

    ports::bump_recur(bd, cfg.db, id, cfg.cause);
    let log_suffix = if was_reopened { " (reopened from closed)" } else { "" };
    log.push(format!("{reference} recurred ({n}) — {id}{log_suffix}"));

    let already_sin = bd.label_list(cfg.db, id).iter().any(|l| l == "sin");
    if events_unknown {
        log.push(format!("{reference}: recurrence count unknown — events query failed; Sin is blind this cycle"));
    } else if cfg.sin_exempt && n >= cfg.sin_at {
        log.push(format!("{reference} crossed SIN_AT={} ({n} recurrences) but is exempt — no escalation", cfg.sin_at));
    } else if decide::crosses_sin(n, cfg.sin_at, cfg.sin_exempt, events_unknown, already_sin) {
        bd.label_add(cfg.db, id, "sin");
        let age = bd
            .show_created_at(cfg.db, id)
            .and_then(|c| parse_iso8601(&c))
            .map(|created| now - created)
            .filter(|secs| *secs > 0)
            .map(|secs| format!(" over {}h {}m", secs / 3600, (secs % 3600) / 60))
            .unwrap_or_default();
        let subject = format!("{} — recurred {n} times{age} with no fix holding. Mute it, or keep paging?", cfg.provenance);
        let default = format!("mute this alert and leave {id} open for Ops to work unpaged; keep paging only if you want a decision on every recurrence");
        let evidence: Vec<u8> = payload.iter().take(2000).copied().collect();
        let body = format!(
            "{id} is \"{title}\". It has fired {n} times{age} and each recurrence pages you while filing nothing new. Its current vital signs are below — if they show nothing you must act on, muting is the right answer.\n\n{}",
            String::from_utf8_lossy(&evidence)
        );
        mailer.send_operator_question(&subject, &default, &body);
        log.push(format!("{reference} is a SIN at {n} recurrences — escalated once"));
    }
    FileOutcome::Filed(id.to_string())
}

#[allow(clippy::too_many_arguments)]
fn file_new(
    bd: &dyn Bd,
    cfg: &FileConfig,
    reference: &str,
    title: &str,
    payload: &[u8],
    labels: &str,
    predecessor: Option<Predecessor>,
    log: &mut Vec<String>,
) -> FileOutcome {
    if let Some(bad_repo) = cfg.repo_declared.and_then(|r| decide::invalid_repo_label(&format!("repo:{r}"), cfg.home_repo, cfg.known_repos)) {
        log.push(format!("spira: repo:{bad_repo} is not in the repo map"));
        // Same fallback the bash's set-state guard takes: file under the home repo rather
        // than refuse the whole event.
    }
    if let Some(phrase) = decide::destructive_phrase(title, "", labels, cfg.ask_label) {
        log.push(format!("create FAILED for {reference} — contains \"{phrase}\", needs {} — stays spooled", cfg.ask_label));
        return FileOutcome::Refused;
    }
    if decide::contains_schema_delete(title, "") {
        log.push(format!("create FAILED for {reference} — contains a schema_migrations DELETE — stays spooled"));
        return FileOutcome::Refused;
    }

    let payload_text = String::from_utf8_lossy(payload);
    let body = match &predecessor {
        Some(p) => format!(
            "Recurrence of {} (predecessor) — closed{}, its lifecycle row terminal, so this recurrence within the {}-day dedup window is filed as a fresh incident rather than reopening it.\n\n{payload_text}",
            p.id,
            p.closed_at.map(|c| format!(" {c}")).unwrap_or_default(),
            cfg.dedup_lookback_days
        ),
        None => payload_text.into_owned(),
    };
    // bd create rejects titles over 500 chars; clip (full text stays in the body) so a long
    // aeon-filed title does not re-spool forever (sp-nredi).
    let clipped: String = title.chars().take(490).collect();
    let title = clipped.as_str();
    let id = match bd.create(cfg.db, title, cfg.kind, cfg.priority, labels, reference, &body, cfg.actor) {
        Ok(id) => id,
        Err(e) => {
            log.push(format!("create FAILED for {reference} — stays spooled ({e})"));
            return FileOutcome::Unreachable;
        }
    };
    match &predecessor {
        Some(p) => {
            log.push(format!("filed {id} for {reference} — recurrence of {} (terminal; left untouched)", p.id));
            if !bd.relate(cfg.db, &id, p.id) {
                log.push(format!("warning: could not relate {id} to its predecessor {} — the body still cites it", p.id));
            }
        }
        None => log.push(format!("filed {id} for {reference}")),
    }

    if let Some(repo) = cfg.repo_declared {
        let effective = if repo == cfg.home_repo || cfg.known_repos.iter().any(|r| r == repo) {
            repo.to_string()
        } else {
            log.push(format!("warning: SPIRA_INCIDENT_REPO={repo} has no entry in the repository map; using {}", cfg.home_repo));
            cfg.home_repo.to_string()
        };
        bd.set_state(cfg.db, &id, &format!("repo={effective}"));
    }

    match cfg.delivers_pref {
        Some("note") => {
            // Reachable exactly when the ledger is under $SPIRA_RUN (a session here can
            // only ever write there) AND its directory can actually be created — the
            // original bug (sp-obwc's sibling): a bare mtime/prefix check without the
            // mkdir left the criterion looking satisfiable while nothing had made the
            // directory exist, so the eventual write would still fail.
            let under_run = cfg.sop_ledger.starts_with(&format!("{}/", cfg.spira_run));
            let mkdir_ok = under_run
                && std::path::Path::new(cfg.sop_ledger)
                    .parent()
                    .map(|p| std::fs::create_dir_all(p).is_ok())
                    .unwrap_or(false);
            if mkdir_ok {
                bd.label_add(cfg.db, &id, &format!("delivers:note:{}", cfg.sop_ledger));
                log.push(format!("delivers: {id}: delivers:note:{} written (SPIRA_INCIDENT_DELIVERS=note)", cfg.sop_ledger));
            } else {
                bd.label_add(cfg.db, &id, "delivers:action");
                if under_run {
                    log.push(format!("delivers: {id}: cannot create the ledger's directory — falling back to delivers:action"));
                } else {
                    log.push(format!("delivers: {id}: ledger {} is outside {} — falling back to delivers:action", cfg.sop_ledger, cfg.spira_run));
                }
            }
        }
        Some("action") => {
            bd.label_add(cfg.db, &id, "delivers:action");
            log.push(format!("delivers: {id}: delivers:action written (SPIRA_INCIDENT_DELIVERS set by caller)"));
        }
        Some(other) => {
            log.push(format!("delivers: {id}: unrecognised SPIRA_INCIDENT_DELIVERS='{other}' — label not written"));
        }
        None => {
            bd.label_add(cfg.db, &id, "delivers:action");
            log.push(format!("delivers: {id}: delivers:action written (default — close reason is the evidence)"));
        }
    }
    bd.label_add(cfg.db, &id, &format!("ref:{}", decide::ref_hash(reference)));

    if cfg.repo_declared.is_none() {
        bd.label_add(cfg.db, &id, "needs-repo-triage");
        bd.note(
            cfg.db,
            &id,
            &format!(
                "Repository not declared — SPIRA_INCIDENT_REPO was not set. An aeon claiming this bead works it in the home-repo fallback, which may be the wrong checkout. Set the repo dimension with: bd set-state {id} repo=<name>."
            ),
        );
        log.push(format!("{reference} labelled needs-repo-triage — repo undeclared"));
    }
    FileOutcome::Filed(id)
}

/// `_provenance`: "<unit> on <host>: <path>".
pub fn provenance(unit: &str, host: &str, path: &str) -> String {
    format!("{unit} on {host}: {path}")
}

/// A minimal, dependency-free RFC3339/ISO-8601 UTC parser for the subset bd actually
/// prints ("YYYY-MM-DDTHH:MM:SSZ"), returning a Unix epoch. `None` on anything else —
/// the caller already treats an unparseable timestamp as "recurrence", never a crash.
pub fn parse_iso8601(s: &str) -> Option<i64> {
    if s.len() < 19 {
        return None;
    }
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let mo: i64 = s.get(5..7)?.parse().ok()?;
    let d: i64 = s.get(8..10)?.parse().ok()?;
    let h: i64 = s.get(11..13)?.parse().ok()?;
    let mi: i64 = s.get(14..16)?.parse().ok()?;
    let se: i64 = s.get(17..19)?.parse().ok()?;
    // days_from_civil (Hinnant), inverse of epoch_to_date above.
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + se)
}

/// Public so `main.rs` has exactly one date formatter in the binary (used for the `ilog`
/// timestamp and the spool filename's stamp, matching `date -u +%Y-%m-%dT%H:%M:%SZ`).
pub fn iso_now_public(epoch: i64) -> String {
    let date = epoch_to_date(epoch);
    let secs_of_day = epoch.rem_euclid(86_400);
    format!("{date}T{:02}:{:02}:{:02}Z", secs_of_day / 3600, (secs_of_day % 3600) / 60, secs_of_day % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decide::BeadRow;
    use crate::decide::BeadStatus;
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[derive(Default)]
    struct FakeBd {
        rows: RefCell<Vec<BeadRow>>,
        reachable: bool,
        /// The lifecycle machine does not answer: every dedup pass is blind.
        lc_down: bool,
        created: RefCell<Vec<(String, String)>>,
        bodies: RefCell<HashMap<String, String>>,
        reopened: RefCell<Vec<String>>,
        related: RefCell<Vec<(String, String)>>,
        labels: RefCell<HashMap<String, Vec<String>>>,
        notes: RefCell<HashMap<String, Vec<String>>>,
        next_id: RefCell<u32>,
    }

    impl FakeBd {
        fn new() -> FakeBd {
            FakeBd { reachable: true, next_id: RefCell::new(1), ..Default::default() }
        }
    }

    impl Bd for FakeBd {
        // A row's `status` stands for its lifecycle row's state (decide::status_of_lc).
        fn list(&self, _db: &str, scope: Scope, label: Option<&str>, _closed_after: Option<&str>) -> Result<Vec<BeadRow>, String> {
            if self.lc_down {
                return Err("spira-lc: down".into());
            }
            let rows = self.rows.borrow();
            Ok(rows
                .iter()
                .filter(|r| scope.holds(r.status) && label.map(|l| r.labels.iter().any(|x| x == l)).unwrap_or(true))
                .cloned()
                .collect())
        }
        fn create(&self, _db: &str, title: &str, _kind: &str, _priority: &str, labels: &str, external_ref: &str, _body: &str, _actor: Option<&str>) -> Result<String, String> {
            let mut n = self.next_id.borrow_mut();
            let id = format!("sp-fake{n}");
            *n += 1;
            self.created.borrow_mut().push((title.to_string(), external_ref.to_string()));
            self.bodies.borrow_mut().insert(id.clone(), _body.to_string());
            self.rows.borrow_mut().push(BeadRow {
                id: id.clone(),
                status: BeadStatus::Open,
                external_ref: Some(external_ref.to_string()),
                labels: labels.split(',').map(str::to_string).collect(),
                closed_at: None,
            });
            Ok(id)
        }
        fn label_add(&self, _db: &str, id: &str, label: &str) -> bool {
            self.labels.borrow_mut().entry(id.to_string()).or_default().push(label.to_string());
            if let Some(row) = self.rows.borrow_mut().iter_mut().find(|r| r.id == id) {
                row.labels.push(label.to_string());
            }
            true
        }
        fn label_remove(&self, _db: &str, id: &str, label: &str) -> bool {
            if let Some(row) = self.rows.borrow_mut().iter_mut().find(|r| r.id == id) {
                row.labels.retain(|l| l != label);
            }
            true
        }
        fn label_list(&self, _db: &str, id: &str) -> Vec<String> {
            self.rows.borrow().iter().find(|r| r.id == id).map(|r| r.labels.clone()).unwrap_or_default()
        }
        fn note(&self, _db: &str, id: &str, text: &str) -> bool {
            self.notes.borrow_mut().entry(id.to_string()).or_default().push(text.to_string());
            true
        }
        fn set_state(&self, _db: &str, _id: &str, _kv: &str) -> bool {
            true
        }
        fn reopen(&self, _db: &str, id: &str) -> bool {
            self.reopened.borrow_mut().push(id.to_string());
            if let Some(row) = self.rows.borrow_mut().iter_mut().find(|r| r.id == id) {
                row.status = BeadStatus::Open;
                row.closed_at = None;
            }
            true
        }
        fn relate(&self, _db: &str, a: &str, b: &str) -> bool {
            self.related.borrow_mut().push((a.to_string(), b.to_string()));
            true
        }
        fn show_closed_at(&self, _db: &str, id: &str) -> Option<String> {
            self.rows.borrow().iter().find(|r| r.id == id).and_then(|r| r.closed_at.clone())
        }
        fn show_created_at(&self, _db: &str, _id: &str) -> Option<String> {
            None
        }
        fn reachable(&self, _db: &str) -> bool {
            self.reachable
        }
        fn sql(&self, _db: &str, _query: &str) -> Result<String, String> {
            Ok("header\n----\n0\n".to_string())
        }
    }

    struct FakeMailer {
        sent: RefCell<Vec<String>>,
    }
    impl Mailer for FakeMailer {
        fn send_operator_question(&self, subject: &str, _default: &str, _body: &str) -> bool {
            self.sent.borrow_mut().push(subject.to_string());
            true
        }
    }

    struct FixedClock(i64);
    impl Clock for FixedClock {
        fn now(&self) -> i64 {
            self.0
        }
    }

    fn cfg<'a>(known: &'a [String]) -> FileConfig<'a> {
        FileConfig {
            db: "db",
            kind: "bug",
            priority: "2",
            actor: None,
            cause: "systemd-fail",
            sin_at: 5,
            sin_exempt: false,
            watcher_interval_s: 1800,
            dedup_lookback_days: 7,
            repo_declared: Some("spira"),
            delivers_pref: None,
            sop_ledger: "/run/sop/applied.jsonl",
            spira_run: "/run",
            home_repo: "spira",
            known_repos: known,
            ask_label: "needs-ryan", // literal-ok: test fixture value, not a config default read at runtime.
            provenance: "foo.service on host: ?",
        }
    }

    #[test]
    fn first_filing_creates_a_bead() {
        let bd = FakeBd::new();
        let mailer = FakeMailer { sent: RefCell::new(vec![]) };
        let clock = FixedClock(1_000_000);
        let known = vec!["spira".to_string()];
        let mut log = vec![];
        let out = file_one(&bd, &mailer, &clock, &cfg(&known), "incident:x", "x failed", b"payload", "spira,incident", &mut log);
        match out {
            FileOutcome::Filed(id) => assert!(id.starts_with("sp-fake")),
            _ => panic!("expected Filed"),
        }
        assert_eq!(bd.created.borrow().len(), 1);
    }

    #[test]
    fn second_filing_is_a_recurrence_not_a_new_bead() {
        let bd = FakeBd::new();
        let mailer = FakeMailer { sent: RefCell::new(vec![]) };
        let clock = FixedClock(1_000_000);
        let known = vec!["spira".to_string()];
        let mut log = vec![];
        let c = cfg(&known);
        let first = file_one(&bd, &mailer, &clock, &c, "incident:x", "x failed", b"payload", "spira,incident", &mut log);
        let id1 = match first {
            FileOutcome::Filed(id) => id,
            _ => panic!(),
        };
        let second = file_one(&bd, &mailer, &clock, &c, "incident:x", "x failed", b"payload", "spira,incident", &mut log);
        match second {
            FileOutcome::Filed(id2) => assert_eq!(id1, id2, "recurrence must bump the SAME bead"),
            _ => panic!("expected Filed"),
        }
        assert_eq!(bd.created.borrow().len(), 1, "only one bead ever created");
    }

    #[test]
    fn three_identical_filings_write_the_payload_note_exactly_once() {
        let bd = FakeBd::new();
        let mailer = FakeMailer { sent: RefCell::new(vec![]) };
        let clock = FixedClock(1_000_000);
        let known = vec!["spira".to_string()];
        let mut log = vec![];
        let c = cfg(&known);
        let payload = vec![b'x'; 500];
        let mut id = String::new();
        for _ in 0..3 {
            let out = file_one(&bd, &mailer, &clock, &c, "incident:bound", "t", &payload, "spira,incident", &mut log);
            id = match out {
                FileOutcome::Filed(id) => id,
                _ => panic!(),
            };
        }
        let notes = bd.notes.borrow().get(&id).cloned().unwrap_or_default().join("\n");
        let payload_str = "x".repeat(500);
        let occurrences = notes.matches(&payload_str).count();
        assert_eq!(occurrences, 1, "notes:\n{notes}");
    }

    #[test]
    fn unreachable_database_stays_spooled() {
        let bd = FakeBd { reachable: false, next_id: RefCell::new(1), ..Default::default() };
        let mailer = FakeMailer { sent: RefCell::new(vec![]) };
        let clock = FixedClock(1_000_000);
        let known = vec!["spira".to_string()];
        let mut log = vec![];
        let out = file_one(&bd, &mailer, &clock, &cfg(&known), "incident:x", "x failed", b"payload", "spira,incident", &mut log);
        assert!(matches!(out, FileOutcome::Unreachable));
        assert_eq!(bd.created.borrow().len(), 0);
    }

    /// sp-jgjvh: a lifecycle machine that does not answer cannot say no incident holds the
    /// reference — the event stays spooled, never filed as a fresh bead.
    #[test]
    fn an_unanswering_lifecycle_machine_stays_spooled() {
        let bd = FakeBd { reachable: true, lc_down: true, next_id: RefCell::new(1), ..Default::default() };
        let mailer = FakeMailer { sent: RefCell::new(vec![]) };
        let clock = FixedClock(1_000_000);
        let known = vec!["spira".to_string()];
        let mut log = vec![];
        let out = file_one(&bd, &mailer, &clock, &cfg(&known), "incident:x", "x failed", b"payload", "spira,incident", &mut log);
        assert!(matches!(out, FileOutcome::Unreachable));
        assert_eq!(bd.created.borrow().len(), 0);
    }

    /// sp-nmlna: a recurrence inside the lookback that matches an incident whose lifecycle
    /// row is terminal (LANDED) files a FRESH bead citing the closed one as its predecessor
    /// and linked to it; the old bead is not reopened, noted or labelled, and its row stays
    /// terminal. SEEN RED before the fix: the old bead was reopened in bd and noted under a
    /// LANDED row, and no new bead was filed (`created` stayed 0).
    #[test]
    fn a_recurrence_of_a_landed_incident_files_a_fresh_bead_citing_its_predecessor() {
        let bd = FakeBd::new();
        let mailer = FakeMailer { sent: RefCell::new(vec![]) };
        let clock = FixedClock(1_790_812_800); // 2026-10-01T00:00:00Z
        let known = vec!["spira".to_string()];
        let c = cfg(&known);
        let old_labels = vec![format!("ref:{}", decide::ref_hash("incident:landed")), "spira".to_string()];
        let old = BeadRow {
            id: "sp-landed".into(),
            status: BeadStatus::Terminal,
            external_ref: Some("incident:landed".into()),
            labels: old_labels.clone(),
            closed_at: Some("2026-09-30T00:00:00Z".into()),
        };
        bd.rows.borrow_mut().push(old.clone());
        let mut log = vec![];
        let out = file_one(&bd, &mailer, &clock, &c, "incident:landed", "landed failed again", b"fresh payload", "spira,incident", &mut log);
        let new_id = match out {
            FileOutcome::Filed(id) => id,
            _ => panic!("expected Filed, log: {log:?}"),
        };
        assert_ne!(new_id, "sp-landed", "a terminal incident must not absorb the recurrence");
        assert_eq!(bd.created.borrow().len(), 1, "exactly one fresh bead filed");
        let body = bd.bodies.borrow().get(&new_id).cloned().unwrap_or_default();
        assert!(body.contains("sp-landed"), "the fresh bead's body cites its predecessor: {body}");
        assert!(body.contains("fresh payload"), "and still carries the payload: {body}");
        assert!(bd.reopened.borrow().is_empty(), "the old bead is never reopened");
        assert!(bd.notes.borrow().get("sp-landed").is_none(), "no note lands under a terminal row");
        let now_old = bd.rows.borrow().iter().find(|r| r.id == "sp-landed").cloned().unwrap();
        assert_eq!(now_old, old, "the old bead and its row are untouched");
        assert_eq!(*bd.related.borrow(), vec![(new_id.clone(), "sp-landed".to_string())], "the fresh bead is linked to its predecessor");
        assert!(log.iter().any(|l| l.contains(&new_id) && l.contains("sp-landed")), "{log:?}");

        // The next recurrence finds the fresh (unfinished) bead and bumps it — no third bead.
        let again = file_one(&bd, &mailer, &clock, &c, "incident:landed", "landed failed again", b"fresh payload", "spira,incident", &mut log);
        assert!(matches!(again, FileOutcome::Filed(ref id) if *id == new_id));
        assert_eq!(bd.created.borrow().len(), 1);
    }

    /// A handed-on but non-terminal incident (SUBMITTED) still absorbs the recurrence: its
    /// row can still move, so the existing bead stays the incident's identity.
    #[test]
    fn a_recurrence_of_a_submitted_incident_still_reopens_it() {
        let bd = FakeBd::new();
        let mailer = FakeMailer { sent: RefCell::new(vec![]) };
        let clock = FixedClock(1_790_812_800);
        let known = vec!["spira".to_string()];
        let c = cfg(&known);
        bd.rows.borrow_mut().push(BeadRow {
            id: "sp-submitted".into(),
            status: BeadStatus::Closed,
            external_ref: Some("incident:sub".into()),
            labels: vec![],
            closed_at: Some("2026-09-30T00:00:00Z".into()),
        });
        let mut log = vec![];
        let out = file_one(&bd, &mailer, &clock, &c, "incident:sub", "t", b"p", "spira,incident", &mut log);
        assert!(matches!(out, FileOutcome::Filed(ref id) if id == "sp-submitted"));
        assert_eq!(bd.created.borrow().len(), 0);
        assert_eq!(*bd.reopened.borrow(), vec!["sp-submitted".to_string()]);
    }

    #[test]
    fn sin_threshold_escalates_exactly_once() {
        let bd = FakeBd::new();
        let mailer = FakeMailer { sent: RefCell::new(vec![]) };
        let clock = FixedClock(1_000_000);
        let known = vec!["spira".to_string()];
        let mut c = cfg(&known);
        bd.rows.borrow_mut().push(BeadRow {
            id: "sp-existing".into(),
            status: BeadStatus::Open,
            external_ref: Some("incident:y".into()),
            labels: vec![],
            closed_at: None,
        });
        c.sin_at = 1;
        let mut log = vec![];
        let out = file_one(&bd, &mailer, &clock, &c, "incident:y", "y failed", b"payload", "spira,incident", &mut log);
        assert!(matches!(out, FileOutcome::Filed(_)));
        assert_eq!(mailer.sent.borrow().len(), 1, "first crossing escalates once");

        let out2 = file_one(&bd, &mailer, &clock, &c, "incident:y", "y failed", b"payload", "spira,incident", &mut log);
        assert!(matches!(out2, FileOutcome::Filed(_)));
        assert_eq!(mailer.sent.borrow().len(), 1, "a bead already labelled sin is never paged twice");
    }

    #[test]
    fn undeclared_repo_gets_needs_repo_triage() {
        let bd = FakeBd::new();
        let mailer = FakeMailer { sent: RefCell::new(vec![]) };
        let clock = FixedClock(1_000_000);
        let known = vec!["spira".to_string()];
        let mut c = cfg(&known);
        c.repo_declared = None;
        let mut log = vec![];
        let out = file_one(&bd, &mailer, &clock, &c, "incident:z", "z failed", b"payload", "spira,incident", &mut log);
        let id = match out {
            FileOutcome::Filed(id) => id,
            _ => panic!(),
        };
        assert!(bd.label_list("db", &id).contains(&"needs-repo-triage".to_string()));
    }

    #[test]
    fn destructive_title_is_refused_without_needs_ryan() {
        let bd = FakeBd::new();
        let mailer = FakeMailer { sent: RefCell::new(vec![]) };
        let clock = FixedClock(1_000_000);
        let known = vec!["spira".to_string()];
        let mut log = vec![];
        let out = file_one(&bd, &mailer, &clock, &cfg(&known), "incident:danger", "needs the world stopped", b"p", "spira,incident", &mut log);
        assert!(matches!(out, FileOutcome::Refused));
        assert_eq!(bd.created.borrow().len(), 0);
    }

    #[test]
    fn delivers_default_is_action() {
        let bd = FakeBd::new();
        let mailer = FakeMailer { sent: RefCell::new(vec![]) };
        let clock = FixedClock(1_000_000);
        let known = vec!["spira".to_string()];
        let mut log = vec![];
        let out = file_one(&bd, &mailer, &clock, &cfg(&known), "incident:d1", "t", b"p", "spira,incident", &mut log);
        let id = match out {
            FileOutcome::Filed(id) => id,
            _ => panic!(),
        };
        let labels = bd.label_list("db", &id);
        assert!(labels.contains(&"delivers:action".to_string()));
        assert!(!labels.iter().any(|l| l.starts_with("delivers:note:")));
    }

    #[test]
    fn delivers_note_inside_run_creates_the_ledger_directory_and_is_stamped() {
        let run_dir = testkit::TempDir::new("sp-0ekp7-run-rs-note-in");
        let ledger = run_dir.join("sop/applied.jsonl");
        let bd = FakeBd::new();
        let mailer = FakeMailer { sent: RefCell::new(vec![]) };
        let clock = FixedClock(1_000_000);
        let known = vec!["spira".to_string()];
        let mut c = cfg(&known);
        let run_str = run_dir.to_str().unwrap().to_string();
        let ledger_str = ledger.to_str().unwrap().to_string();
        c.spira_run = &run_str;
        c.sop_ledger = &ledger_str;
        c.delivers_pref = Some("note");
        let mut log = vec![];
        let out = file_one(&bd, &mailer, &clock, &c, "incident:d2", "t", b"p", "spira,incident", &mut log);
        let id = match out {
            FileOutcome::Filed(id) => id,
            _ => panic!(),
        };
        let labels = bd.label_list("db", &id);
        assert!(labels.contains(&format!("delivers:note:{ledger_str}")), "{labels:?}");
        assert!(!labels.contains(&"delivers:action".to_string()));
        assert!(ledger.parent().unwrap().is_dir(), "the ledger's directory must exist — the criterion must be reachable, not just look reachable");
    }

    #[test]
    fn delivers_note_outside_run_falls_back_to_action_and_logs_outside() {
        let run_dir = testkit::TempDir::new("sp-0ekp7-run-rs-note-out");
        let bd = FakeBd::new();
        let mailer = FakeMailer { sent: RefCell::new(vec![]) };
        let clock = FixedClock(1_000_000);
        let known = vec!["spira".to_string()];
        let mut c = cfg(&known);
        let run_str = run_dir.to_str().unwrap().to_string();
        c.spira_run = &run_str;
        c.sop_ledger = "/somewhere/else/applied.jsonl";
        c.delivers_pref = Some("note");
        let mut log = vec![];
        let out = file_one(&bd, &mailer, &clock, &c, "incident:d3", "t", b"p", "spira,incident", &mut log);
        let id = match out {
            FileOutcome::Filed(id) => id,
            _ => panic!(),
        };
        let labels = bd.label_list("db", &id);
        assert!(labels.contains(&"delivers:action".to_string()));
        assert!(!labels.iter().any(|l| l.starts_with("delivers:note:")));
        assert!(log.iter().any(|l| l.contains("outside")), "{log:?}");
    }

    #[test]
    fn delivers_action_mode_writes_that_label_verbatim() {
        let bd = FakeBd::new();
        let mailer = FakeMailer { sent: RefCell::new(vec![]) };
        let clock = FixedClock(1_000_000);
        let known = vec!["spira".to_string()];
        let mut c = cfg(&known);
        c.delivers_pref = Some("action");
        let mut log = vec![];
        let out = file_one(&bd, &mailer, &clock, &c, "incident:d5", "t", b"p", "spira,incident", &mut log);
        let id = match out {
            FileOutcome::Filed(id) => id,
            _ => panic!(),
        };
        let labels = bd.label_list("db", &id);
        assert!(labels.contains(&"delivers:action".to_string()));
    }

    #[test]
    fn delivers_unrecognised_value_writes_no_label_and_logs_it() {
        let bd = FakeBd::new();
        let mailer = FakeMailer { sent: RefCell::new(vec![]) };
        let clock = FixedClock(1_000_000);
        let known = vec!["spira".to_string()];
        let mut c = cfg(&known);
        c.delivers_pref = Some("bogus");
        let mut log = vec![];
        let out = file_one(&bd, &mailer, &clock, &c, "incident:d4", "t", b"p", "spira,incident", &mut log);
        let id = match out {
            FileOutcome::Filed(id) => id,
            _ => panic!(),
        };
        let labels = bd.label_list("db", &id);
        assert!(!labels.iter().any(|l| l.starts_with("delivers:")), "{labels:?}");
        assert!(log.iter().any(|l| l.contains("unrecognised")), "{log:?}");
    }

    #[test]
    fn epoch_date_matches_known_values() {
        // Verified against `date -u -d @1790812800 +%Y-%m-%d` => 2026-10-01.
        assert_eq!(epoch_to_date(1_790_812_800), "2026-10-01");
        assert_eq!(parse_iso8601("2026-10-01T00:00:00Z"), Some(1_790_812_800));
        assert_eq!(parse_iso8601("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn since_date_subtracts_lookback_days() {
        // 1_790_812_800 - 7*86400 = 1_790_208_000 => `date -u` 2026-09-24.
        assert_eq!(since_date(1_790_812_800, 7), "2026-09-24");
    }

    #[test]
    fn provenance_format() {
        assert_eq!(provenance("foo.service", "host1", "/tmp/x"), "foo.service on host1: /tmp/x");
    }
}
