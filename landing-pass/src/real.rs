//! Production implementations of the ports: bd, git, the lib.sh seam, the harness
//! programs, /proc and the clock.

use crate::model::{BeadRow, LandMode, Rebase, Recut, RepoRow, Settings};
use crate::ports::{Beads, Clock, Git, Lib, Procs, Tools};
use crate::report::Reporter;
use crate::seam::{self, Op, FIELD};
use crate::util::{command, run_capture, unix_now};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

// ──────────────────────────────────────────────────────────────────────────────
// The lib.sh seam
// ──────────────────────────────────────────────────────────────────────────────

pub struct SeamRunner<'a> {
    pub home: PathBuf,
    pub out: &'a Reporter,
}

impl<'a> SeamRunner<'a> {
    /// Run one seam op; lib.sh log lines are passed through to the pass's stdout and
    /// `progress` lines become movements. Returns (status, answer).
    pub fn call(&self, op: Op, values: &[&str]) -> (i32, String) {
        let home = self.home.to_string_lossy().into_owned();
        let mut all: Vec<&str> = vec![&home];
        all.extend_from_slice(values);
        let bytes = seam::stdin_bytes(op, &all);
        let mut c = command("bash");
        c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        let mut child = match c.spawn() {
            Ok(ch) => ch,
            Err(e) => {
                self.out.log(&format!("landing: the lib.sh seam could not start ({e})"));
                return (-1, String::new());
            }
        };
        crate::util::CURRENT_CHILD.store(child.id() as i32, std::sync::atomic::Ordering::SeqCst);
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(&bytes);
        }
        let o = child.wait_with_output();
        crate::util::CURRENT_CHILD.store(0, std::sync::atomic::Ordering::SeqCst);
        let Ok(o) = o else { return (-1, String::new()) };
        let printed = seam::split(&String::from_utf8_lossy(&o.stdout));
        for l in &printed.logs {
            self.out.raw(l);
        }
        for p in &printed.progress {
            self.out.progress(p);
        }
        (o.status.code().unwrap_or(-1), printed.answer)
    }
}

/// Run a harness binary by bare name (sp-gypjk's convention), relaying its stdout through
/// `out.raw` line by line exactly as a lib.sh seam call's own log lines were, and its
/// stderr (if any) as one line — `gh-intake closeout`/`unlanded-scan` (sp-j3fim, "wave
/// 4.31") are the first callers that need this outside the seam itself.
fn run_bin(out: &Reporter, bin: &str, args: &[&str]) {
    let mut c = command(bin);
    c.args(args);
    let (_rc, stdout, stderr) = run_capture(c);
    for line in String::from_utf8_lossy(&stdout).lines() {
        out.raw(line);
    }
    let stderr = String::from_utf8_lossy(&stderr);
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        out.raw(&format!("{bin}: {stderr}"));
    }
}

/// Resolve settings and every repository through lib.sh (seam S1): the one resolver.
pub fn load_context(home: &Path, out: &Reporter) -> Result<(Settings, Vec<RepoRow>), String> {
    let sr = SeamRunner { home: home.to_path_buf(), out };
    let (rc, answer) = sr.call(Op::Context, &[]);
    if rc != 0 {
        return Err(format!("the context seam exited {rc} (is {}/lib.sh loadable?)", home.display()));
    }
    parse_context(&answer, home)
}

/// The repo registry, in-process (sp-o88bx, "wave 4.12": family W — `spira_landref`/
/// `ref_remote`/`ref_branch`/`qualify_base_ref`/`spira_publish_forge` — the CONTEXT seam's
/// per-repo loop used to shell into, once per function per repository). `Registry::from_env`
/// (sp-k6lku, following the structural fix for sp-z3eyk) is the one production door onto
/// a registry: building one from a bare `std::env::vars()` directly, with no resolution,
/// found NO map and NO landref in production (conf.sh exports none of `SPIRA_REPO_MAP`/
/// `SPIRA_HOME_REPO`/`SPIRA_REPO`/`SPIRA_REPO_DERIVED`); `from_env` resolves them
/// in-process instead.
fn repo_registry(home: &Path) -> spira_config::repos::Registry {
    spira_config::repos::Registry::from_env(std::env::vars().collect(), home)
}

/// The base-ref columns (family W) for one already-resolved `(name, path, mode)` — ported
/// verbatim from the CONTEXT seam's own per-repo loop (sp-o88bx, "wave 4.12"): nothing when
/// `path` is empty or not a real checkout; `forge_ref` is the qualified publish target under
/// `queue.local`, else the same as `base_fq`.
fn resolve_base_refs(
    reg: &spira_config::repos::Registry,
    name: &str,
    path: &Path,
    mode: &LandMode,
) -> (Option<String>, Option<String>, Option<String>, String, Option<String>) {
    if path.as_os_str().is_empty() || !path.join(".git").exists() {
        return (None, None, None, String::new(), None);
    }
    let path_s = path.to_string_lossy().into_owned();
    let Some(landref) = spira_config::repos::landref(reg, &path_s) else {
        return (None, None, None, String::new(), None);
    };
    let base_remote = spira_config::repos::ref_remote(&landref, Some(&path_s));
    let base_branch = spira_config::repos::ref_branch(&landref);
    let base_fq = spira_config::repos::qualify_base_ref(&landref, &path_s);
    let forge_ref = if *mode == LandMode::QueueLocal {
        spira_config::repos::publish_forge(reg, name, &std::env::vars().collect()).map(|(remote, branch)| format!("refs/remotes/{remote}/{branch}"))
    } else {
        Some(base_fq.clone())
    };
    (Some(landref), Some(base_fq), base_remote, base_branch, forge_ref)
}

