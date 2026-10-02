//! One handler per subcommand, over the ports in `ports.rs`. Each takes what it needs and
//! returns a `Report` — stdout lines, stderr lines, exit code — so `main.rs` is a thin
//! dispatch and every handler is testable through fakes.

use crate::ledger::{digest_lines, log_lines, LogFilter, LogOutcome, Record};
use crate::ports::{Bd, Clock, Proc};
use crate::shelf;
use crate::validate::{match_line, metric_line, validate};

pub struct Report {
    pub out: Vec<String>,
    pub err: Vec<String>,
    pub code: i32,
}

impl Report {
    fn ok() -> Self {
        Report { out: Vec::new(), err: Vec::new(), code: 0 }
    }
    fn fail(mut self, code: i32, msg: impl Into<String>) -> Self {
        self.err.push(format!("sop: {}", msg.into()));
        self.code = code;
        self
    }
    fn say(&mut self, msg: impl Into<String>) {
        self.out.push(msg.into());
    }
}

pub struct Env {
    pub word_cap: usize,
    pub why_cap: usize,
    pub actor: String,
    pub cockpit_bin: String,
    pub cockpit_bash_prefix: bool,
}

// ───────────────────────────── write ─────────────────────────────

pub fn write(bd: &dyn Bd, proc: &dyn Proc, env: &Env, key: &str, text: &str) -> Report {
    let key = shelf::slugify(key);
    let mut r = Report::ok();
    if text.trim().is_empty() {
        return r.fail(1, "refusing — empty SOP");
    }
    let violations = validate(text, env.word_cap);
    if !violations.is_empty() {
        r.err.push("sop: refusing — the SOP does not validate:".to_string());
        for v in &violations {
            r.err.push(format!("     {}", v.0));
        }
        r.code = 1;
        return r;
    }
    match proc.inventory_scan(text) {
        Ok(hits) if !hits.is_empty() => {
            r.err.push("sop: refusing — SOP text names operator infrastructure:".to_string());
            for h in &hits {
                r.err.push(format!("     {h}"));
            }
            r.err.push("     Use env vars ($SPIRA_DB, $SPIRA_HOME, …) instead of absolute paths.".to_string());
            r.code = 1;
            return r;
        }
        Err(e) => return r.fail(1, e),
        Ok(_) => {}
    }
    if !bd.remember(&key, text) {
        return r.fail(1, format!("failed to write {key}"));
    }
    let words = text.split_whitespace().count();
    r.say(format!("wrote {key} ({words} words)"));
    match match_line(text) {
        Some(re) => r.say(format!("  matches: {re}")),
        None => r.say("  no MATCH: line — recall falls back to key tokens".to_string()),
    }
    r
}

// ───────────────────────────── show ─────────────────────────────

pub fn show(bd: &dyn Bd, key: &str) -> Report {
    let key = shelf::slugify(key);
    let mut r = Report::ok();
    match bd.recall(&key) {
        Some(text) => {
            r.say(text.trim_end().to_string());
            r
        }
        None => r.fail(1, format!("no such SOP: {key}")),
    }
}

// ───────────────────────────── list ─────────────────────────────

pub fn list(bd: &dyn Bd) -> Report {
    let mut r = Report::ok();
    let Some(raw) = bd.memories_json() else {
        return r.fail(1, "cannot read the shelf (bd unreachable or timed out); refusing to report 0 SOPs".to_string());
    };
    let map = shelf::parse_or_empty(&raw);
    for (k, v) in &map {
        let words = v.split_whitespace().count();
        let sym = shelf::symptom_of(v);
        let sym: String = sym.chars().take(60).collect();
        r.say(format!("  {k:<40} {words:>3}w  {sym}"));
    }
    r.say(String::new());
    r.say(format!("{} SOP(s) on the shelf", map.len()));
    r
}

// ───────────────────────────── match ─────────────────────────────

pub fn match_cmd(bd: &dyn Bd, payload: &str) -> Report {
    let mut r = Report::ok();
    let raw = bd.memories_json().unwrap_or_default();
    let map = shelf::parse_or_empty(&raw);
    for hit in crate::match_sop::score(&map, payload) {
        r.say(format!("{}\t{}\t{}\t{}", hit.key, hit.how, hit.score, hit.symptom));
    }
    r
}