pub fn parse_context(answer: &str, home: &Path) -> Result<(Settings, Vec<RepoRow>), String> {
    let mut kv: BTreeMap<String, String> = BTreeMap::new();
    for rec in answer.split('\0') {
        let Some((k, v)) = rec.split_once('=') else { continue };
        kv.insert(k.into(), v.into());
    }
    let run = kv.get("run").cloned().unwrap_or_default();
    if run.is_empty() {
        return Err("the context seam named no SPIRA_RUN".into());
    }
    // spira_home_repo/spira_repos/repo_root/repo_land (family U) in-process (sp-k6lku,
    // "wave 4.13"): the seam above no longer emits "repo=" records or "home_repo" at all.
    let reg = repo_registry(home);
    let repos: Vec<RepoRow> = reg
        .all()
        .into_iter()
        .map(|name| {
            let path = reg.root(&name).map(PathBuf::from).unwrap_or_default();
            let mode = LandMode::parse(&reg.land(&name));
            let (landref, base_fq, base_remote, base_branch, forge_ref) = resolve_base_refs(&reg, &name, &path, &mode);
            RepoRow { name, path, mode, landref, base_fq, base_remote, base_branch, forge_ref }
        })
        .collect();
    let g = |k: &str| kv.get(k).cloned().unwrap_or_default();
    let num = |k: &str, d: i64| kv.get(k).and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(d);
    let path_opt = |k: &str| kv.get(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let s = Settings {
        home: path_opt("home").unwrap_or_else(|| home.to_path_buf()),
        run: PathBuf::from(&run),
        repo: PathBuf::from(g("repo")),
        db: g("db"),
        bd: kv.get("bd").filter(|v| !v.is_empty()).cloned().unwrap_or_else(|| "bd".into()),
        bd_timeout: num("bd_timeout", 180).max(1) as u64,
        home_repo: reg.home_repo().to_string(),
        id_prefix: g("id_prefix"),
        land_maxsec: num("land_maxsec", 3600),
        gate_reserve: num("gate_reserve", 1200),
        gate_lock_wait: kv.get("gate_lock_wait").filter(|v| !v.is_empty()).cloned(),
        certify_par: certify_par(kv.get("certify_par").map(String::as_str)),
        verdict_ttl: num("verdict_ttl", 0).max(0) as u64,
        verdicts: path_opt("verdicts").unwrap_or_else(|| PathBuf::from(&run).join("verdicts")),
        deferral_escalate_at: num("deferral_at", 5).max(0) as u32,
        express_label: g("express_label"),
        cutover_label: g("cutover_label"),
        submitted_label: g("submitted_label"),
        rebase_escalate_at: num("rebase_escalate_at", 3).max(0) as u32,
        git_name: g("git_name"),
        git_email: g("git_email"),
        incident: PathBuf::from(g("incident")),
        scope_label: g("scope_label"),
        // Every harness tool by name, on the launcher's PATH (sp-gypjk).
        queue_bin: Some(PathBuf::from("queue")),
        queue_dir: path_opt("queue_dir").unwrap_or_else(|| PathBuf::from(&run).join("queue")),
        lc_bin: Some(PathBuf::from("spira-lc")),
        halt_grace: num("halt_grace", 30).max(0) as u64,
        path: kv.get("path").filter(|v| !v.is_empty()).cloned(),
        bdjson_fixture: path_opt("bdjson_fixture"),
        pr_refresh_max: num("pr_refresh_max", 5).max(0) as u32,
        toml: path_opt("toml"),
        lifecycle_enforce: false,
        // Empty only when conf.sh itself did not run (a stand-in lib.sh in a unit test);
        // in production conf.sh always sets SPIRA_ASK_LABEL before this seam reads it, so
        // no fallback literal belongs here (law-schema-over-code).
        ask_label: g("ask_label"),
        noverdict_max: num("noverdict_max", 3).max(1) as u32,
        noverdict_class_window: num("noverdict_class_window", 86_400).max(0),
        rebase_decompose_files: num("rebase_decompose_files", 4).max(1) as u32,
        rebase_generated_files: g("rebase_generated_files"),
    };
    Ok((s, repos))
}

/// `SPIRA_CERTIFY_PAR` as the landing pass reads it: a positive integer, else 4 (D14 (b)).
pub fn certify_par(v: Option<&str>) -> usize {
    match v.map(str::trim).and_then(|v| v.parse::<usize>().ok()) {
        Some(n) if n > 0 => n,
        _ => 4,
    }
}

pub struct RealLib<'a> {
    pub seam: SeamRunner<'a>,
    /// incident.sh — the intake the base's own red is filed through.
    pub incident: PathBuf,
    /// sp-31hjr (family C, asks/escalation): the settings and bead reads the native
    /// `ask_*`/`noverdict` methods need, that the seam used to resolve inside lib.sh.
    pub s: Settings,
    pub beads: RealBeads,
}

fn p(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

impl<'a> RealLib<'a> {
    /// lib.sh `ask_already_open <subject>` (sp-31hjr) — the strongest dedupe is "is it
    /// already in front of him", so this asks the database.
    fn ask_already_open(&self, subject: &str) -> bool {
        self.beads.ask_open(&self.s.ask_label, subject)
    }

    /// `mail send <to> --from <from> --subject <subject> --kind <kind> [--default <d>]`,
    /// `body` on stdin. Every escalation path below goes through this one subprocess call
    /// (sp-gypjk: by bare name), so an escalation that silently drops a message would show
    /// up here, not three times over.
    fn send_mail(&self, to: &str, from: &str, subject: &str, kind: &str, default: Option<&str>, body: &str) -> bool {
        let mut c = command("mail");
        c.arg("send").arg(to).arg("--from").arg(from).arg("--subject").arg(subject).arg("--kind").arg(kind);
        if let Some(d) = default {
            c.arg("--default").arg(d);
        }
        c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
        let Ok(mut child) = c.spawn() else { return false };
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(body.as_bytes());
        }
        child.wait().map(|s| s.success()).unwrap_or(false)
    }

    /// lib.sh `spira_ask_machinery` — a per-branch machinery-fault ask, deduped on
    /// "<branch> cannot be judged".
    #[allow(clippy::too_many_arguments)]
    fn ask_machinery(&self, id: &str, branch: &str, repo: &str, outcome: &str, reason: &str, n: u32, out: &str) {
        if self.ask_already_open(&crate::ask::machinery_subject(branch)) {
            return;
        }
        let ev = crate::util::tail_lines(out, 20);
        let (subj, dflt, body) = crate::ask::machinery_mail(id, branch, repo, outcome, reason, n, &ev);
        self.send_mail("operator", "Landing gate <gate@spira>", &subj, "question", Some(&dflt), &body);
    }

    /// lib.sh `spira_ask_machinery_class` — one ask per (repo, reason) class, naming every
    /// branch it touched, deduped on "<repo> cannot be judged: <outcome> (<reason>)".
    fn ask_machinery_class(&self, repo: &str, reason: &str, branches: &str, outcome: &str, n: u32, out: &str) {
        if self.ask_already_open(&crate::ask::machinery_class_subject(repo, outcome, reason)) {
            return;
        }
        let ev = crate::util::tail_lines(out, 20);
        let (subj, dflt, body) = crate::ask::machinery_class_mail(repo, reason, branches, outcome, n, &ev);
        self.send_mail("operator", "Landing gate <gate@spira>", &subj, "question", Some(&dflt), &body);
    }
}