// ───────────────────────────── applied ─────────────────────────────

pub struct AppliedArgs<'a> {
    pub key: &'a str,
    pub bead: Option<&'a str>,
    pub pass: Option<&'a str>,
    pub check: &'a str,
    pub held: &'a str,
    pub why: Option<&'a str>,
}

pub fn applied(
    bd: &dyn Bd,
    proc: &dyn Proc,
    clock: &dyn Clock,
    env: &Env,
    ledger_path: &std::path::Path,
    a: &AppliedArgs,
) -> Report {
    let key = shelf::slugify(a.key);
    let mut r = Report::ok();

    if a.bead.is_none() && a.pass.is_none() {
        return r.fail(
            1,
            "applied needs --bead <id> or --pass <id> — a record nobody can join back to an incident counts nothing",
        );
    }
    match a.check {
        "pass" | "fail" => {}
        _ => return r.fail(1, "applied needs --check pass|fail — did the SOP's CHECK confirm this really is that failure?"),
    }
    match a.held {
        "yes" | "no" | "unknown" => {}
        _ => {
            r.err.push("sop: applied needs --held yes|no|unknown — did the FIX resolve it?".to_string());
            r.err.push("     yes      it fit, it held, and it taught us nothing new. Say this; it is a real outcome.".to_string());
            r.err.push("     no       it fit and the fix did NOT hold. The runbook needs amending.".to_string());
            r.err.push("     unknown  too early to tell, or the CHECK did not confirm so no FIX was run.".to_string());
            r.code = 1;
            return r;
        }
    }
    if a.check == "fail" && a.held == "yes" {
        r.err.push("sop: refusing — --check fail --held yes. A CHECK that did not confirm means the".to_string());
        r.err.push("     SOP does not apply and its FIX was never run, so nothing of it can have held.".to_string());
        r.err.push("     Record --held unknown and diagnose instead.".to_string());
        r.code = 1;
        return r;
    }

    let mut held = a.held.to_string();
    let why = a.why.unwrap_or("").to_string();

    let raw = bd.memories_json();
    let shelf_state: &str;
    let mut shelf_obj: Option<serde_json::Value> = None;
    match raw {
        None => {
            shelf_state = "unreadable";
            r.err.push(format!("sop: warning — could not read the shelf from the store; recording {key} unverified"));
        }
        Some(text) => {
            let parsed: Option<serde_json::Value> = serde_json::from_str(&text).ok();
            match parsed {
                Some(v) if v.is_object() => {
                    if !v.as_object().unwrap().contains_key(&key) {
                        return r.fail(1, format!("refusing — no such SOP: {key}. `sop list` shows the shelf."));
                    }
                    shelf_state = "ok";
                    shelf_obj = Some(v);
                }
                _ => {
                    shelf_state = "unreadable";
                    r.err.push(format!("sop: warning — could not read the shelf from the store; recording {key} unverified"));
                }
            }
        }
    }

    let mut metric_note = String::new();
    if held == "yes" && shelf_state == "ok" {
        if let Some(sop_text) = shelf_obj.as_ref().and_then(|v| v.get(&key)).and_then(|v| v.as_str()) {
            if let Some(spec) = metric_line(sop_text) {
                let parts: Vec<&str> = spec.split_whitespace().collect();
                if parts.len() == 2 {
                    let (metric_key, metric_subcmd) = (parts[0], parts[1]);
                    let cockpit_out = proc.metric_probe(&env.cockpit_bin, env.cockpit_bash_prefix, metric_subcmd, 30);
                    let metric_raw = cockpit_out.as_deref().and_then(|o| extract_metric_value(o, metric_key));
                    let fixed = metric_raw.as_deref().map(is_zero).unwrap_or(false);
                    if fixed {
                        metric_note = format!("METRIC {metric_key}=0: fix confirmed by cockpit-collect probe {metric_subcmd}.");
                    } else {
                        held = "unknown".to_string();
                        let shown = metric_raw.clone().unwrap_or_else(|| "?".to_string());
                        metric_note = format!(
                            "METRIC {metric_key}={shown} (still nonzero or unreadable); held downgraded from yes to \
unknown — the fix has not cleared the metric that triggered this incident."
                        );
                    }
                }
            }
        }
    }

    let (epoch, ts) = clock.now();
    let actor = env.actor.clone();

    let verdict = match (a.check, held.as_str()) {
        ("fail", _) => "The CHECK did not confirm: this SOP does not apply to this incident. Its MATCH fired anyway, which is a fact about the regex.".to_string(),
        ("pass", "yes") => "The SOP fit, it held, and it taught us nothing new.".to_string(),
        ("pass", "no") => "The SOP fit and its FIX did NOT hold. The runbook needs amending, not the threshold.".to_string(),
        ("pass", "unknown") => "The CHECK confirmed. Whether the FIX held is not known yet.".to_string(),
        _ => String::new(),
    };

    let mut note_state = "n/a".to_string();
    if let Some(bead) = a.bead {
        note_state = "ok".to_string();
        let mut note = format!("SOP {key} applied — CHECK {}, held={held}.\n\n{verdict}\n", a.check);
        if !metric_note.is_empty() {
            note.push_str(&format!("\n{metric_note}\n"));
        }
        if !why.trim().is_empty() {
            note.push_str(&format!("\n{why}\n"));
        }
        note.push_str(&format!("\nRecorded {ts} by {actor}. Ledger: {}\n", ledger_path.display()));
        if !bd.note(bead, &note) {
            note_state = "failed".to_string();
        }
    }

    let rec = Record {
        ts,
        epoch,
        sop: key.clone(),
        bead: a.bead.map(String::from),
        pass: a.pass.map(String::from),
        check: a.check.to_string(),
        held: held.clone(),
        actor,
        shelf: shelf_state.to_string(),
        note: note_state.clone(),
        why,
    };
    let line = rec.to_line(env.why_cap);

    if let Some(parent) = ledger_path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return r.fail(1, "cannot create ledger directory");
        }
    }
    use std::io::Write as _;
    let appended = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(ledger_path)
        .and_then(|mut f| writeln!(f, "{line}"));
    if appended.is_err() {
        return r.fail(1, format!("failed to append to {}", ledger_path.display()));
    }

    let target = a.bead.map(String::from).unwrap_or_else(|| format!("pass={}", a.pass.unwrap_or_default()));
    r.say(format!("recorded {key} on {target} — check={} held={held}", a.check));
    r.say(format!("  {verdict}"));
    if note_state == "failed" {
        r.err.push(format!(
            "sop: the ledger line was written but the note on {} was NOT — the human reading",
            a.bead.unwrap_or_default()
        ));
        r.err.push("     that incident will not see this. Add it by hand.".to_string());
        r.code = 1;
    }
    r
}