impl<'a> Lib for RealLib<'a> {
    /// In-process now (sp-cnnt6, "wave 4.16"): this crate owns the landstate ledger, so its
    /// own writes never need the lib.sh seam — only the other five crates' seams do, and
    /// those shell to `landing-pass mark` instead.
    fn land_mark(&self, id: &str, state: &str, tip: &str, reason: &str) {
        crate::landstate::land_mark(&self.s.run, id, state, tip, reason, "");
    }
    fn reopen(&self, id: &str, cause: &str, note: &str) {
        self.seam.call(Op::Reopen, &[id, cause, note]);
    }
    fn event(&self, kind: &str, id: &str, title: &str, detail: &str) {
        self.seam.call(Op::Event, &[kind, id, title, detail]);
    }
    /// lib.sh `spira_land_noverdict` — ported natively (sp-31hjr; was the S5 seam). A
    /// harness-fault reason is counted and escalated BY CLASS (repo+reason), since a dead
    /// container makes every branch fail identically; any other reason is per-branch. The
    /// class window resets after `noverdict_class_window` so a fault that went away and
    /// came back later escalates again rather than being silenced forever.
    fn noverdict(&self, id: &str, branch: &str, repo: &str, reason: &str, outcome: &str, out: &str) {
        let dir = self.s.run.join("noverdict");
        let _ = std::fs::create_dir_all(&dir);
        let max = self.s.noverdict_max as u64;

        if reason == "harness-fault" {
            let key = crate::util::branch_key(&format!("{repo}-{reason}"));
            let file = dir.join(&key);
            let asked = dir.join(format!("{key}.asked"));
            let branches_file = dir.join(format!("{key}.branches"));
            if let Ok(md) = std::fs::metadata(&asked) {
                let age = unix_now().saturating_sub(md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0));
                if age as i64 >= self.s.noverdict_class_window {
                    let _ = std::fs::remove_file(&file);
                    let _ = std::fs::remove_file(&asked);
                    let _ = std::fs::remove_file(&branches_file);
                }
            }
            let n = std::fs::read_to_string(&file).ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0) + 1;
            let _ = crate::util::atomic_write(&file, &n.to_string());
            let existing = std::fs::read_to_string(&branches_file).unwrap_or_default();
            if !existing.lines().any(|l| l == branch) {
                let mut f = existing;
                f.push_str(branch);
                f.push('\n');
                let _ = crate::util::atomic_write(&branches_file, &f);
            }
            if n >= max && !asked.exists() {
                let _ = std::fs::write(&asked, "");
                let branches_csv = std::fs::read_to_string(&branches_file)
                    .unwrap_or_default()
                    .lines()
                    .filter(|l| !l.is_empty())
                    .collect::<Vec<_>>()
                    .join(",");
                let branches = if branches_csv.is_empty() { branch } else { &branches_csv };
                self.ask_machinery_class(repo, reason, branches, outcome, n as u32, out);
                self.seam.out.progress(&format!("escalated {repo} — {outcome} x{n} in a day ({reason}) across {branches}"));
            }
            return;
        }

        let key = crate::util::branch_key(&format!("{branch}-{reason}"));
        let file = dir.join(&key);
        let asked = dir.join(format!("{key}.asked"));
        let n = std::fs::read_to_string(&file).ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0) + 1;
        let _ = crate::util::atomic_write(&file, &n.to_string());
        if n >= max && !asked.exists() {
            let _ = std::fs::write(&asked, "");
            self.ask_machinery(id, branch, repo, outcome, reason, n as u32, out);
            self.seam.out.progress(&format!("escalated {id} — {outcome} x{n} on {branch}"));
        }
    }
    fn incident(&self, labels: &str, repo: &str, ext_ref: &str, title: &str, payload: &str) -> Result<String, i32> {
        let intake = p(&self.incident);
        let (rc, ans) = self.seam.call(Op::Incident, &[&intake, labels, repo, ext_ref, title, payload]);
        if rc == 0 { Ok(ans) } else { Err(rc) }
    }
    /// lib.sh `spira_ask_rebase_loop` — ported natively (sp-31hjr; was the S7 seam). Mail
    /// send **concierge**, kind note — every call, never deduped: the repetition is
    /// reported every time the caller's own escalate-at threshold fires.
    fn ask_rebase_loop(&self, args: &[&str]) {
        let get = |i: usize| args.get(i).copied().unwrap_or("");
        let (id, branch, repo, n, conflicts, others) = (get(0), get(1), get(2), get(3), get(4), get(5));
        let repo_dir = args.get(6).copied().filter(|s| !s.is_empty());
        let base = args.get(7).copied().filter(|s| !s.is_empty());

        let row = self.beads.show(&[id.to_string()]).ok().and_then(|r| r.into_iter().next());
        let bead_title = row.as_ref().map(|r| r.title.as_str()).filter(|t| !t.is_empty());
        let bead_status = row.as_ref().map(|r| r.status.as_str()).filter(|s| !s.is_empty());

        let (mut tip_short, mut ahead, mut nfiles) = (String::new(), String::new(), 0u32);
        if let (Some(rd), Some(base)) = (repo_dir, base) {
            let rd = Path::new(rd);
            let mut c = git(rd);
            c.args(["rev-parse", "--short", branch]);
            tip_short = git_out(c).unwrap_or_default();
            let mut c = git(rd);
            c.args(["rev-list", "--count", &format!("{base}..{branch}")]);
            ahead = git_out(c).unwrap_or_else(|| "?".into());
            let mut c = git(rd);
            c.args(["diff", "--name-only", &format!("{base}...{branch}")]);
            nfiles = git_out(c).unwrap_or_default().lines().filter(|l| !l.trim().is_empty()).count() as u32;
        }
        let rctx = crate::ask::RebaseLoopCtx {
            bead_title,
            bead_status,
            tip_short: Some(tip_short.as_str()).filter(|s| !s.is_empty()),
            ahead: Some(ahead.as_str()).filter(|s| !s.is_empty()),
            nfiles,
        };
        let (subj, body) = crate::ask::rebase_loop_mail(id, branch, repo, n, conflicts, others, base, &rctx, self.s.rebase_decompose_files, &self.s.rebase_generated_files);
        self.send_mail("concierge", "Landing gate <gate@spira>", &subj, "note", None, &body);
    }
    /// lib.sh `spira_ask_red_recurring` — ported natively (sp-31hjr; was the S7 seam).
    fn ask_red_recurring(&self, id: &str, branch: &str, repo: &str, class: &str, first_at: &str) {
        if self.ask_already_open(&crate::ask::red_recurring_subject(branch, class)) {
            return;
        }
        let first_epoch: i64 = first_at.trim().parse().unwrap_or(0);
        let elapsed_h = if first_epoch > 0 { (unix_now() as i64 - first_epoch) / 3600 } else { 0 };
        let (subj, dflt, body) = crate::ask::red_recurring_mail(id, branch, repo, class, elapsed_h);
        self.send_mail("operator", "Landing gate <gate@spira>", &subj, "question", Some(&dflt), &body);
    }
    /// lib.sh `spira_ask_rebase_refused` — ported natively (sp-31hjr; was the S7 seam).
    fn ask_rebase_refused(&self, id: &str, branch: &str, repo: &str, reason: &str) {
        if self.ask_already_open(&crate::ask::rebase_refused_subject(branch)) {
            return;
        }
        let (subj, dflt, body) = crate::ask::rebase_refused_mail(id, branch, repo, reason);
        self.send_mail("operator", "Landing gate <gate@spira>", &subj, "question", Some(&dflt), &body);
    }
    /// lib.sh `spira_ask_budget_deferred` — ported natively (sp-31hjr; was the S7 seam).
    /// Kind `alert`, no `--default` (unlike every other ask in this family).
    fn ask_budget_deferred(&self, branch: &str, repo: &str, n: u32) {
        if self.ask_already_open(&crate::ask::budget_deferred_subject(branch)) {
            return;
        }
        let (subj, body) = crate::ask::budget_deferred_mail(branch, repo, n);
        self.send_mail("operator", "Landing gate <gate@spira>", &subj, "alert", None, &body);
    }
    fn rebase(&self, branch: &str, onto: &str, repo: &Path, name: &str) -> Rebase {
        let (rc, ans) = self.seam.call(Op::Rebase, &[branch, onto, &p(repo), name]);
        let f: Vec<&str> = ans.split(FIELD).collect();
        let get = |i: usize| f.get(i).map(|s| s.to_string()).unwrap_or_default();
        let mut failure = get(0);
        if rc != 0 && failure.is_empty() {
            failure = if rc == 96 { "no-lib".into() } else { "unknown".into() };
        }
        Rebase { ok: rc == 0, failure: if rc == 0 { String::new() } else { failure }, conflicts: get(1), refused_reason: get(2) }
    }
    fn recut(&self, branch: &str, onto: &str, repo: &Path, name: &str) -> Recut {
        let (rc, ans) = self.seam.call(Op::Recut, &[branch, onto, &p(repo), name]);
        let (a, c) = ans.split_once(FIELD).unwrap_or((ans.as_str(), ""));
        Recut { ok: rc == 0, applied: a.trim().parse().unwrap_or(0), conflicts: c.to_string() }
    }
    fn bump_requeue(&self, id: &str, reason: &str) {
        self.seam.call(Op::BumpRequeue, &[id, reason]);
    }
    fn requeues_of(&self, id: &str) -> u32 {
        self.seam.call(Op::RequeuesOf, &[id]).1.trim().parse().unwrap_or(0)
    }
    fn conflict_note(&self, args: &[&str]) -> String {
        self.seam.call(Op::ConflictNote, args).1
    }
    fn other_beads(&self, repo: &Path, branch: &str, base: &str, files: &str) -> String {
        self.seam.call(Op::OtherBeads, &[&p(repo), branch, base, files]).1
    }
    fn pr_merged(&self, repo: &Path, branch: &str) -> bool {
        self.seam.call(Op::PrMerged, &[&p(repo), branch]).0 == 0
    }
    fn note(&self, id: &str, text: &str) {
        self.seam.call(Op::Note, &[id, text]);
    }
    fn push(&self, tree: &Path, remote: &str, refspec: &str) -> Result<(), String> {
        let (rc, err) = self.seam.call(Op::Push, &[&p(tree), remote, refspec]);
        if rc == 0 { Ok(()) } else { Err(err) }
    }
    fn land_subject(&self, id: &str) -> String {
        let s = self.seam.call(Op::LandSubject, &[id]).1;
        if s.is_empty() { format!("spira: land {id}") } else { s }
    }
    fn deliver_delivered(&self, id: &str, sha: &str) {
        self.seam.call(Op::DeliverDelivered, &[id, sha]);
    }
    fn deliver_requeued(&self, id: &str, tip: &str) {
        self.seam.call(Op::DeliverRequeued, &[id, tip]);
    }
    fn deliver_returned(&self, id: &str, reason: &str) {
        self.seam.call(Op::DeliverReturned, &[id, reason]);
    }
    /// `gh-intake closeout <id> <sha> <repo>` (sp-j3fim, "wave 4.31"): gh_issue_closeout
    /// moved natively into gh-intake; this crate shells to the compiled binary by bare
    /// name now, the same way `land_mark` shells to `landing-pass` itself rather than the
    /// lib.sh seam. No lib.sh snippet backs this any more. stdout is captured and relayed
    /// through this pass's own `Reporter::raw`, exactly as the seam's own log lines were.
    fn closeout(&self, id: &str, sha: &str, repo: &Path) {
        run_bin(self.seam.out, "gh-intake", &["closeout", id, sha, &p(repo)]);
    }
    fn close_on_land(&self, id: &str, sha: &str) {
        self.seam.call(Op::CloseOnLand, &[id, sha]);
    }
    fn prune_worktrees(&self, repo: &Path) {
        self.seam.call(Op::PruneWorktrees, &[&p(repo)]);
    }
    /// `gh-intake unlanded-scan` (sp-j3fim, "wave 4.31"): `_gh_unlanded_scan` moved
    /// natively into gh-intake; no lib.sh snippet backs this any more.
    fn gh_unlanded_scan(&self) {
        run_bin(self.seam.out, "gh-intake", &["unlanded-scan"]);
    }
    /// lib.sh `spira_ask_refresh_loop` — ported natively (sp-31hjr; was the S19 seam).
    /// Deduped on the bead id ("<id> refresh cap"), not the branch — one alert per cap.
    fn ask_refresh_loop(&self, repo: &Path, name: &str, branch: &str, id: &str, base_fq: &str, n: u32) {
        if self.ask_already_open(&crate::ask::refresh_loop_subject(id)) {
            return;
        }
        let mut c = git(repo);
        c.args(["rev-list", "--count", &format!("{branch}..{base_fq}")]);
        let behind = git_out(c).unwrap_or_else(|| "?".into());
        let ctx = self.beads.context(id, unix_now() as i64);
        let (subj, dflt, body) = crate::ask::refresh_loop_mail(id, branch, name, base_fq, &behind, n, self.s.pr_refresh_max, &ctx);
        self.send_mail("operator", "Landing gate <gate@spira>", &subj, "question", Some(&dflt), &body);
    }
    fn deliver_pr_merged(&self, repo: &Path, id: &str, branch: &str, merge_sha: &str) {
        self.seam.call(Op::DeliverPrMerged, &[&p(repo), id, branch, merge_sha]);
    }
    fn deliver_pr_closed(&self, id: &str, reason: &str) {
        self.seam.call(Op::DeliverPrClosed, &[id, reason]);
    }
    fn force_push(&self, repo: &Path, remote: &str, branch: &str) -> Result<(), String> {
        let (rc, err) = self.seam.call(Op::ForcePush, &[&p(repo), remote, branch]);
        if rc == 0 { Ok(()) } else { Err(err) }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// The bead store (read only)
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct RealBeads {
    pub home: PathBuf,
    pub db: String,
    pub bd: String,
    pub timeout: u64,
    pub home_repo: String,
    pub submitted_label: String,
    pub fixture: Option<PathBuf>,
}

/// `json_only`: bd can print warnings on stdout before the payload.
pub fn json_only(s: &str) -> &str {
    let mut off = 0;
    for line in s.split_inclusive('\n') {
        if line.starts_with('[') || line.starts_with('{') {
            return &s[off..];
        }
        off += line.len();
    }
    ""
}

impl RealBeads {
    fn show_raw(&self, ids: &[String]) -> Result<String, String> {
        if let Some(fx) = &self.fixture {
            let mut c = command("bdsim.py");
            c.arg(fx).arg("show").args(ids).arg("--json").stdin(Stdio::null());
            let (rc, so, _) = run_capture(c);
            return if rc == 0 { Ok(String::from_utf8_lossy(&so).into_owned()) } else { Err(format!("bdsim exited {rc}")) };
        }
        // bdq's refusal: an empty SPIRA_DB would let bd discover whatever store the working
        // directory resolves to.
        if self.db.is_empty() {
            return Err("SPIRA_DB is empty — refusing to let bd auto-discover a store".into());
        }
        let mut last = String::new();
        for _try in 0..2 {
            let mut c = command("timeout");
            c.arg(self.timeout.to_string()).arg(&self.bd).arg("-C").arg(&self.db).arg("show").args(ids).arg("--json");
            c.stdin(Stdio::null());
            let (rc, so, se) = run_capture(c);
            if rc == 0 {
                return Ok(String::from_utf8_lossy(&so).into_owned());
            }
            last = String::from_utf8_lossy(&se).into_owned();
            // A pooled connection the server already dropped: the query never ran, retrying
            // is as safe as the first attempt (bdq's own rule).
            if !last.contains("invalid connection") {
                break;
            }
        }
        Err(format!("bd show failed: {}", last.lines().next().unwrap_or("")))
    }

    /// `bdq <verb> <args…> --json` (sp-31hjr): the same retry-on-dropped-connection rule
    /// as `show_raw`, for a verb other than `show`.
    fn bdq_raw(&self, verb: &str, args: &[&str]) -> Result<String, String> {
        if let Some(fx) = &self.fixture {
            let mut c = command("bdsim.py");
            c.arg(fx).arg(verb).args(args).arg("--json").stdin(Stdio::null());
            let (rc, so, _) = run_capture(c);
            return if rc == 0 { Ok(String::from_utf8_lossy(&so).into_owned()) } else { Err(format!("bdsim exited {rc}")) };
        }
        if self.db.is_empty() {
            return Err("SPIRA_DB is empty — refusing to let bd auto-discover a store".into());
        }
        let mut last = String::new();
        for _try in 0..2 {
            let mut c = command("timeout");
            c.arg(self.timeout.to_string()).arg(&self.bd).arg("-C").arg(&self.db).arg(verb).args(args).arg("--json");
            c.stdin(Stdio::null());
            let (rc, so, se) = run_capture(c);
            if rc == 0 {
                return Ok(String::from_utf8_lossy(&so).into_owned());
            }
            last = String::from_utf8_lossy(&se).into_owned();
            if !last.contains("invalid connection") {
                break;
            }
        }
        Err(format!("bd {verb} failed: {}", last.lines().next().unwrap_or("")))
    }
}

impl Beads for RealBeads {
    fn show(&self, ids: &[String]) -> Result<Vec<BeadRow>, String> {
        let mut out = Vec::new();
        for chunk in ids.chunks(100) {
            let raw = self.show_raw(chunk)?;
            let js = json_only(&raw);
            if js.trim().is_empty() {
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(js).map_err(|e| format!("bd show: not JSON ({e})"))?;
            let items = match v {
                serde_json::Value::Array(a) => a,
                o @ serde_json::Value::Object(_) => vec![o],
                _ => Vec::new(),
            };
            out.extend(items.iter().filter_map(|i| BeadRow::from_json(i, &self.home_repo, &self.submitted_label)));
        }
        Ok(out)
    }
    fn land_status(&self, id: &str) -> String {
        match self.show(&[id.to_string()]) {
            Ok(rows) => rows.into_iter().next().map(|r| r.status).unwrap_or_else(|| "-".into()),
            Err(_) => "-".into(),
        }
    }
    fn ask_open(&self, label: &str, subject: &str) -> bool {
        if subject.is_empty() {
            return false;
        }
        let Ok(raw) = self.bdq_raw("list", &["--status", "open", "--label", label, "--limit", "0"]) else {
            return false;
        };
        let js = json_only(&raw);
        let Ok(v) = serde_json::from_str::<serde_json::Value>(js) else {
            return false;
        };
        let items: Vec<serde_json::Value> = match v {
            serde_json::Value::Array(a) => a,
            o @ serde_json::Value::Object(_) => vec![o],
            _ => Vec::new(),
        };
        items.iter().any(|i| i.get("title").and_then(|t| t.as_str()).is_some_and(|t| t.contains(subject)))
    }
    fn context(&self, id: &str, now: i64) -> String {
        let Ok(raw) = self.show_raw(&[id.to_string()]) else {
            return format!("(could not read {id} — say so rather than pretend)");
        };
        let js = json_only(&raw);
        let Ok(v) = serde_json::from_str::<serde_json::Value>(js) else {
            return format!("(could not read {id} — say so rather than pretend)");
        };
        let item = match v {
            serde_json::Value::Array(a) => a.into_iter().next(),
            o @ serde_json::Value::Object(_) => Some(o),
            _ => None,
        };
        let Some(item) = item else {
            return "(could not read the bead — say so rather than pretend)".into();
        };
        let status = item.get("status").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let priority = item.get("priority").and_then(|x| x.as_i64()).unwrap_or(9999);
        let created_at = item.get("created_at").and_then(|x| x.as_str()).map(String::from);
        let title = item.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let labels: Vec<String> = item
            .get("labels")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|l| l.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let description = item.get("description").and_then(|x| x.as_str()).map(String::from);
        let notes = item.get("notes").cloned();
        crate::ask::bead_context(id, &status, priority, created_at.as_deref(), &title, &labels, description.as_deref(), notes.as_ref(), now)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// git
// ──────────────────────────────────────────────────────────────────────────────

pub struct RealGit;

fn git(repo: &Path) -> std::process::Command {
    let mut c = command("git");
    c.arg("-C").arg(repo).stdin(Stdio::null());
    c
}

fn git_ok(mut c: std::process::Command) -> bool {
    c.stdout(Stdio::null()).stderr(Stdio::null());
    c.status().map(|s| s.success()).unwrap_or(false)
}

fn git_out(c: std::process::Command) -> Option<String> {
    let (rc, so, _) = run_capture(c);
    if rc == 0 { Some(String::from_utf8_lossy(&so).trim_end_matches('\n').to_string()) } else { None }
}

impl Git for RealGit {
    fn spira_refs(&self, repo: &Path) -> Vec<(String, String)> {
        let mut c = git(repo);
        c.args(["for-each-ref", "--format=%(refname:short) %(objectname)", "refs/heads/spira/*"]);
        git_out(c)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once(' ').map(|(a, b)| (a.to_string(), b.to_string())))
            .filter(|(a, b)| !a.is_empty() && !b.is_empty())
            .collect()
    }
    fn branch_exists(&self, repo: &Path, branch: &str) -> bool {
        let mut c = git(repo);
        c.args(["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")]);
        git_ok(c)
    }
    fn rev_parse(&self, repo: &Path, rev: &str) -> Option<String> {
        let mut c = git(repo);
        c.args(["rev-parse", "--verify", "-q", rev]);
        git_out(c).filter(|s| !s.is_empty())
    }
    fn is_ancestor(&self, repo: &Path, a: &str, b: &str) -> bool {
        let mut c = git(repo);
        c.args(["merge-base", "--is-ancestor", a, b]);
        git_ok(c)
    }
    fn content_landed(&self, repo: &Path, branch: &str, base: &str) -> bool {
        let Some(ahead) = self.count(repo, &format!("{base}..{branch}")) else { return false };
        // An ancestor branch is landed: every commit is already reachable from the base.
        if self.is_ancestor(repo, branch, base) {
            return true;
        }
        if ahead == 0 {
            return false;
        }
        let mut c = git(repo);
        c.args(["merge-tree", "--write-tree", base, branch]);
        let Some(merged) = git_out(c) else { return false };
        let merged = merged.lines().next().unwrap_or("").to_string();
        if merged.is_empty() {
            return false;
        }
        self.rev_parse(repo, &format!("{base}^{{tree}}")).map(|t| t == merged).unwrap_or(false)
    }
    fn count(&self, repo: &Path, range: &str) -> Option<u64> {
        let mut c = git(repo);
        c.args(["rev-list", "--count", range]);
        git_out(c).and_then(|s| s.trim().parse().ok())
    }
    fn fetch(&self, repo: &Path, remote: &str) {
        let mut c = git(repo);
        c.args(["fetch", "-q", "--no-write-fetch-head", remote]);
        let _ = git_ok(c);
    }
    fn tree_ok(&self, tree: &Path) -> bool {
        tree.join(".git").exists()
    }
    fn tree_add_detached(&self, repo: &Path, tree: &Path, at: &str) {
        if let Some(d) = tree.parent() {
            let _ = fs::create_dir_all(d);
        }
        let mut c = git(repo);
        c.args(["worktree", "add", "-q", "--detach"]).arg(tree).arg(at);
        let _ = git_ok(c);
    }
    fn tree_checkout_landing(&self, tree: &Path, base: &str) -> bool {
        let mut c = git(tree);
        c.args(["checkout", "-q", "-B", "landing", base]);
        git_ok(c)
    }
    fn tree_merge(&self, tree: &Path, subject: &str, branch: &str, name: &str, email: &str) -> Result<(), String> {
        let mut c = git(tree);
        c.arg("-c").arg(format!("user.name={name}")).arg("-c").arg(format!("user.email={email}"));
        c.args(["merge", "--no-edit", "-q", "-m", subject, branch]);
        if git_ok(c) {
            return Ok(());
        }
        let mut d = git(tree);
        d.args(["diff", "--name-only", "--diff-filter=U"]);
        let files = git_out(d).unwrap_or_default().split('\n').filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
        self.tree_merge_abort(tree);
        Err(files)
    }
    fn tree_head(&self, tree: &Path) -> Option<String> {
        self.rev_parse(tree, "HEAD")
    }
    fn tree_reset_hard(&self, tree: &Path, to: &str) {
        let mut c = git(tree);
        c.args(["reset", "-q", "--hard", to]);
        let _ = git_ok(c);
    }
    fn tree_merge_abort(&self, tree: &Path) {
        let mut c = git(tree);
        c.args(["merge", "--abort"]);
        let _ = git_ok(c);
    }
    fn delete_branch(&self, repo: &Path, branch: &str) -> bool {
        let mut c = git(repo);
        c.env("SPIRA_REF_SANCTIONED", "1").args(["branch", "-D", branch]);
        git_ok(c)
    }
    fn refs_matching(&self, repo: &Path, pattern: &str) -> Vec<String> {
        let mut c = git(repo);
        c.args(["for-each-ref", "--format=%(refname:short)", pattern]);
        git_out(c).unwrap_or_default().lines().filter(|l| !l.is_empty()).map(String::from).collect()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Harness programs
// ──────────────────────────────────────────────────────────────────────────────

pub struct RealTools {
    pub home: PathBuf,
    pub queue_bin: Option<PathBuf>,
    /// Exported to every child: testenv appends its container names here for `halt`.
    pub containers: Option<PathBuf>,
    /// gate.sh's host-wide admission pool (`$SPIRA_RUN/gate-admission`), probed before a
    /// concurrent gate starts (DESIGN.md §8 D14 (f)).
    pub admission: Option<PathBuf>,
    /// The gates the concurrent walk has started and not yet collected.
    pub pool: GatePool,
}

impl RealTools {
    pub fn new(home: PathBuf, queue_bin: Option<PathBuf>, containers: Option<PathBuf>, admission: Option<PathBuf>) -> RealTools {
        RealTools { home, queue_bin, containers, admission, pool: GatePool::new(&crate::util::GATE_CHILDREN) }
    }
}

/// Gates running concurrently (D14): each is spawned on the calling thread — so its pid is
/// registered for SIGTERM forwarding before `start` returns — and waited on by a thread of
/// its own, which sends (ticket, status, transcript) back when it exits.
pub struct GatePool {
    tx: std::sync::mpsc::Sender<(u64, i32, String)>,
    rx: std::sync::mpsc::Receiver<(u64, i32, String)>,
    next: std::cell::Cell<u64>,
    running: std::cell::Cell<usize>,
    children: &'static crate::util::ChildSet,
}

impl GatePool {
    pub fn new(children: &'static crate::util::ChildSet) -> GatePool {
        let (tx, rx) = std::sync::mpsc::channel();
        GatePool { tx, rx, next: std::cell::Cell::new(1), running: std::cell::Cell::new(0), children }
    }

    /// Start a prepared command (stdout and stderr into one capture, as `combined`).
    pub fn start(&self, c: std::process::Command) -> u64 {
        let ticket = self.next.get();
        self.next.set(ticket + 1);
        self.running.set(self.running.get() + 1);
        let tx = self.tx.clone();
        let fail = |e: String| {
            let _ = tx.send((ticket, -1, e));
        };
        let mut c = c;
        let Some((r, w)) = os_pipe() else {
            fail("cannot create a pipe for the gate".into());
            return ticket;
        };
        let Ok(w2) = w.try_clone() else {
            fail(String::new());
            return ticket;
        };
        c.stdout(Stdio::from(w)).stderr(Stdio::from(w2)).stdin(Stdio::null());
        let mut child = match c.spawn() {
            Ok(ch) => ch,
            Err(e) => {
                fail(e.to_string());
                return ticket;
            }
        };
        // Our copies of the pipe's write end go with the Command: the reader sees EOF when
        // the gate (and every child of it holding the pipe) is done.
        drop(c);
        let pid = child.id() as i32;
        let children = self.children;
        children.insert(pid);
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut r = r;
            let _ = std::io::Read::read_to_end(&mut r, &mut buf);
            let st = child.wait();
            children.remove(pid);
            let rc = st.ok().and_then(|s| s.code()).unwrap_or(-1);
            let _ = tx.send((ticket, rc, String::from_utf8_lossy(&buf).into_owned()));
        });
        ticket
    }

    pub fn wait_any(&self) -> Option<(u64, i32, String)> {
        if self.running.get() == 0 {
            return None;
        }
        let got = self.rx.recv().ok()?;
        self.running.set(self.running.get() - 1);
        Some(got)
    }
}

/// Free slots among gate.sh's `slot.1.lock … slot.<par>.lock`, each probed with a
/// non-blocking flock released at once. A missing pool directory is an idle pool.
pub fn admission_free(dir: &Path, par: usize) -> usize {
    use std::os::unix::io::AsRawFd;
    if !dir.is_dir() {
        return par;
    }
    (1..=par)
        .filter(|k| {
            let Ok(f) = fs::OpenOptions::new().create(true).write(true).truncate(false).open(dir.join(format!("slot.{k}.lock"))) else {
                return false;
            };
            // RELEASED BY LOCK_UN, NEVER BY THE CLOSE. A flock belongs to the open file
            // description, and closing `f` releases it only if no other process holds a copy.
            // Any thread of this process that forks while the probe holds the lock (every
            // child starts through `util::command`, whose pre_exec forces a real fork) gives
            // the child a copy that lives until it execs, and for that window the slot reads
            // held to gate.sh's admission and to the next probe. LOCK_UN drops the lock on the
            // description itself, however many copies of it exist.
            let fd = f.as_raw_fd();
            let free = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) == 0 };
            if free {
                unsafe { libc::flock(fd, libc::LOCK_UN) };
            }
            free
        })
        .count()
}

fn combined(c: std::process::Command) -> (i32, String) {
    // stdout and stderr interleaved into one capture, as `2>&1` did.
    let mut c = c;
    let (r, w) = match os_pipe() {
        Some(p) => p,
        None => {
            let (rc, so, se) = run_capture(c);
            let mut s = String::from_utf8_lossy(&so).into_owned();
            s.push_str(&String::from_utf8_lossy(&se));
            return (rc, s);
        }
    };
    let w2 = match w.try_clone() {
        Ok(w2) => w2,
        Err(_) => return (-1, String::new()),
    };
    c.stdout(Stdio::from(w)).stderr(Stdio::from(w2)).stdin(Stdio::null());
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return (-1, e.to_string()),
    };
    drop(c);
    crate::util::CURRENT_CHILD.store(child.id() as i32, std::sync::atomic::Ordering::SeqCst);
    let mut buf = Vec::new();
    let mut r = r;
    let _ = std::io::Read::read_to_end(&mut r, &mut buf);
    let st = child.wait();
    crate::util::CURRENT_CHILD.store(0, std::sync::atomic::Ordering::SeqCst);
    let rc = st.ok().and_then(|s| s.code()).unwrap_or(-1);
    (rc, String::from_utf8_lossy(&buf).into_owned())
}

fn os_pipe() -> Option<(fs::File, fs::File)> {
    use std::os::unix::io::FromRawFd;
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return None;
    }
    Some(unsafe { (fs::File::from_raw_fd(fds[0]), fs::File::from_raw_fd(fds[1])) })
}