/// `grep "^<key>=" | sed "s/^<key>=//" | head -1` — the first line's value.
fn extract_metric_value(output: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    output.lines().find_map(|l| l.strip_prefix(prefix.as_str()).map(String::from))
}

fn is_zero(v: &str) -> bool {
    !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()) && v.parse::<u64>().map(|n| n == 0).unwrap_or(false)
}

// ───────────────────────────── log ─────────────────────────────

pub fn log(ledger_text: Option<&str>, f: &LogFilter) -> Report {
    let mut r = Report::ok();
    let Some(text) = ledger_text else {
        return r.fail(2, "no ledger — nothing has ever been recorded, or it is not where this program looks.");
    };
    match log_lines(text, f) {
        LogOutcome::Found(lines) => {
            for l in lines {
                r.say(l);
            }
            r
        }
        LogOutcome::Empty => r.fail(1, "no matching record"),
        LogOutcome::Corrupt(n) => r.fail(2, format!("{n} ledger line(s) and not one parsed as JSON — this ledger is corrupt, not empty")),
    }
}

// ───────────────────────────── digest ─────────────────────────────

pub fn digest(bd: &dyn Bd) -> Report {
    let mut r = Report::ok();
    let raw = bd.memories_json();
    let Some(raw) = raw else {
        return r.fail(2, "could not read the shelf — this is not an empty shelf");
    };
    let Some(map) = shelf::parse(&raw) else {
        return r.fail(2, "the shelf did not parse cleanly — this is not an empty shelf");
    };
    for l in digest_lines(&map) {
        r.say(l);
    }
    r
}

// ───────────────────────────── ledger-init ─────────────────────────────

pub fn ledger_init(ledger_path: &std::path::Path) -> Report {
    let mut r = Report::ok();
    if ledger_path.exists() {
        r.say(format!("ledger present: {}", ledger_path.display()));
        return r;
    }
    if let Some(parent) = ledger_path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return r.fail(1, "cannot create ledger directory");
        }
    }
    if std::fs::OpenOptions::new().create(true).append(true).open(ledger_path).is_err() {
        return r.fail(1, format!("cannot create the ledger at {}", ledger_path.display()));
    }
    r.say(format!("created empty ledger: {}", ledger_path.display()));
    r
}

// ───────────────────────────── retire ─────────────────────────────

pub fn retire(bd: &dyn Bd, key: &str) -> Report {
    let key = shelf::slugify(key);
    let mut r = Report::ok();
    if !bd.forget(&key) {
        return r.fail(1, format!("no such SOP: {key}"));
    }
    r.say(format!("retired {key}"));
    r.say(String::new());
    r.say("Retire an SOP the way a statute is retired: remove it. Do not leave it standing".to_string());
    r.say("with a correction attached — that is a stale runbook with a warning label.".to_string());
    r
}

// ───────────────────────────── synth ─────────────────────────────

pub fn synth(bd: &dyn Bd, clock: &dyn Clock, out_path: Option<&str>) -> Report {
    let mut r = Report::ok();
    let Some(out_path) = out_path else {
        r.say("sop: no wiki configured (SPIRA_WIKI) — nothing to synthesise into.".to_string());
        r.say("sop: the SOPs themselves are in the database; `sop list` reads them.".to_string());
        return r;
    };
    let raw = bd.memories_json().unwrap_or_default();
    let map = shelf::parse_or_empty(&raw);
    let today = clock.today();
    let page = crate::synth::render(&map, &today);
    if std::fs::write(out_path, page).is_err() {
        return r.fail(1, format!("failed to write {out_path}"));
    }
    r.say(format!("sop-synth: wrote {out_path} — {} SOP(s)", map.len()));
    r
}

// ───────────────────────────── lint ─────────────────────────────

pub fn lint(bd: &dyn Bd, word_cap: usize) -> Report {
    let mut r = Report::ok();
    let raw = bd.memories_json();
    let Some(raw) = raw else {
        return r.fail(1, "lint: could not read the shelf — refusing to report clean");
    };
    let Some(map) = shelf::parse(&raw) else {
        return r.fail(1, "lint: shelf did not parse cleanly — refusing to report clean");
    };
    let total = map.len();
    let mut failed = 0;
    for (k, v) in &map {
        let violations = validate(v, word_cap);
        if violations.is_empty() {
            continue;
        }
        failed += 1;
        for viol in &violations {
            r.say(format!("FAIL  {k}: {}", viol.0));
        }
    }
    if failed > 0 {
        r.say(String::new());
        r.err.push(format!("sop: {failed} of {total} SOP(s) failed — fix with `sop write` or remove with `sop retire`"));
        r.code = 1;
        return r;
    }
    if total > 0 {
        r.say(format!("ok — {total} SOP(s) on the shelf, all valid"));
    } else {
        r.say("ok — shelf is empty".to_string());
    }
    r
}

// ───────────────────────────── validate (subcommand) ─────────────────────────────

pub fn validate_cmd(proc: &dyn Proc, key: &str, text: &str, word_cap: usize) -> Report {
    let key = shelf::slugify(key);
    let mut r = Report::ok();
    let violations = validate(text, word_cap);
    let mut rc = 0;
    if violations.is_empty() {
        r.say(format!("ok    {key}"));
    } else {
        for v in &violations {
            r.say(format!("FAIL  {key}: {}", v.0));
        }
        rc = 1;
    }
    if !text.trim().is_empty() {
        match proc.inventory_scan(text) {
            Ok(hits) if !hits.is_empty() => {
                r.say(format!("FAIL  {key}: names operator infrastructure:"));
                for h in &hits {
                    r.say(format!("        {h}"));
                }
                rc = 1;
            }
            _ => {}
        }
    }
    r.code = rc;
    r
}

#[cfg(test)]
mod tests;