impl RealTools {
    fn with_env(&self, c: &mut std::process::Command) {
        if let Some(p) = &self.containers {
            c.env("SPIRA_LANDING_CONTAINERS", p);
        }
    }
}

pub fn executable(p: &Path) -> bool {
    fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

impl RealTools {
    fn gate_command(&self, branch: &str, repo: &str, lock_wait: &str, bead: &str) -> std::process::Command {
        let mut c = command("gate.sh");
        c.arg(branch).arg(repo).env("SPIRA_GATE_LOCK_WAIT", lock_wait).env("SPIRA_GATE_BEAD", bead);
        self.with_env(&mut c);
        c
    }
}

impl Tools for RealTools {
    fn gate(&self, branch: &str, repo: &str, lock_wait: &str, bead: &str) -> (i32, String) {
        combined(self.gate_command(branch, repo, lock_wait, bead))
    }
    fn gate_start(&self, branch: &str, repo: &str, lock_wait: &str, bead: &str) -> u64 {
        self.pool.start(self.gate_command(branch, repo, lock_wait, bead))
    }
    fn gate_wait_any(&self) -> Option<(u64, i32, String)> {
        self.pool.wait_any()
    }
    fn gate_slots_free(&self, par: usize) -> Option<usize> {
        self.admission.as_ref().map(|d| admission_free(d, par))
    }
    /// `gate-run.sh`, by name on the launcher's PATH (sp-gypjk); a suite scripts this
    /// method's behavior by putting its own gate-run.sh first on PATH. A failed run (or a
    /// missing tool) is "no status".
    fn gate_status(&self, branch: &str, repo: &str) -> Option<String> {
        let mut c = command("gate-run.sh");
        c.arg("--status").arg(branch).arg(repo).stdin(Stdio::null());
        let (rc, so, _) = run_capture(c);
        if rc == 0 { Some(String::from_utf8_lossy(&so).trim_end_matches('\n').to_string()) } else { None }
    }
    fn confine(&self, id: &str, branch: &str, repo: &Path, base: &str, labels: &str) -> (i32, String) {
        let mut c = command("confine.sh");
        c.arg(id).arg(branch).arg(repo).arg(base).arg(labels);
        let (rc, s) = combined(c);
        (rc, s.trim_end_matches('\n').to_string())
    }
    fn queue_step(&self, repo: &str) -> Result<Vec<String>, String> {
        let Some(bin) = self.queue_bin.as_ref() else {
            return Err("no queue program — the queue step did not run".into());
        };
        let mut c = command(bin);
        c.arg("step").arg(repo);
        self.with_env(&mut c);
        let (_, s) = combined(c);
        Ok(s.lines().map(String::from).collect())
    }
    fn skew_refresh(&self, repo: &Path) -> String {
        // `skew` is a compiled binary now (sp-yyk47), same bare-name PATH resolution.
        let mut c = command("skew");
        c.arg("refresh").arg(repo);
        combined(c).1.trim_end_matches('\n').to_string()
    }
    fn ensure(&self, script: &Path) -> Vec<String> {
        // A bare name is a launcher-PATH program, run directly; a path (systemd/ is not on
        // PATH) is run with bash when it is there.
        let c = if script.components().count() == 1 {
            command(script)
        } else {
            if !executable(script) {
                return Vec::new();
            }
            let mut c = command("bash");
            c.arg(script);
            c
        };
        combined(c).1.lines().map(String::from).collect()
    }
    fn rebase_stale(&self, id: &str, repo: &str) -> i32 {
        // rebase-stale, by name on the launcher's PATH (sp-gypjk); a failed spawn is rc 3.
        let mut c = command("rebase-stale");
        c.arg(id).arg(repo).env("SPIRA_HOME", &self.home);
        self.with_env(&mut c);
        match combined(c).0 {
            r @ 0..=2 => r,
            _ => 3,
        }
    }
    fn forge_pr_state(&self, repo: &Path, selector: &str) -> Option<String> {
        let mut c = command("forge");
        c.arg("pr-state").arg(repo).arg(selector).stdin(Stdio::null());
        let (rc, so, _) = run_capture(c);
        if rc == 0 { Some(String::from_utf8_lossy(&so).trim().to_string()) } else { None }
    }
    fn forge_pr_create(&self, repo: &Path, head: &str, base: &str, title: &str, body: &str) -> Option<u64> {
        let mut c = command("forge");
        c.arg("pr-create").arg(repo).arg(head).arg(base).arg(title);
        c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = c.spawn().ok()?;
        if let Some(mut si) = child.stdin.take() {
            use std::io::Write;
            let _ = si.write_all(body.as_bytes());
        }
        let out = child.wait_with_output().ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }
    fn forge_pr_list_open(&self, repo: &Path) -> Vec<(u64, String)> {
        let mut c = command("forge");
        c.arg("pr-list-open").arg(repo).stdin(Stdio::null());
        let (rc, so, _) = run_capture(c);
        if rc != 0 {
            return Vec::new();
        }
        String::from_utf8_lossy(&so)
            .lines()
            .filter_map(|l| {
                let (n, br) = l.split_once(' ')?;
                Some((n.trim().parse().ok()?, br.to_string()))
            })
            .collect()
    }
    fn forge_pr_automerge(&self, repo: &Path, selector: &str) -> bool {
        let mut c = command("forge");
        c.arg("pr-automerge").arg(repo).arg(selector).stdin(Stdio::null());
        run_capture(c).0 == 0
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Liveness and time
// ──────────────────────────────────────────────────────────────────────────────

pub struct RealProcs {
    pub run: PathBuf,
}

pub fn pid_alive(pid: &str) -> bool {
    let pid = pid.trim();
    !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()) && Path::new(&format!("/proc/{pid}")).is_dir()
}

impl Procs for RealProcs {
    fn holder_alive(&self, id: &str) -> bool {
        if let Ok(pid) = fs::read_to_string(self.run.join(format!("hold-{id}.pid"))) {
            if pid_alive(&pid) {
                return true;
            }
        }
        let suffix = format!("-{id}.pid");
        let Ok(rd) = fs::read_dir(&self.run) else { return false };
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if !(n.starts_with("aeon-") && n.ends_with(&suffix)) {
                continue;
            }
            let Ok(pid) = fs::read_to_string(e.path()) else { continue };
            if !pid_alive(&pid) {
                continue;
            }
            // argv must be our runner, not a recycled pid.
            let cmd = fs::read(format!("/proc/{}/cmdline", pid.trim())).unwrap_or_default();
            if is_aeon_cmdline(&String::from_utf8_lossy(&cmd).replace('\0', " ")) {
                return true;
            }
        }
        false
    }
}

/// Whether a space-joined cmdline is an aeon: the Rust binary (argv[0] `aeon` or `…/aeon`)
/// or the retired `aeon.sh` — the rule lib.sh aeon_alive, the sentinel, strand and
/// rebase-stale share.
pub fn is_aeon_cmdline(cmd: &str) -> bool {
    let argv0 = cmd.split(' ').next().unwrap_or("");
    cmd.contains("aeon.sh") || argv0 == "aeon" || argv0.ends_with("/aeon")
}

pub struct RealClock;

impl Clock for RealClock {
    fn now(&self) -> u64 {
        unix_now()
    }
    fn sleep(&self, secs: u64) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
}

#[cfg(test)]
mod aeon_cmdline_tests {
    #[test]
    fn the_binary_and_the_retired_script_are_both_aeons() {
        assert!(super::is_aeon_cmdline("/r/current/bin/aeon --home /r/current/spira builder "));
        assert!(super::is_aeon_cmdline("bash /h/spira/aeon.sh builder "));
        assert!(!super::is_aeon_cmdline("sleep 30 "));
        assert!(!super::is_aeon_cmdline("/usr/bin/aeonic --x "));
    }
}
