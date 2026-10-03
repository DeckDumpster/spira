//! The trial, in the order DESIGN.md "Order of the trial" gives. One way out: [`Trial::finish`].

use crate::basecache;
use crate::cert;
use crate::def::{self, Resolved};
use crate::fence;
use crate::compose::{self, Composition, Forces};
use crate::key::{self, KeyInputs};
use crate::parse::{self, Attribution};
use crate::ports::{Ctx, Merge, World};
use spira_config::GateMode;
use std::path::{Path, PathBuf};

pub const PASS: i32 = 0;
pub const FAIL: i32 = 1;
pub const NOVERDICT: i32 = 75;
pub const BASEFAIL: i32 = 76;

/// `spira_gate_outcome`.
pub fn outcome(st: i32) -> &'static str {
    match st {
        PASS => "PASS",
        NOVERDICT => "NO_VERDICT",
        BASEFAIL => "BASE_FAIL",
        _ => "FAIL",
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub status: i32,
    pub reason: String,
    pub msg: String,
}

fn v(status: i32, reason: &str, msg: impl Into<String>) -> Verdict {
    Verdict {
        status,
        reason: reason.into(),
        msg: msg.into(),
    }
}

/// A FAIL THAT SAYS NOTHING AT ALL IS NO_VERDICT: it is the machinery failing to run a check.
pub fn settle(mut vd: Verdict) -> Verdict {
    if ![PASS, NOVERDICT, BASEFAIL].contains(&vd.status) && vd.msg.trim().is_empty() {
        vd.reason = format!("no-evidence:{}", vd.reason);
        vd.status = NOVERDICT;
        vd.msg = "the gate returned a failure with no output at all — that is the machinery failing to run a check, not the branch failing one".into();
    }
    vd
}

pub struct Args {
    pub home: PathBuf,
    pub branch: String,
    pub repo: Option<String>,
    /// `--release-bins` (sp-z61hj): on a PASS, build the release profile of the judged tree
    /// in the gate tree (tmpfs, through the build cache), so a hand landing ships it with
    /// `queue land-local --worktree <gate tree>` instead of rebuilding in a worktree on disk.
    pub release_bins: bool,
}

/// What the finish needs to know about how far the trial got.
#[derive(Default)]
struct State {
    repo_name: String,
    repo: PathBuf,
    run: String,
    /// yield.sh records need the repository; armed once it resolved.
    armed: bool,
    gate_log: PathBuf,
    start: u64,
    waited: u64,
    branch_tree: String,
    branch_tip: String,
    suite: String,
    key: String,
    held_lock: bool,
    tree: Option<PathBuf>,
    filelist: Option<PathBuf>,
    bead: String,
    /// The composition's label once one was chosen (DESIGN.md "Composition"); empty before.
    compose: String,
    /// (phase, wall seconds), in the order they ran; the base trial's carry a `base-` prefix.
    phases: Vec<(String, u64)>,
    /// Seconds the trial spent waiting for a slot (testenv's `queue-wait=` lines), inside
    /// the `gate` phase's wall.
    queue_secs: u64,
    /// The tree certificate (cert.rs) a PASS writes: the merged tree judged, the revision
    /// that carries it, where verdicts live, the harness hash and the suites covered.
    merged_tree: String,
    rev: String,
    verdict_dir: PathBuf,
    harness_h: String,
    pass_suites: String,
    caller: String,
    /// The gate-run TSD row's (telemetry.rs): the mode in force and the branch's shape.
    gate_mode: String,
    branch_type: String,
    /// The gate command's PATH, set outright: the launcher's release PATH, then cargo for the
    /// tree builds (sp-31gtu). Empty until SPIRA_RELEASE has been read.
    path: String,
    /// The compiler wrapper's environment for every build the trial runs (sp-z61hj).
    build_env: Vec<(String, String)>,
    /// Why the build cache cannot be used; refused only by a trial that builds (sp-z61hj).
    cache_refusal: Option<String>,
    /// What `--release-bins` builds with: HOME, SPIRA_RELEASE and the trial's timeout.
    home_dir: String,
    release: String,
    timeout: String,
}

pub struct Trial<'w, W: World> {
    w: &'w W,
    a: Args,
    s: State,
}

impl<'w, W: World> Trial<'w, W> {
    pub fn new(w: &'w W, a: Args) -> Self {
        Trial {
            w,
            a,
            s: State {
                suite: "-".into(),
                ..State::default()
            },
        }
    }

    /// Run the trial and its one exit; returns the exit status.
    pub fn run(mut self) -> i32 {
        let verdict = self.trial();
        self.finish(verdict)
    }

    fn trial(&mut self) -> Verdict {
        let w = self.w;
        let ctx = match w.context(self.a.repo.as_deref()) {
            Ok(c) => c,
            Err(e) => {
                self.s.repo_name = self.a.repo.clone().unwrap_or_else(|| "?".into());
                return v(
                    NOVERDICT,
                    "lib-unavailable",
                    format!(
                        "gate: lib.sh could not be sourced from {} — refusing to judge.\n{e}",
                        self.a.home.display()
                    ),
                );
            }
        };
        self.s.repo_name = ctx.repo_name.clone();
        self.s.bead = ctx.var("SPIRA_GATE_BEAD").to_string();
        let br = self.a.branch.clone();
        let name = ctx.repo_name.clone();

        let map = ctx.var("SPIRA_REPO_MAP");
        if map.is_empty() || !w.readable(Path::new(map)) {
            let shown = if map.is_empty() { "<unset>" } else { map };
            return v(NOVERDICT, "no-repo-map-file", format!(
                "gate: the repository map at {shown} cannot be read — refusing to judge.\ngate: without it every repository looks like one with no gate command, and every branch\ngate: would pass a trial that never ran."));
        }
        let Some(repo) = ctx.repo_root.clone() else {
            return v(
                NOVERDICT,
                "no-repo-map",
                format!("gate: repo-map has no entry for '{name}' — refusing to guess a checkout"),
            );
        };
        // THE LAUNCHER'S PATH, SET OUTRIGHT (sp-31gtu): the gate command runs under `env -i`
        // with PATH built from SPIRA_RELEASE — never the PATH this process inherited — so a
        // bare tool name in a step is the running release's. The box's own tool tail
        // (spira.path, sp-c7b85) is appended after the system directories for cargo (the
        // tree builds — `bin` lines, unit phases) and every other box tool a step might name.
        // Unset SPIRA_RELEASE, or a tail entry inside a release or a checkout, is a refusal
        // naming it, never a fallback.
        match spira_config::release_path_from_env_with_tail(Some(ctx.var(spira_config::RELEASE_ENV)), ctx.var("SPIRA_PATH")) {
            Ok(p) => self.s.path = p,
            Err(e) => return v(NOVERDICT, "release-unset", format!("gate: {e} — refusing to judge")),
        }
        // THE BUILD CACHE (sp-z61hj; spira-config/DESIGN-build-cache.md): every cargo build the
        // trial runs — the tools, the unit phases, the build fence, testenv's — compiles
        // through the box's one sccache, resolved on the PATH the command gets. Resolved here,
        // required where the trial builds in the tree (below: absent is a refusal, never a cold
        // build of every dependency); a definition that builds nothing never needs it.
        // SPIRA_BUILD_CACHE=off opts out, loudly.
        match w.build_wrapper(&self.s.path, ctx.var(spira_config::build::CACHE_ENV)) {
            Ok(wr) => {
                if wr == spira_config::build::Wrapper::Off {
                    w.eprint(&format!("gate: {}", wr.describe()));
                }
                // Every cargo the gate runs — tools, unit phases, the build fence's `make build`,
                // release-bins — goes through `spira-admit` with the GATE's token (sp-f4ig1-fix,
                // DESIGN-admission.md D11): it takes a compile lease for that cargo WITHOUT
                // WAITING (oversubscribing a full pool), so the gate never queues and agent builds
                // queue behind it instead of competing at full width. testenv's own build does
                // the same in-process. The token also keeps every other admission inherited.
                // Without spira-admit on PATH (an older release) the plain wrapper still carries
                // the token: the gate never waits, it just holds nothing.
                let who = format!("gate:{br}");
                // THE SHARED STORE (sp-xtdqi): `SPIRA_SCCACHE_DAV_ADDR`, resolved in-process
                // (`merge_resolved_config`, `Real::context`) the same as every other
                // `spira.toml`-only key this `ctx` already carries — never a bare env read.
                let store = spira_config::build::Store::from_values(|k| {
                    let v = ctx.var(k);
                    (!v.is_empty()).then(|| v.to_string())
                });
                self.s.build_env = match w.which(spira_config::admission::BIN) {
                    Some(admit) => wr.admitted_env(&admit, ctx.var("SPIRA_RUN"), &who, store.as_ref()),
                    None => wr.env(store.as_ref()),
                };
                self.s.build_env.push((spira_config::admission::INHERIT_ENV.to_string(), "gate".to_string()));
            }
            Err(e) => self.s.cache_refusal = Some(e),
        }
        self.s.home_dir = ctx.var("HOME").to_string();
        self.s.release = ctx.var(spira_config::RELEASE_ENV).to_string();
        self.s.timeout = ctx.var_or("SPIRA_GATE_TIMEOUT", "2700").to_string();
        let repo = PathBuf::from(repo);
        self.s.repo = repo.clone();
        self.s.run = ctx.var("SPIRA_RUN").to_string();
        self.s.gate_log = PathBuf::from(
            ctx.var_or("SPIRA_GATE_LOG", &format!("{}/gate.log", self.s.run))
                .to_string(),
        );
        self.s.branch_tree = w
            .rev_parse(&repo, &format!("{br}^{{tree}}"))
            .unwrap_or_else(|| "-".into());
        self.s.branch_tip = w
            .rev_parse(&repo, &format!("{br}^{{commit}}"))
            .unwrap_or_else(|| "-".into());
        self.s.armed = true;
        self.s.start = w.now();
        self.s.verdict_dir = PathBuf::from(
            ctx.var_or("SPIRA_VERDICTS", &format!("{}/verdicts", self.s.run))
                .to_string(),
        );
        self.s.caller = ctx.var_or("SPIRA_GATE_CALLER", &br).to_string();

        let Some(base) = ctx.landref.clone() else {
            return v(NOVERDICT, "no-base", format!(
                "gate: cannot resolve the ref '{name}' lands on — refusing to guess a base\ngate: give it a `base` column in {map}"));
        };
        // THE BASE IS PINNED (sp-hh5h0): the landing ref is resolved to a commit once, here,
        // and every later read — the merge, the touched set, the gate string's
        // SPIRA_GATE_BASE, the base trial — uses that commit. The ref moves while a trial
        // runs (a landing lands); re-reading it by name made the base trial judge a
        // different base than the merge was cut from, and a red the old base shared was
        // charged to the branch because the new base had fixed it. `base` stays the name,
        // for the messages.
        let Some(base_rev) = w
            .rev_parse(&repo, &format!("{base}^{{commit}}"))
            .filter(|r| !r.is_empty())
        else {
            return v(NOVERDICT, "no-base", format!(
                "gate: the ref '{base}' that {name} lands on does not resolve to a commit — refusing to guess a base\ngate: give it a `base` column in {map}"));
        };
        let range = format!("{base_rev}...{br}");
        let files = match w.diff_names(&repo, &range) {
            Ok(f) => f,
            Err(out) => {
                return v(NOVERDICT, "no-diff", format!(
                    "gate: cannot diff {base}...{br} in {name} — one of them does not resolve in this checkout\n{out}"))
            }
        };
        let status_list = w.diff_name_status(&repo, &range);
        let ejected = self.ejected(&ctx);

        // THE MERGE (DESIGN.md "The merge"): what would land is what is judged.
        let merged_tree = match w.merge_tree(&repo, &base_rev, &br) {
            Merge::Clean(t) => t,
            Merge::Conflict(paths) => {
                let list: String = paths
                    .iter()
                    .fold(String::new(), |acc, p| acc + "gate:   " + p + "\n");
                return v(NOVERDICT, "conflict", format!(
                    "gate: {br} does not merge onto {base} — it is stale, not red; nothing about the work is judged.\n{list}gate: rebase it onto {base} (the landing pass hands it to rebase-stale)."));
            }
            Merge::Failed(e) => {
                return v(NOVERDICT, "merge-failed", format!("gate: cannot merge {br} onto {base} in {name} to judge it — refusing to judge the branch alone.\n{e}"));
            }
        };
        let (rev, label) = if w.is_ancestor(&repo, &base_rev, &br) {
            (br.clone(), br.clone())
        } else {
            match w.commit_merge(&repo, &merged_tree, &base_rev, &br) {
                Some(c) => (c.clone(), c),
                None => {
                    return v(NOVERDICT, "merge-failed", format!("gate: cannot record the merge of {br} onto {base} in {name} — refusing to judge the branch alone."));
                }
            }
        };

        self.s.merged_tree = merged_tree.clone();
        self.s.rev = rev.clone();

        // LAYER 1 — every changed shell script parses, as it would land.
        for f in files.lines().filter(|f| f.ends_with(".sh")) {
            let Some(content) = w.show_blob(&repo, &rev, f) else {
                continue;
            };
            if let Err(syntax) = w.bash_n(&content) {
                return v(FAIL, "syntax", format!("gate: {f} fails bash -n\n{syntax}"));
            }
        }
        let Some(exclude) = w.which("exclude.sh") else {
            return v(
                NOVERDICT,
                "missing-exclude",
                "gate: exclude.sh is not on PATH — refusing to land unchecked".to_string(),
            );
        };
        let offenders = w.exclude_filter(&exclude, &w.ls_tree_all(&repo, &rev));
        let offenders = offenders.trim_end_matches('\n');
        if !offenders.is_empty() {
            let listed: Vec<String> = offenders.lines().map(|l| format!("gate:   {l}")).collect();
            return v(FAIL, "beads-data", format!(
                "gate: {br} would land beads data in the harness tree:\n{}\ngate: a beads database is never public and belongs in no shared repository.\ngate: remove them from the branch — there is no override for this one.",
                listed.join("\n")));
        }
        let Some(skew) = w.which("skew") else {
            return v(
                NOVERDICT,
                "missing-skew",
                "gate: skew is not on PATH — refusing to land unchecked".to_string(),
            );
        };
        let (skew_rc, skew_out) = w.skew_foreign(&skew, &repo, &base_rev, &br);
        if skew_rc == 3 {
            return v(NOVERDICT, "skew-init-fault", format!(
                "gate: skew could not initialize — conf.sh or the database may be unavailable.\ngate: the foreign-harness check did not run; this is a machinery fault, not a branch fault.\n{skew_out}"));
        }
        if skew_rc != 0 {
            return v(
                FAIL,
                "foreign-harness",
                format!(
                    "gate: {br} belongs in the harness's own repository, not {name}.\n{skew_out}"
                ),
            );
        }

        let Some(cite) = w.which("commit-cite.sh") else {
            return v(
                NOVERDICT,
                "missing-commit-cite",
                "gate: commit-cite.sh is not on PATH — refusing to land unchecked".to_string(),
            );
        };
        let (cite_rc, cite_out) = w.commit_cite(&cite, &repo, &base_rev, &br);
        if cite_rc == 3 {
            return v(NOVERDICT, "commit-cite-fault", format!(
                "gate: the bead store did not answer — the commit-citation check did not run.\ngate: this is a machinery fault, not a branch fault.\n{cite_out}"));
        }
        if cite_rc != 0 {
            return v(FAIL, "phantom-bead-id", format!(
                "gate: {br} cites a bead id that does not exist:\n{cite_out}"));
        }

        // LAYER 2 — the repository's own gate. THE TREE OWNS ITS GATE (sp-quu2w): the
        // definition is the tree under test's `gate.steps`, and the base trial runs the
        // landing ref's own. The repo-map column is read only for a repository whose landing
        // ref has never carried one; once it has, a tree without a readable definition is
        // refused, never quietly gated by config.
        let column = ctx.gate_cmd.clone();
        let base_blob = w.show_blob(&repo, &base_rev, def::PATH);
        let tree_blob = w.show_blob(&repo, &rev, def::PATH);
        let (cmd, tree_def) = match def::resolve(base_blob.as_deref(), tree_blob.as_deref(), &column) {
            Resolved::Column(c) => (c, None),
            Resolved::Tree(d) => {
                if !column.trim().is_empty() {
                    w.eprint(&format!("gate: {name}'s repo-map gate column is ignored — the tree's {} is its gate (sp-quu2w); clear the column", def::PATH));
                }
                (d.command(), Some(d))
            }
            Resolved::Refused { branch: true, why } => {
                return v(FAIL, "gate-definition", format!(
                    "gate: {br} as it would land has no gate definition the gate can read.\ngate: {why}\ngate: the tree under test owns its gate ({}); fix it on the branch.", def::PATH));
            }
            Resolved::Refused { branch: false, why } => {
                return v(BASEFAIL, "base-gate-definition", format!(
                    "gate: {base}'s own gate definition does not read — this branch did not cause it.\ngate: {why}\ngate: fix {} on {base}.", def::PATH));
            }
        };
        let base_def = def::resolve_base(base_blob.as_deref(), &column);
        if cmd.is_empty() {
            return v(PASS, "syntax-only", "");
        }
        for p in parse::bash_paths(&cmd) {
            if tree_def.is_none() {
                if !w.ls_tree_has(&repo, &base_rev, &p) {
                    return v(NOVERDICT, "cmd-missing-file", format!(
                        "gate: {name}'s gate command names 'bash {p}' but {p} is absent from {base}.\ngate: command: {cmd}\ngate: this is a gate configuration error, not a fault in any branch.\ngate: land {p} on the base before wiring it into the gate command, or remove it."));
                }
                continue;
            }
            // A tree definition names files of the same tree: the branch that deletes a
            // fence script removes its step in the same commit.
            if !w.ls_tree_has(&repo, &rev, &p) {
                let base_names_it = matches!(&base_def, Ok(Resolved::Tree(b)) if parse::bash_paths(&b.command()).contains(&p));
                if base_names_it && !w.ls_tree_has(&repo, &base_rev, &p) {
                    return v(BASEFAIL, "base-gate-definition", format!(
                        "gate: {base}'s own {} names 'bash {p}' and {base} does not carry {p} — this branch did not cause it.\ngate: fix {} on {base}.", def::PATH, def::PATH));
                }
                return v(FAIL, "gate-definition", format!(
                    "gate: {br} as it would land names 'bash {p}' in {} but does not carry {p}.\ngate: command: {cmd}\ngate: a branch that deletes a fence removes its `step` line in the same commit.", def::PATH));
            }
        }

        // THE MODE (DESIGN.md "Composition"): typed, per repository, through spira-config.
        let mode = match w.gate_mode(&name) {
            Ok(m) => m,
            Err(e) => {
                w.eprint(&format!("gate: the configuration does not validate ({e})\ngate: composing as gate_mode=suites, today's whole sequence — the stronger check."));
                GateMode::Suites
            }
        };
        self.s.gate_mode = mode.as_str().into();
        // A unit-mode PASS is not a suites-mode PASS: the mode is part of what was judged.
        // Suites mode hashes the command alone, so today's keys are unchanged.
        let cmd_text = tree_def.as_ref().map(def::Def::key_text).unwrap_or_else(|| cmd.clone());
        let key_cmd = match mode {
            GateMode::Unit => format!("{cmd_text}\n# gate_mode=unit"),
            GateMode::Suites => cmd_text,
        };

        // THE VERDICT CACHE.
        let suites_mode = ctx.var_or("SPIRA_GATE_SUITES", "on").to_string();
        let verdict_dir = self.s.verdict_dir.clone();
        if let Some(h) = w.harness_hash() {
            self.s.harness_h = h.clone();
            self.s.key = key::gate_key(&KeyInputs {
                repo: &name,
                tree: &merged_tree,
                files: &files,
                cmd: &key_cmd,
                harness_h: &h,
                suites: &suites_mode,
                bead: if self.s.bead.is_empty() {
                    "none"
                } else {
                    &self.s.bead
                },
                ejected: &ejected,
            });
        }
        let mut cached: Option<Verdict> = None;
        if !self.s.key.is_empty() {
            if let Some(entry) = w.read(&verdict_dir.join(&self.s.key)) {
                if let Some((when, by)) =
                    key::cache_fresh(&entry, ctx.var_or("SPIRA_VERDICT_TTL", "0"), w.now())
                {
                    self.s.pass_suites = key::cached_suites(&entry);
                    let hit = v(PASS, "cached", format!(
                        "gate: this exact tree already passed {name}'s gate at {when} ({by})\ngate: key {} — same tree, same changed files, same command, same harness.\ngate: gate PASS covered suites: {}",
                        self.s.key, key::cached_suites(&entry)));
                    if !self.a.release_bins {
                        return hit;
                    }
                    cached = Some(hit);
                }
            }
        }

        let timeout = ctx.var_or("SPIRA_GATE_TIMEOUT", "2700").to_string();
        let lock_wait: u64 = key::digits(ctx.var("SPIRA_GATE_LOCK_WAIT"))
            .unwrap_or_else(|| key::digits(&timeout).unwrap_or(2700) * 4);

        // HOST-WIDE ADMISSION (fences-only certification takes no slot — unless the round
        // named suites against this bead: the re-entry phase runs them, sp-p3srm).
        //
        // sp-q20wb: a gate that blocks here used to print nothing at all — not even the
        // composition line, so a genuine queue looked exactly like a hang — and gate.log's
        // `waited=` measured only the tree-lock wait after admission, never this one. Both
        // are fixed below: a first message on blocking (and one on being admitted, if the
        // wait was not instant), and the wait accumulated into `self.s.waited` so the
        // tree-lock section's own wait adds to it instead of overwriting it.
        if suites_mode != "off" || !ejected.trim().is_empty() || cached.is_some() {
            let dir = PathBuf::from(format!("{}/gate-admission", self.s.run));
            w.mkdir_p(&dir);
            let t0 = w.now();
            let mut said: Option<u64> = None;
            'wait: loop {
                // Re-read on every pass (sp-q20wb): a raised SPIRA_CERTIFY_PAR in the config
                // document must reach a gate already waiting, which the frozen `Ctx` —
                // captured once when lib.sh sourced conf.sh at this process's start — cannot.
                // `admission_wait_line` below takes this same `par`, never re-deriving its
                // own: one source for the pool's size, not two.
                let par = self.admission_par(&ctx);
                for slot in 1..=par {
                    if w.admission_try(&dir, slot, &br) {
                        if said.is_some() {
                            w.eprint(&format!("gate: admitted to a gate slot after {}s", w.now().saturating_sub(t0)));
                        }
                        break 'wait;
                    }
                }
                // Visible, never silent (sp-f4ig1): who holds the pool, first and every 60s.
                if said.map_or(true, |t| w.now().saturating_sub(t) >= 60) {
                    w.eprint(&format!("gate: {}", w.admission_wait_line(&self.s.run, par)));
                    said = Some(w.now());
                }
                if w.signalled() {
                    self.s.waited = w.now() - t0;
                    return v(
                        NOVERDICT,
                        "died",
                        "gate: signalled while waiting for admission",
                    );
                }
                if w.now().saturating_sub(t0) >= lock_wait {
                    self.s.waited = w.now() - t0;
                    return v(NOVERDICT, "admission-timeout", format!(
                        "gate: all {par} host-wide gate admission slots busy for {lock_wait}s — no verdict on {br}\ngate: this is host-wide gate concurrency (SPIRA_CERTIFY_PAR={par}), not a fault in the branch."));
                }
                w.sleep_ms(1000);
            }
            // sp-q20wb: gate.log's `waited=` covers admission too now, not only the tree
            // lock that follows it — the tree-lock section below adds its own wait to this
            // instead of overwriting it.
            self.s.waited = w.now() - t0;
        }

        // THE TREE, locked for the whole trial.
        let tree = PathBuf::from(format!(
            "{}/worktree/.gate.{}.{}",
            self.s.run,
            repo.file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default(),
            parse::tree_key(&br)
        ));
        if let Some(sweep) = w.which("gate-sweep.sh") {
            w.sweep(&sweep, &repo);
        }
        if let Some(parent) = tree.parent() {
            w.mkdir_p(parent);
        }
        let lock = PathBuf::from(format!("{}.lock", tree.display()));
        if !w.tree_lock_open(&lock) {
            return v(
                NOVERDICT,
                "no-lockfile",
                format!(
                    "gate: cannot open the gate tree's lock at {}",
                    lock.display()
                ),
            );
        }
        // sp-q20wb: carried forward so an admission wait above adds to this one in
        // gate.log's `waited=`, rather than the tree-lock wait overwriting it.
        let admission_waited = self.s.waited;
        let t0 = w.now();
        loop {
            if w.tree_lock_try() {
                break;
            }
            if w.signalled() {
                self.s.waited = admission_waited + w.now().saturating_sub(t0);
                return v(
                    NOVERDICT,
                    "died",
                    "gate: signalled while waiting for the gate tree",
                );
            }
            if w.now().saturating_sub(t0) >= lock_wait {
                self.s.waited = admission_waited + (w.now() - t0);
                self.s.start = w.now();
                return v(NOVERDICT, "lock-timeout", format!(
                    "gate: another gate has held {} for {lock_wait}s — no verdict on {br}\ngate: this is a queue, not a fault in the branch; retry, or raise SPIRA_GATE_LOCK_WAIT.",
                    tree.display()));
            }
            w.sleep_ms(200);
        }
        self.s.held_lock = true;
        self.s.tree = Some(tree.clone());
        let tree_waited = w.now() - t0;
        self.s.waited = admission_waited + tree_waited;
        self.s.start = w.now();
        if tree_waited > 0 {
            w.eprint(&format!("gate: waited {tree_waited}s for {}", tree.display()));
        }
        w.write_holder(&PathBuf::from(format!("{}.lock.holder", tree.display())));

        let want = w
            .rev_parse(&repo, &format!("{rev}^{{commit}}"))
            .unwrap_or_default();
        if let Err(e) = self.gate_at(&repo, &tree, &rev, &want) {
            w.eprint(&e);
            return v(NOVERDICT, "tree-unidentified", "");
        }

        // A cached PASS keeps its verdict, but --release-bins still needs the judged tree's binaries.
        if let Some(hit) = cached {
            let mut _reservation = None;
            if let Some(e) = self.prepare_build_tree(&ctx, &tree, &mut _reservation) {
                return e;
            }
            return hit;
        }

        let list = if status_list.trim().is_empty() {
            files.clone()
        } else {
            status_list.trim_end_matches('\n').to_string()
        };
        self.s.filelist = w.temp_file(&format!("{list}\n"));

        // THE COMPOSITION: what the branch touches decides what runs.
        let comp = self.composition(mode, &ctx, &repo, &base_rev, &rev, &tree);
        // THE RE-ENTRY CHECK: the suites the round named, against the tree under test.
        let mut named = ejected.clone();
        if let Ok(changed) = w.diff_raw(&repo, &base_rev, &rev) {
            for s in compose::touched_suites(&changed) {
                named.push(' ');
                named.push_str(&s);
            }
        }
        let re = compose::reentry(&named, |s| w.exists(&tree.join("spira").join(s)));
        self.s.compose = comp.label();
        self.s.branch_type = crate::telemetry::branch_type(&crate::telemetry::shape(
            w, mode, &comp, &ctx, &repo, &base_rev, &rev, &tree,
        ))
        .into();
        if !re.required.is_empty() {
            self.s.compose.push_str("+reentry");
        }
        w.eprint(&describe(&comp));
        if let Some(line) = describe_reentry(&self.s.bead, &re) {
            w.eprint(&line);
        }
        let jobs = compose::jobs(key::digits(&ctx.host_cores).unwrap_or(1));

        let env = |branch: &str, repeat: &str, c: &Composition| -> Vec<(String, String)> {
            let e = |k: &str, v: &str| (k.to_string(), v.to_string());
            vec![
                // Set outright from SPIRA_RELEASE (sp-31gtu): the release's bin/ and spira/, the
                // system dirs, then cargo for the tree builds. Never the inherited PATH.
                e("PATH", &self.s.path),
                e(spira_config::RELEASE_ENV, ctx.var(spira_config::RELEASE_ENV)),
                // Everything the trial runs is on the gate's admission (sp-f4ig1): its testenv and
                // cargo take no compile or test slot of their own (no hold-and-wait, no deadlock).
                e(spira_config::admission::INHERIT_ENV, "gate"),
                e("HOME", ctx.var("HOME")),
                e("TERM", "dumb"),
                e("SPIRA_GATE_REPO", &repo.to_string_lossy()),
                e("SPIRA_GATE_REPO_NAME", &name),
                e("SPIRA_GATE_BRANCH", branch),
                e("SPIRA_GATE_BASE", &base_rev),
                e("SPIRA_GATE_SELECT_HEAD", &br),
                e(
                    "SPIRA_GATE_FILES",
                    &self
                        .s
                        .filelist
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                ),
                e("SPIRA_GATE_EJECTED_SUITES", &ejected),
                e("SPIRA_GATE_ALL", ctx.var_or("SPIRA_GATE_ALL", "0")),
                // Suites off for a unit or fences composition: the gate string runs its
                // fences (a unit composition without the build fence, sp-aprxm) and
                // selects nothing (the selector, `suite-select gate`). The always-
                // covers carve-out is cleared with it; its default, spira/lib.sh, is a
                // script, and a script composes as suites, so no carve-out can apply here.
                e(
                    "SPIRA_GATE_SUITES",
                    if c.suites_off() { "off" } else { &suites_mode },
                ),
                e(
                    "SPIRA_CERTIFY_ALWAYS_COVERS",
                    if c.suites_off() {
                        ""
                    } else {
                        ctx.var("SPIRA_CERTIFY_ALWAYS_COVERS")
                    },
                ),
                e("SPIRA_BATCH_MAXPAR", ctx.var("SPIRA_BATCH_MAXPAR")),
                e("SPIRA_VERDICT_REPEAT_CONSIDERED", repeat),
                e("SPIRA_GATE_BUDGET", ctx.var_or("SPIRA_GATE_BUDGET", "300")),
                e("SPIRA_RUN", &self.s.run),
                // The runner's budget split and warm path (testenv DESIGN.md §11): the
                // operator's knobs reach the trial they tune; unset = the runner's defaults.
                e("SPIRA_TESTENV_SETUP_SHARE", ctx.var("SPIRA_TESTENV_SETUP_SHARE")),
                e("SPIRA_TESTENV_WARM_SLOTS", ctx.var("SPIRA_TESTENV_WARM_SLOTS")),
                // The build cache's switch reaches testenv, which resolves its own wrapper.
                e(spira_config::build::CACHE_ENV, ctx.var(spira_config::build::CACHE_ENV)),
            ]
            .into_iter()
            .chain(self.s.build_env.iter().cloned())
            .collect()
        };

        // A TRIAL THAT BUILDS IN THE TREE (the tools phase, the unit phases, --release-bins)
        // needs the build cache — absent, it refuses before any build (sp-z61hj) — and builds
        // on tmpfs.
        let mut _scratch_reservation: Option<Box<dyn std::any::Any>> = None;
        let builds = tree_def.as_ref().is_some_and(|d| !d.bins.is_empty())
            || matches!(comp, Composition::Unit { .. })
            || self.a.release_bins;
        if builds {
            if let Some(e) = self.prepare_build_tree(&ctx, &tree, &mut _scratch_reservation) {
                return e;
            }
        }
        // THE TOOLS ARE THE TREE'S, PROVABLY (sp-g9f3t): keyed by the tree id the gate tree
        // holds, which must be the merge's.
        let want_tree = w.rev_parse(&repo, &format!("{rev}^{{tree}}")).unwrap_or_default();
        let tools = match tools_for(w, &tree, &want_tree, tree_def.as_ref(), jobs) {
            Ok(t) => t,
            Err(e) => {
                return v(NOVERDICT, "tools-unattributed", format!("{e}\ngate: no trial of {br} ran — refusing to judge with tools it cannot attribute."));
            }
        };
        let (rc, out, ph) = run_composed(
            w,
            &tree,
            &comp,
            &with_bins(
                env(&label, ctx.var("SPIRA_VERDICT_REPEAT_CONSIDERED"), &comp),
                tree_def.as_ref(),
                tools.as_ref(),
            ),
            &timeout,
            &cmd,
            tools.as_ref(),
            jobs,
            &re.required,
            "",
        );
        self.s.phases.extend(ph);
        self.s.queue_secs = parse::queue_secs(&out);
        if w.signalled() {
            return v(
                NOVERDICT,
                "died",
                format!("gate: signalled during the trial of {br}"),
            );
        }
        // EVERY FENCE PROVES IT CHECKED (sp-ufbkh). The fences the string that ran names must
        // each have printed `fence: <name> checked <n> <unit>` with n > 0; a fence that exited
        // 0 without it (skipped, a precondition unmet, empty input) judged nothing, and a
        // trial that judged nothing is never a PASS.
        let fences = fence::expected(&compose::gate_string(&comp, &cmd).0);
        if rc == 0 {
            let quiet = fence::silent(&fences, &out);
            if !quiet.is_empty() {
                self.s.suite = quiet[0].clone();
                return v(NOVERDICT, "fence-silent", format!(
                    "gate: fence-silent — the gate exited 0, but these fences printed no `fence: <name> checked <n> <unit>` line (or checked 0): {}\ngate: a fence that checked nothing proves nothing; a fence that cannot check must exit non-zero naming what is missing.\n{}",
                    quiet.join(" "),
                    parse::tail_bytes(&out, 4000)));
            }
            let missing = compose::unproven(&out, &re.required);
            if !missing.is_empty() {
                self.s.suite = missing[0].clone();
                return v(NOVERDICT, "reentry-unproven", format!(
                    "gate: the round returned {} with suites named against it, and this gate did not see them pass: {}\ngate: each must report ok (a skip, a deferral or no line proves nothing) before the bead re-certifies.\n{}",
                    if self.s.bead.is_empty() { "this branch" } else { &self.s.bead },
                    missing.join(" "),
                    parse::tail_bytes(&out, 4000)));
            }
            let ran = parse::ran_suites(&out);
            if let Some(mismatch) = summary_mismatch(ran.len(), parse::verdict_ran(&out)) {
                return v(NOVERDICT, "summary-mismatch", format!(
                    "gate: summary-mismatch — {mismatch}\ngate: a summary that names fewer suites than the runner executed does not record what was certified.\n{}",
                    parse::tail_bytes(&out, 4000)));
            }
            let pass_suites = if ran.is_empty() {
                "-".to_string()
            } else {
                ran.join(",")
            };
            self.s.pass_suites = pass_suites.clone();
            if !self.s.key.is_empty() {
                w.mkdir_p(&verdict_dir);
                let by = ctx.var_or("SPIRA_GATE_CALLER", &br).to_string();
                let entry = key::render_entry(&w.utc(), w.now(), &by, &name, &br, &pass_suites);
                w.write_atomic(
                    &verdict_dir,
                    &self.s.key,
                    &format!("{entry}compose={}\n", self.s.compose),
                );
            }
            let checked = fence::summary(&fences, &out);
            return v(
                PASS,
                "pass",
                if checked.is_empty() {
                    format!("gate: gate PASS covered suites: {pass_suites}")
                } else {
                    format!("gate: fences checked: {checked}\ngate: gate PASS covered suites: {pass_suites}")
                },
            );
        }
        let out = if out.trim().is_empty() {
            format!("(the command printed nothing; it exited {rc})")
        } else {
            out
        };
        self.s.suite = parse::red_suites(&out)
            .into_iter()
            .next()
            .unwrap_or_else(|| "-".into());

        if rc == NOVERDICT && out.contains(TOOLS_UNATTRIBUTED) {
            return v(NOVERDICT, "tools-unattributed", format!(
                "{out}\ngate: no verdict on {br} — its tools could not be attributed to the tree under test."));
        }
        if spira_config::scratch::is_exhaustion(&out) {
            return v(NOVERDICT, "scratch-short", format!(
                "gate: {name}'s build ran out of scratch space mid-trial — the host's room, not a fault in {br}.\n{}",
                parse::tail_bytes(&out, 4000)));
        }
        if let Some(d) = parse::harness_fault_detail(&out) {
            return v(NOVERDICT, "harness-fault", format!(
                "gate: {name}'s batch reported a harness fault — container died mid-batch ({d}).\ngate: command: {cmd}\n{out}"));
        }
        if rc == 124 {
            return v(NOVERDICT, "timeout", format!(
                "gate: {name}'s own gate was killed at {timeout}s — it judged nothing.\ngate: command: {cmd}\ngate: this is the harness's budget, not a fault in the branch; raise SPIRA_GATE_TIMEOUT.\n{out}"));
        }
        // THE BUDGET IS THE TRIAL'S (testenv DESIGN.md D9, sp-govet): a trial whose setup did
        // not finish inside its share of SPIRA_GATE_BUDGET, or whose every suite was deferred,
        // judged nothing — a named NO_VERDICT, never a pass and never the branch's red.
        if rc == NOVERDICT {
            if parse::testenv_fault_reason(&out).as_deref() == Some("queue") {
                return v(NOVERDICT, "queue", format!(
                    "gate: {name}'s suites trial waited {}s for a testenv slot and gave up inside its share of SPIRA_GATE_BUDGET={}s; it judged nothing.\ngate: this is the host's queue, not a fault in the branch.\ngate: command: {cmd}\n{out}",
                    parse::queue_secs(&out),
                    ctx.var_or("SPIRA_GATE_BUDGET", "300")));
            }
            if let Some(r) = parse::testenv_fault_reason(&out).filter(|r| r.starts_with("deadline-")) {
                let phase = &r["deadline-".len()..];
                return v(NOVERDICT, "budget", format!(
                    "gate: {name}'s suites trial did not fit its budget — phase `{phase}` was cut at its share of SPIRA_GATE_BUDGET={}s; it judged nothing.\ngate: command: {cmd}\n{out}",
                    ctx.var_or("SPIRA_GATE_BUDGET", "300")));
            }
            return v(NOVERDICT, "harness-fault", format!(
                "gate: {name}'s own gate reported a harness fault (exit {NOVERDICT}) — container or install failed.\ngate: command: {cmd}\n{out}"));
        }

        // WHOSE FAULT: the same command on the landing ref itself — the pinned commit the
        // merge was cut from, never the ref re-read by name (sp-hh5h0).
        let mut base_ran = false;
        let (mut base_rc, mut base_out) = (1, String::new());
        // THE BASE TRIAL RUNS THE BASE'S OWN DEFINITION (sp-quu2w): the landing ref's
        // gate.steps, or its column when it never adopted one. A base definition that does
        // not read leaves the base untested — whose fault the red is stays unestablished.
        let base_gate = match &base_def {
            Ok(Resolved::Tree(d)) => Some((d.command(), Some(d.clone()))),
            Ok(Resolved::Column(c)) => Some((c.clone(), None)),
            Ok(Resolved::Refused { .. }) => None,
            Err(e) => {
                w.eprint(&format!("gate: {base}'s own {} does not read — no base trial: {e}", def::PATH));
                None
            }
        };
        // THE BASE'S TOOLS ARE THE BASE'S, PROVABLY (sp-g9f3t): after the checkout, the gate
        // tree must hold the base's tree id, and the tools the steps read are keyed by it —
        // never the branch trial's build left in target/aeon, never another tree's. One that
        // cannot be attributed leaves the base untested (BaseUntestable), never judged.
        let base_tree = w.rev_parse(&repo, &format!("{base_rev}^{{tree}}")).unwrap_or_default();
        let base_tools = base_gate.as_ref().and_then(|(_, bdef)| {
            if let Err(e) = self.gate_at(&repo, &tree, &base_rev, &base_rev) {
                w.eprint(&e);
                return None;
            }
            match tools_for(w, &tree, &base_tree, bdef.as_ref(), jobs) {
                Ok(t) => Some(t),
                Err(e) => {
                    w.eprint(&format!("{e}\ngate: no base trial — {base}'s tools cannot be attributed to {base}'s tree."));
                    None
                }
            }
        });
        if let (Some((base_cmd, bdef)), Some(base_tools)) = (base_gate.as_ref(), base_tools.as_ref()) {
            // A branch red before its suites step ran (a fence, the selector) is judged on the
            // base's fences only: the base's suites answer no question this red asks, and they
            // were most of every such base trial's wall (sp-govet: base-gate 164-501 s behind
            // a 12 s fence red).
            //
            // sp-kqger: A SUITES COMPOSITION NEVER MIRRORS THE BRANCH'S SELECTION ON THE BASE
            // EITHER, for the same reason — it was the 275-337 s cost this bead exists to cut,
            // and it answered nothing the branch's own red suites did not already ask. The
            // base's fences run instead (cheap, ~12-60 s); each red suite is judged by the
            // base-suite cache below, or a targeted rerun that pays for only the suites the
            // cache could not answer.
            let base_comp = if matches!(comp, Composition::Suites { .. }) {
                Composition::Fences
            } else {
                self.base_composition(&comp, &ctx, &tree)
            };
            let (r, o, ph) = run_composed(
                w,
                &tree,
                &base_comp,
                &with_bins(
                    env(
                        &base_rev,
                        "base trial — confirming whether base is independently red",
                        &base_comp,
                    ),
                    bdef.as_ref(),
                    base_tools.as_ref(),
                ),
                &timeout,
                base_cmd,
                base_tools.as_ref(),
                jobs,
                &re.required,
                "base-",
            );
            self.s.phases.extend(ph);
            base_rc = r;
            base_out = o;
            base_ran = r != 124 && r != NOVERDICT;
        }
        if w.signalled() {
            return v(
                NOVERDICT,
                "died",
                format!("gate: signalled during the base trial for {br}"),
            );
        }
        // sp-kqger fix: the base's own `bin` tools (if its tree declares any, e.g. a
        // tree-owned testenv, sp-isom7) outlive the `if let` above, so the image-tag query
        // and the targeted rerun below can use them too — never bare names on the release's
        // PATH, which may not resolve at all for a tree whose testenv it cannot run.
        let base_bdef: Option<&def::Def> = base_gate.as_ref().and_then(|(_, d)| d.as_ref());
        let base_tools_ref: Option<&Tools> = base_tools.as_ref().and_then(Option::as_ref);

        // EACH RED IS JUDGED ON THE BASE ON THAT SUITE (sp-hh5h0). A branch red the base
        // trial did not run — its selection differed, or --deadline deferred it — is run on
        // the base now, by name, before anyone is blamed. A suite the base does not have is
        // the branch's own and needs no run.
        //
        // sp-kqger: THE BASE-SUITE CACHE. Since the base trial above never ran the branch's
        // suites at all (a suites composition is always judged fences-only now), every named
        // red suite reaches this loop. Each is answered by `basecache` — keyed on the base
        // tree (already this cache's own directory), the suite, the gate's harness and the
        // testenv image a rebuild of the suite container would change — or, on a miss, by the
        // same targeted rerun the bash's "unrun" suites always used, now paying for only the
        // suites the cache could not answer. A full cache hit skips the rerun entirely: a
        // flip-suspect (green in the cache, red on the branch) re-runs nothing extra.
        let mut absent: Vec<String> = Vec::new();
        if base_ran {
            let base_ran_set = parse::ran_suites(&base_out);
            let cacheable = matches!(comp, Composition::Suites { .. });
            let mut image_tag: Option<String> = None;
            let mut image_tag_tried = false;
            let mut unrun: Vec<String> = Vec::new();
            for s in parse::red_suites(&out) {
                if base_ran_set.contains(&s) {
                    continue;
                }
                if !w.ls_tree_has(&repo, &base_rev, &format!("spira/{s}")) {
                    absent.push(s);
                    continue;
                }
                if cacheable {
                    if !image_tag_tried {
                        image_tag_tried = true;
                        // A pure hash of the suite container's build closure in this tree
                        // (Containerfile, deps.toml) — the fourth key component, alongside
                        // the base tree, the suite and the harness. None when it cannot be
                        // read: every lookup below then misses, and nothing is cached.
                        let t = w.now();
                        let (tag_rc, tag_out) = w.run_gate(
                            &tree,
                            &with_bins(
                                env(
                                    &base_rev,
                                    "testenv image tag (sp-kqger base-suite cache)",
                                    &Composition::Fences,
                                ),
                                base_bdef,
                                base_tools_ref,
                            ),
                            &timeout,
                            "\"${SPIRA_TESTENV_BIN:-testenv}\" container tag",
                        );
                        self.s
                            .phases
                            .push(("base-image-tag".into(), w.now().saturating_sub(t)));
                        image_tag = (tag_rc == 0)
                            .then(|| tag_out.trim().to_string())
                            .filter(|s| !s.is_empty());
                    }
                    if let Some(tag) = &image_tag {
                        let hit = basecache::path(&self.s.verdict_dir, &self.s.repo_name, &base_tree, &s)
                            .and_then(|p| w.read(&p))
                            .and_then(|e| basecache::fresh(&e, &self.s.harness_h, tag));
                        if let Some(pass) = hit {
                            base_out.push_str(&format!(
                                "\n  {s:<32} {} cached (sp-kqger base-suite cache)",
                                if pass { "ok     " } else { "RED    " }
                            ));
                            if !pass {
                                base_rc = base_rc.max(1);
                            }
                            continue;
                        }
                    }
                }
                unrun.push(s);
            }
            if !unrun.is_empty() {
                let rerun = base_rerun_cmd(&unrun);
                let t = w.now();
                let (r, o) = w.run_gate(
                    &tree,
                    &with_bins(
                        env(
                            &base_rev,
                            "base trial — the branch's red suites the base trial did not run",
                            &Composition::Suites {
                                why: "base-rerun".into(),
                            },
                        ),
                        base_bdef,
                        base_tools_ref,
                    ),
                    &timeout,
                    &rerun,
                );
                self.s
                    .phases
                    .push(("base-rerun".into(), w.now().saturating_sub(t)));
                if w.signalled() {
                    return v(
                        NOVERDICT,
                        "died",
                        format!("gate: signalled during the base trial for {br}"),
                    );
                }
                if !o.is_empty() {
                    base_out.push_str(&format!(
                        "\ngate: re-ran on {base} ({}): {}\n{o}",
                        short(&base_rev),
                        unrun.join(" ")
                    ));
                }
                if r != 0 && base_rc == 0 {
                    base_rc = r;
                }
                // sp-kqger: warm the cache with whatever this rerun actually judged. A suite
                // it never reported on (a fault, an unexpected timeout) is left uncached —
                // the next gate asks again rather than trusting a run that did not answer.
                if let Some(tag) = &image_tag {
                    let ran = parse::ran_suites(&o);
                    let fresh_reds = parse::red_suites(&o);
                    let when = w.utc();
                    let at = w.now();
                    for s in &unrun {
                        if !ran.contains(s) {
                            continue;
                        }
                        let Some(p) = basecache::path(&self.s.verdict_dir, &self.s.repo_name, &base_tree, s) else {
                            continue;
                        };
                        let Some(dir) = p.parent() else { continue };
                        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                            continue;
                        };
                        w.mkdir_p(dir);
                        w.write_atomic(
                            dir,
                            name,
                            &basecache::render(!fresh_reds.contains(s), &self.s.harness_h, tag, &when, at),
                        );
                    }
                }
            }
        }
        if spira_config::scratch::is_exhaustion(&base_out) {
            return v(NOVERDICT, "scratch-short", format!(
                "gate: {name}'s base trial ran out of scratch space — the host's room; it judged neither {base} nor {br}.\n{}",
                parse::tail_bytes(&base_out, 4000)));
        }
        let spaced = |v: &[String]| v.join(" ");
        let attribution = parse::attribute(&out, base_ran, base_rc, &base_out, &absent);
        if matches!(attribution, Attribution::BaseRed(_)) {
            let reds = parse::red_suites(&base_out);
            let timeouts = parse::timed_out_suites(&base_out);
            let retryable = !reds.is_empty()
                && reds.iter().all(|s| {
                    !timeouts.contains(s) && w.ls_tree_has(&repo, &base_rev, &format!("spira/{s}"))
                });
            if retryable {
                let t = w.now();
                let (_, o) = w.run_gate(
                    &tree,
                    &with_bins(
                        env(
                            &base_rev,
                            "base trial — a base red must reproduce before it holds a branch",
                            &Composition::Suites {
                                why: "base-rerun".into(),
                            },
                        ),
                        base_bdef,
                        base_tools_ref,
                    ),
                    &timeout,
                    &base_rerun_cmd(&reds),
                );
                self.s
                    .phases
                    .push(("base-retry".into(), w.now().saturating_sub(t)));
                if w.signalled() {
                    return v(
                        NOVERDICT,
                        "died",
                        format!("gate: signalled during the base trial for {br}"),
                    );
                }
                let ran = parse::ran_suites(&o);
                let again = parse::red_suites(&o);
                if reds.iter().all(|s| ran.contains(s) && !again.contains(s)) {
                    self.s.suite = reds[0].clone();
                    return v(NOVERDICT, "base-flake", format!(
                        "gate: {name}'s base trial was red on {} but did not reproduce on a second run of {base} — a flake, not a base red.\ngate: no verdict for {br}; it is re-gated, not held.\n--- {base}'s first output ---\n{}\n--- the retry ---\n{o}",
                        spaced(&reds), parse::tail_bytes(&base_out, 4000)));
                }
            }
        }
        match attribution {
            Attribution::BranchRed(s) => {
                self.s.suite = s;
                if base_ran && base_rc != 0 {
                    let base_reds = parse::red_suites(&base_out);
                    let only: Vec<String> = parse::red_suites(&out).into_iter().filter(|x| !base_reds.contains(x)).collect();
                    return v(FAIL, "branch-red", format!(
                        "gate: {name}'s own gate failed: {cmd}\n{out}\ngate: red on this branch and not on {base}: {}\ngate: {base} is red too, on: {}",
                        spaced(&only), spaced(&base_reds)));
                }
                v(FAIL, "branch-red", format!(
                    "gate: {name}'s own gate failed: {cmd}\n{out}\ngate: the same command passes against {base}, so this is the branch's own."))
            }
            Attribution::BaseTimeout(s) => {
                self.s.suite = s;
                let t: String = parse::timed_out_suites(&base_out).iter().fold(String::new(), |acc, x| acc + x + " ");
                v(NOVERDICT, "base-timeout", format!(
                    "gate: {name}'s base trial timed out on {t}— no verdict for {br}.\ngate: a killed suite cannot prove the base is broken; retry when the box is quieter."))
            }
            Attribution::BaseRed(s) => {
                self.s.suite = s;
                let reds = parse::red_suites(&base_out).join("\n");
                let reds = if reds.is_empty() { "(no suite named; read the output)".to_string() } else { reds };
                v(BASEFAIL, "base-red", format!(
                    "gate: {name}'s own gate fails against {base} — this branch did not cause it.\ngate: command: {cmd}\ngate: red on {base}: {reds}\n--- {base}'s own output ---\n{}\n--- this branch's output ---\n{}\ngate: fix the repository, or clear that command from {map}.",
                    parse::tail_bytes(&base_out, 8000), parse::tail_bytes(&out, 4000)))
            }
            Attribution::BaseUntestable => {
                // sp-e5v53-2: name exactly why, from whatever the base side actually
                // produced — the fences' own failure when base_ran never became true at
                // all, or the targeted rerun's own fault output (a testenv VERDICT FAULT,
                // its stderr) when it ran but could not judge. An attempt that produced
                // nothing at all is said as that, never silently as "cannot be established"
                // alone — a reader should never have to go read gate.log's phases to learn
                // whether the base was tried.
                let why = if base_out.trim().is_empty() {
                    "gate: the base side produced no output at all — see gate.log's phases for whether it ran.".to_string()
                } else {
                    format!("--- {base}'s own attempt ---\n{}", parse::tail_bytes(&base_out, 4000))
                };
                v(NOVERDICT, "base-untestable", format!(
                    "gate: {name}'s own gate failed: {cmd}\n{out}\ngate: and the same command could not be tried against {base}, so whose fault this is\ngate: cannot be established — refusing to charge it to the branch on a guess.\n{why}"))
            }
        }
    }

    /// DESIGN.md "Composition": suites mode never looks; unit mode reads the touched set
    /// (landing ref → the revision under test) and the workspace graph of the gate tree.
    #[allow(clippy::too_many_arguments)]
    fn composition(
        &self,
        mode: GateMode,
        ctx: &Ctx,
        repo: &Path,
        base: &str,
        rev: &str,
        tree: &Path,
    ) -> Composition {
        if mode == GateMode::Suites {
            return Composition::Suites { why: "mode".into() };
        }
        let changed = match self.w.diff_raw(repo, base, rev) {
            Ok(c) => c,
            Err(e) => {
                self.w.eprint(&format!(
                    "gate: cannot read the touched set {base}..{rev}: {e}"
                ));
                return Composition::Suites {
                    why: "no-diff".into(),
                };
            }
        };
        let forces = Forces {
            gate_all: ctx.var("SPIRA_GATE_ALL") == "1",
        };
        let members = self.members(ctx, tree);
        if let Err(e) = &members {
            self.w.eprint(&format!(
                "gate: cargo metadata failed in the gate tree: {e}"
            ));
        }
        compose::compose(
            mode,
            &forces,
            &changed,
            members.as_deref().map_err(String::as_str),
        )
    }

    fn members(&self, ctx: &Ctx, tree: &Path) -> Result<Vec<compose::Member>, String> {
        self.w
            .cargo_metadata(tree, &self.s.path, ctx.var("HOME"))
            .and_then(|j| compose::parse_metadata(&j))
    }

    /// The base trial runs the same composition, over the crates the base has: a crate the
    /// branch adds cannot be tested on a base without it, and its absence is not a red.
    fn base_composition(&self, comp: &Composition, ctx: &Ctx, tree: &Path) -> Composition {
        let Composition::Unit { touched, crates } = comp else {
            return comp.clone();
        };
        let Ok(members) = self.members(ctx, tree) else {
            return comp.clone();
        };
        let crates: Vec<String> = crates
            .iter()
            .filter(|c| members.iter().any(|m| &m.name == *c))
            .cloned()
            .collect();
        if crates.is_empty() {
            return Composition::Fences;
        }
        Composition::Unit {
            touched: touched.clone(),
            crates,
        }
    }

    /// `gate_at`: through the port, which proves HEAD == want.
    fn gate_at(&self, repo: &Path, tree: &Path, rev: &str, want: &str) -> Result<(), String> {
        if want.is_empty() {
            return Err(format!(
                "gate: cannot resolve {rev} in {}",
                self.s.repo_name
            ));
        }
        self.w.checkout(repo, tree, rev, want)
    }

    /// EJECTED SUITES: `<bead>.ejected`, else an EJECTED landstate row's fourth field.
    fn ejected(&self, ctx: &Ctx) -> String {
        if self.s.bead.is_empty() {
            return String::new();
        }
        let ls = ctx.var("LANDSTATE");
        let ls = if ls.is_empty() { "/nonexistent" } else { ls };
        let ej = PathBuf::from(format!("{ls}/{}.ejected", self.s.bead));
        if self.w.exists(&ej) {
            return self
                .w
                .read(&ej)
                .and_then(|c| c.lines().next().map(str::to_string))
                .unwrap_or_default();
        }
        let row = PathBuf::from(format!("{ls}/{}", self.s.bead));
        if let Some(c) = self.w.read(&row) {
            let f: Vec<&str> = c.lines().next().unwrap_or("").split_whitespace().collect();
            if f.first() == Some(&"EJECTED") {
                return f.get(3).map(|s| s.to_string()).unwrap_or_default();
            }
        }
        String::new()
    }

    /// The config document's own `certify_par`, read fresh; else `SPIRA_CERTIFY_PAR` as this
    /// trial's `Ctx` froze it at start; else derived from the box. Called on every pass of the
    /// admission wait (sp-q20wb): the live config read is what lets a limit raised in the
    /// file admit a gate that is already waiting — the frozen `Ctx` value cannot change
    /// mid-trial, since conf.sh exported it once, before this process's `exec`.
    fn admission_par(&self, ctx: &Ctx) -> u64 {
        if let Some(n) = self.w.certify_par_live() {
            return n.max(1);
        }
        if let Some(n) = key::digits(ctx.var("SPIRA_CERTIFY_PAR")) {
            return n;
        }
        let par = (self.w.nproc_all() / 4).max(1);
        let par_mem = (self.w.mem_avail_mib() / 400).max(1);
        par.min(par_mem)
    }

    /// Every PASS certifies the merged tree it judged (cert.rs; queue/DESIGN.md §8 D12): the
    /// record `queue land-local` requires before it lands a head with that tree. Keyed by
    /// `(repo, tree)` only; a PASS that never reached the merge certifies nothing.
    fn certify(&self) {
        let w = self.w;
        let Some(dir) = cert::path(&self.s.verdict_dir, &self.s.repo_name, &self.s.merged_tree)
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
        else {
            return;
        };
        let c = cert::Cert {
            source: cert::Source::Gate,
            tree: self.s.merged_tree.clone(),
            repo: self.s.repo_name.clone(),
            rev: self.s.rev.clone(),
            branch: self.a.branch.clone(),
            by: self.s.caller.clone(),
            when: w.utc(),
            at: w.now(),
            harness: self.s.harness_h.clone(),
            suites: self.s.pass_suites.clone(),
        };
        w.mkdir_p(&dir);
        w.write_atomic(&dir, &self.s.merged_tree, &cert::render(&c));
    }

    /// The refusals a tree build meets before it starts: no build cache, no room on tmpfs.
    fn prepare_build_tree(&self, ctx: &Ctx, tree: &Path, reservation: &mut Option<Box<dyn std::any::Any>>) -> Option<Verdict> {
        let w = self.w;
        if let Some(e) = &self.s.cache_refusal {
            return Some(v(NOVERDICT, "no-build-cache", format!("gate: {e} — refusing to judge")));
        }
        let lim = crate::target::Limits::from_vars(
            ctx.var("SPIRA_GATE_TARGET_CAP_MIB"),
            ctx.var("SPIRA_GATE_TARGET_MIN_FREE_MIB"),
            ctx.var("SPIRA_GATE_TARGET_MIN_MEM_MIB"),
            ctx.var("SPIRA_TMPFS_SHED_FREE_MIB"),
            ctx.var("SPIRA_GATE_TARGET_RESERVE_MIB"),
        );
        let short = |e: String| Some(v(NOVERDICT, "scratch-short", format!("{e}\ngate: this is the host's room, not a fault in the branch.")));
        match w.target_on_tmpfs(tree, ctx.var("SPIRA_GATE_TARGET_ROOT"), &self.s.run, &lim) {
            Ok(line) => w.eprint(&line),
            Err(e) => return short(e),
        }
        match w.reserve_scratch(tree, ctx.var("SPIRA_GATE_TARGET_ROOT"), &self.s.run, &lim) {
            Ok(g) => {
                *reservation = Some(g);
                None
            }
            Err(e) => short(e),
        }
    }

    /// The one way out: meters, records, certifies and cleans up.
    /// `--release-bins` after a PASS (sp-z61hj): `cargo build --release --workspace` of the
    /// judged tree in the gate tree — its build directories are on tmpfs and it compiles
    /// through the build cache, so a hand landing writes nothing to the disk for it. A failed
    /// build empties `target/release`, so `queue land-local --worktree` can never ship an
    /// older tree's binaries from it (fail closed). The verdict is unchanged: it judged the
    /// tree; the binaries are the landing's.
    fn release_bins(&mut self) {
        let w = self.w;
        let Some(tree) = self.s.tree.clone() else { return };
        let e = |k: &str, v: &str| (k.to_string(), v.to_string());
        let mut env = vec![
            e("PATH", &self.s.path),
            e(spira_config::RELEASE_ENV, &self.s.release),
            e("HOME", &self.s.home_dir),
            e("TERM", "dumb"),
        ];
        env.extend(self.s.build_env.iter().cloned());
        let cmd = release_bins_command();
        let t0 = w.now();
        let (rc, out) = w.run_gate(&tree, &env, &self.s.timeout, &cmd);
        self.s.phases.push(("release-bins".into(), w.now().saturating_sub(t0)));
        let full = spira_config::room::is_space_failure(&out);
        if full && rc == 0 {
            w.run_gate(&tree, &env, &self.s.timeout, "find target/release -mindepth 1 -delete 2>/dev/null; true");
        }
        if rc == 0 && !full {
            w.eprint(&format!(
                "gate: --release-bins: the release binaries of tree {} are in {}/target/release — land with: queue land-local <repo> --head <sha> --members <…> --worktree {}",
                self.s.merged_tree,
                tree.display(),
                tree.display()
            ));
        } else {
            let tail: Vec<&str> = out.lines().rev().take(20).collect();
            w.eprint(&format!(
                "{}\ngate: --release-bins: the release build FAILED (exit {rc}){} — target/release emptied; there are no binaries to land",
                tail.into_iter().rev().collect::<Vec<_>>().join("\n"),
                if full { " writing to a full disk or an exhausted quota (EDQUOT/ENOSPC)" } else { "" }
            ));
        }
    }

    fn finish(&mut self, vd: Verdict) -> i32 {
        let mut vd = vd;
        let w = self.w;
        vd = settle(vd);
        if !vd.msg.is_empty() {
            w.eprint(&vd.msg);
        }
        let repo_name = if self.s.repo_name.is_empty() {
            "?"
        } else {
            &self.s.repo_name
        };
        w.eprint(&format!(
            "gate: VERDICT={} reason={} branch={} repo={} suite={}",
            outcome(vd.status),
            vd.reason,
            self.a.branch,
            repo_name,
            self.s.suite
        ));
        if vd.status == PASS {
            self.certify();
            if self.a.release_bins {
                self.release_bins();
            }
        }
        if let Some(f) = self.s.filelist.take() {
            w.remove(&f);
        }
        if let Some(t) = &self.s.tree {
            w.release_target(t, vd.status == PASS && self.a.release_bins);
            w.remove(&PathBuf::from(format!("{}.lock.holder", t.display())));
            if self.s.held_lock && vd.status != PASS && w.exists(&t.join(".git")) {
                w.remove_worktree(&self.s.repo, t);
            }
        }
        if self.s.armed {
            let ran = w.now().saturating_sub(self.s.start);
            w.append(
                &self.s.gate_log,
                &format!(
                    "{} {} {} waited={}s ran={}s rc={} {}{}\n",
                    w.utc(),
                    self.s.repo_name,
                    self.a.branch,
                    self.s.waited,
                    ran,
                    vd.status,
                    vd.reason,
                    meter_suffix(&self.s.compose, &self.s.phases, self.s.queue_secs)
                ),
            );
            let run = crate::telemetry::GateRun {
                repo: self.s.repo_name.clone(),
                branch: self.a.branch.clone(),
                bead: self.s.bead.clone(),
                caller: self.s.caller.clone(),
                status: outcome(vd.status).into(),
                rc: vd.status,
                reason: vd.reason.clone(),
                waited_secs: self.s.waited,
                ran_secs: ran,
                gate_mode: self.s.gate_mode.clone(),
                compose: self.s.compose.clone(),
                branch_type: self.s.branch_type.clone(),
                queue_secs: self.s.queue_secs,
                phases: meter_suffix(&self.s.compose, &self.s.phases, 0)
                    .split_once(" phases=")
                    .map(|(_, p)| p.to_string())
                    .unwrap_or_default(),
            };
            crate::telemetry::append(w, &self.s.run, &w.utc(), &run);
            if let Some(y) = w.which("yield.sh") {
                if vd.status == PASS {
                    w.yield_sh(
                        &y,
                        &self.s.run,
                        &[
                            "pass",
                            &self.s.repo_name,
                            &self.a.branch,
                            &self.s.branch_tree,
                        ],
                    );
                } else {
                    let st = vd.status.to_string();
                    w.yield_sh(
                        &y,
                        &self.s.run,
                        &[
                            "record",
                            &self.s.repo_name,
                            &self.a.branch,
                            &st,
                            &vd.reason,
                            &self.s.branch_tree,
                            &self.s.suite,
                        ],
                    );
                }
            }
        }
        if !self.s.bead.is_empty() && !self.s.branch_tip.is_empty() && self.s.branch_tip != "-" {
            let (o, d) = match vd.status {
                PASS => ("pass", self.s.key.clone()),
                NOVERDICT | BASEFAIL => ("infra", vd.reason.clone()),
                _ => ("red", vd.reason.clone()),
            };
            w.lc_certify(&self.s.bead, &self.s.branch_tip, o, &d);
        }
        vd.status
    }
}

fn summary_mismatch(listed: usize, runner_ran: Option<usize>) -> Option<String> {
    match runner_ran {
        Some(n) if listed < n => Some(format!(
            "the runner reported ran={n} but only {listed} suites could be named from its output"
        )),
        _ => None,
    }
}

/// The gate.log fields after the reason: ` compose=<label> phases=<name>:<secs>,… queue=<secs>`
/// (the last only when the trial waited for a slot), empty until a composition was chosen. Readers split the note on spaces and take its first word
/// as the reason (yield.sh), so trailing fields are compatible.
pub fn meter_suffix(compose: &str, phases: &[(String, u64)], queue_secs: u64) -> String {
    if compose.is_empty() {
        return String::new();
    }
    let ph: Vec<String> = phases.iter().map(|(n, t)| format!("{n}:{t}")).collect();
    let ph = if ph.is_empty() {
        "-".to_string()
    } else {
        ph.join(",")
    };
    let queue = if queue_secs > 0 {
        format!(" queue={queue_secs}")
    } else {
        String::new()
    };
    format!(" compose={compose} phases={ph}{queue}")
}

/// The line the trial prints once it has chosen what to run.
pub fn describe(c: &Composition) -> String {
    match c {
        Composition::Suites { why } => format!(
            "gate: composition=suites ({why}) — the repository's gate string, whole"
        ),
        Composition::Fences => {
            "gate: composition=fences — nothing buildable touched; fences only, suites off".into()
        }
        Composition::Unit { touched, crates } => format!(
            "gate: composition=unit — fences (suites off, no build fence: the build phase is the compile check), then cargo build and test on the host for: {} (touched: {})",
            crates.join(" "),
            touched.join(" ")
        ),
    }
}

/// The line the trial prints when the round named suites against the bead.
pub fn describe_reentry(bead: &str, r: &compose::Reentry) -> Option<String> {
    if r.required.is_empty() && r.gone.is_empty() && r.invalid.is_empty() {
        return None;
    }
    let who = if bead.is_empty() { "this branch" } else { bead };
    let mut s =
        format!(
        "gate: re-entry — the round named suites against {who}; each must pass in this gate: {}",
        if r.required.is_empty() { "-".to_string() } else { r.required.join(" ") }
    );
    if !r.gone.is_empty() {
        s.push_str(&format!(
            " (no longer on the tree, nothing to run: {})",
            r.gone.join(" ")
        ));
    }
    if !r.invalid.is_empty() {
        s.push_str(&format!(
            " (not suite names, ignored: {})",
            r.invalid.join(" ")
        ));
    }
    Some(s)
}

/// Run a composition's phases in order, each under what is left of `timeout`, stopping at
/// the first non-zero status. Returns (status, the phases' output joined, the phase walls).
/// The base re-run: the named suites through testenv, on the revision in
/// `SPIRA_GATE_BRANCH` (the pinned base), no deadline — the gate timeout bounds it. testenv's
/// 2 and 3 are its own faults, NO_VERDICT, as the gate string maps them. Names are
/// `red_suites` words; anything that is not a suite name (`is_suite_name`, the selector's
/// rule) is dropped, never quoted into a shell.
/// The base re-run's first words: a no-op that names it in a process listing and a trace.
pub const BASE_RERUN_MARK: &str = ": base-rerun; ";

pub fn base_rerun_cmd(suites: &[String]) -> String {
    let list: Vec<&str> = suites
        .iter()
        .map(String::as_str)
        .filter(|s| crate::compose::is_suite_name(s))
        .collect();
    // `${SPIRA_TESTENV_BIN:-testenv}` (sp-kqger fix): a tree that owns its own testenv
    // (`bin SPIRA_TESTENV_BIN testenv` in gate.steps, sp-isom7 — "the release's testenv
    // could not run such a branch's suites at all") needs THAT binary here too, never the
    // release's bare name on PATH, which may not even resolve. `with_bins` sets the
    // variable when the base's own definition declares one; unset, this falls back to
    // bare `testenv` exactly as it always did. Scar: before this, every targeted rerun
    // against such a tree exited 127 near-instantly, mapped to NO_VERDICT base-untestable
    // — invisible while this path was rare, then most of every red gate once sp-kqger
    // made it the primary one.
    //
    // `"$SPIRA_GATE_REPO"` (sp-e5v53-2 fix): the repository gate string always passes
    // testenv its repo as a second positional argument; this call never did, since sp-hh5h0.
    // Without it, `resolve_repo` falls back to `$SPIRA_REPO` (never set in the gate
    // command's `env -i` environment — DESIGN.md "The gate command's environment") or the
    // harness root, whose basename rarely matches a configured repo, so `landref` finds no
    // `base` column for it and the whole call faults `VERDICT FAULT rc=2 reason=base-ref`
    // before a single suite runs — mapped by the case clause below to NO_VERDICT, so
    // `base_out` never gains the suite's line at all: `BaseUntestable`, indistinguishable
    // from a rerun that genuinely could not judge. Scar: concierge/sp-g3uwp (red on
    // test-persona-model.sh) and sp-ooh1k (red on test-install-dolt-*), both after sp-e5v53
    // had already fixed the bare-`testenv`-name defect — a second, independent omission in
    // the same pre-existing call.
    format!(
        "{BASE_RERUN_MARK}_b=0; \"${{SPIRA_TESTENV_BIN:-testenv}}\" --suites {} \"$SPIRA_GATE_BRANCH\" \"$SPIRA_GATE_REPO\" || _b=$?; case \"$_b\" in 2|3|127) exit 75;; *) exit \"$_b\";; esac",
        list.join(",")
    )
}

fn short(rev: &str) -> &str {
    rev.get(..12).unwrap_or(rev)
}

/// The environment with each `bin` of a tree definition pointing at the binary built from
/// `tree` (sp-quu2w), replacing the installed one the context named.
/// Marks a trial that stopped because its tools could not be attributed to its tree.
pub const TOOLS_UNATTRIBUTED: &str = "gate: tools-unattributed";

/// A trial's tools (sp-g9f3t; DESIGN.md "Tools keyed by the tree they were built from"):
/// where they live, keyed by the tree id they were built from, and the build to run first
/// (None when that directory already holds this tree's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tools {
    pub build: Option<String>,
    pub tree: PathBuf,
    pub dir: PathBuf,
    pub id: String,
    pub pkgs: Vec<String>,
}

/// The tools of the trial about to run in `tree`, which must hold `want` (a git tree id).
/// Ok(None) when the definition builds nothing; Err naming the mismatch when the gate tree
/// holds another tree, or its HEAD cannot be read — the caller refuses, never guesses.
pub fn tools_for<W: World>(
    w: &W,
    tree: &Path,
    want: &str,
    d: Option<&def::Def>,
    jobs: u64,
) -> Result<Option<Tools>, String> {
    let Some(d) = d.filter(|d| !d.bins.is_empty()) else {
        return Ok(None);
    };
    let have = w.rev_parse(tree, "HEAD^{tree}").unwrap_or_default();
    if want.is_empty() || have != want {
        return Err(format!(
            "{TOOLS_UNATTRIBUTED}: {} holds tree {}, not {} — its tools would not be the tree's",
            tree.display(),
            if have.is_empty() { "<unreadable>" } else { &have },
            if want.is_empty() { "<unresolved>" } else { want }
        ));
    }
    let dir = def::Def::tools_dir(tree, want);
    let pkgs = d.packages();
    let build = if tools_proved(w, &dir, want, &pkgs).is_ok() {
        w.eprint(&format!("gate: tools for tree {want} reused from {} (built from this tree)", dir.display()));
        None
    } else {
        d.tools_command(jobs)
    };
    Ok(Some(Tools { build, tree: tree.to_path_buf(), dir, id: want.to_string(), pkgs }))
}

/// `dir` is stamped with `id` and holds every package.
fn tools_proved<W: World>(w: &W, dir: &Path, id: &str, pkgs: &[String]) -> Result<(), String> {
    let stamp = w.read(&dir.join(def::TOOLS_STAMP)).unwrap_or_default();
    if stamp.trim() != id {
        return Err(format!(
            "{TOOLS_UNATTRIBUTED}: {} is stamped '{}', not tree {id}",
            dir.display(),
            stamp.trim()
        ));
    }
    match pkgs.iter().find(|p| !w.exists(&dir.join(p))) {
        Some(p) => Err(format!("{TOOLS_UNATTRIBUTED}: {} lacks {p}", dir.display())),
        None => Ok(()),
    }
}

/// The `--release-bins` command (sp-z61hj): the release profile of the whole workspace,
/// one-shot, locked; on failure `target/release` is emptied so nothing stale can be landed.
pub fn release_bins_command() -> String {
    format!(
        "cargo build --release --workspace --locked {} || {{ _r=$?; find target/release -mindepth 1 -delete 2>/dev/null; exit $_r; }}",
        spira_config::build::one_shot_words("release")
    )
}

pub fn with_bins(
    mut env: Vec<(String, String)>,
    d: Option<&def::Def>,
    tools: Option<&Tools>,
) -> Vec<(String, String)> {
    let Some(t) = tools else { return env };
    for b in d.map(|d| d.bins.as_slice()).unwrap_or(&[]) {
        let p = t.dir.join(&b.package).to_string_lossy().into_owned();
        env.retain(|(k, _)| k != &b.var);
        env.push((b.var.clone(), p));
        // A tree-built testenv drives the tree's own harness (sp-isom7). It used to find it
        // by walking up from its own executable, which now resolves into the tmpfs build root
        // (sp-z61hj) — so the gate names it: the harness of the tree's testenv is the tree.
        if b.package == "testenv" {
            env.retain(|(k, _)| k != "SPIRA_TESTENV_HARNESS");
            env.push(("SPIRA_TESTENV_HARNESS".into(), t.tree.to_string_lossy().into_owned()));
        }
    }
    env
}

#[allow(clippy::too_many_arguments)]
pub fn run_composed<W: World>(
    w: &W,
    tree: &Path,
    comp: &Composition,
    env: &[(String, String)],
    timeout: &str,
    cmd: &str,
    tools: Option<&Tools>,
    jobs: u64,
    reentry: &[String],
    prefix: &str,
) -> (i32, String, Vec<(String, u64)>) {
    let budget = key::digits(timeout);
    let start = w.now();
    let left = || match budget {
        Some(b) => b
            .saturating_sub(w.now().saturating_sub(start))
            .max(1)
            .to_string(),
        None => timeout.to_string(),
    };
    let mut phases = Vec::new();
    // THE TOOLS PHASE (sp-quu2w): the binaries the steps call, built from the tree under
    // test, before any step runs. A tree whose tools do not build is judged like any red.
    // Built into the shared target/, then installed into the directory keyed by the tree id
    // (sp-g9f3t), and — built or reused — proved before any step reads it.
    if let Some(tl) = tools {
        if let Some(t) = &tl.build {
            let t0 = w.now();
            let (rc, out) = w.run_gate(tree, env, &left(), t);
            phases.push((format!("{prefix}tools"), w.now().saturating_sub(t0)));
            if rc != 0 || w.signalled() {
                return (rc, format!("{out}\ngate: phase 'tools' failed (exit {rc}): {t}"), phases);
            }
            if let Err(e) = w.install_tools(&tl.tree, &tl.pkgs, &tl.dir, &tl.id) {
                return (NOVERDICT, format!("{out}\n{TOOLS_UNATTRIBUTED}: {e}"), phases);
            }
        }
        if let Err(e) = tools_proved(w, &tl.dir, &tl.id, &tl.pkgs) {
            return (NOVERDICT, e, phases);
        }
    }
    let first = if comp.suites_off() { "fences" } else { "gate" };
    // A unit composition builds once (sp-aprxm): its build phase, not the build fence.
    let (cmd, _) = compose::gate_string(comp, cmd);
    let t = w.now();
    let (rc, mut out) = w.run_gate(tree, env, &left(), &cmd);
    phases.push((format!("{prefix}{first}"), w.now().saturating_sub(t)));
    if rc != 0 || w.signalled() {
        return (rc, out, phases);
    }
    let mut steps: Vec<(&str, String)> = match comp {
        Composition::Unit { crates, .. } => compose::unit_commands(crates, jobs).into(),
        _ => Vec::new(),
    };
    // THE RE-ENTRY PHASE, last: whatever of the named suites the phases before did not show
    // passing (all of them after a unit or fences gate or suites-off; only those the gate
    // string's budget deferred or skipped after a suites gate).
    let mut reentry_done = false;
    loop {
        let (name, c) = if !steps.is_empty() {
            steps.remove(0)
        } else if !reentry_done {
            reentry_done = true;
            let still = compose::unproven(&out, reentry);
            if still.is_empty() {
                break;
            }
            ("reentry", compose::reentry_command(&still))
        } else {
            break;
        };
        if budget.is_some_and(|b| w.now().saturating_sub(start) >= b) {
            out.push_str(&format!(
                "\ngate: SPIRA_GATE_TIMEOUT ({timeout}s) spent before the {name} phase"
            ));
            return (124, out, phases);
        }
        let t = w.now();
        let (r, o) = w.run_gate(tree, env, &left(), &c);
        phases.push((format!("{prefix}{name}"), w.now().saturating_sub(t)));
        if !o.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&o);
        }
        if r != 0 {
            out.push_str(&format!("\ngate: phase '{name}' failed (exit {r}): {c}"));
            return (r, out, phases);
        }
        if w.signalled() {
            return (r, out, phases);
        }
    }
    (0, out, phases)
}

/// `gate --definition [repo-name]` (sp-quu2w): the gate command the landing ref's tree
/// defines for the repository — its `gate.steps`, composed; or its repo-map column when the
/// landing ref never carried one. The one reader of a repository's gate outside a trial
/// (doctor.sh's compile-check check), so the resolution lives here and nowhere else.
pub fn definition<W: World>(w: &W, repo: Option<&str>) -> Result<String, String> {
    let ctx = w.context(repo)?;
    let name = ctx.repo_name.clone();
    let root = ctx
        .repo_root
        .clone()
        .ok_or_else(|| format!("repo-map has no entry for '{name}'"))?;
    let base = ctx
        .landref
        .clone()
        .ok_or_else(|| format!("cannot resolve the ref '{name}' lands on"))?;
    let root = PathBuf::from(root);
    let blob = w.show_blob(&root, &base, def::PATH);
    match def::resolve_base(blob.as_deref(), &ctx.gate_cmd)? {
        Resolved::Tree(d) => Ok(d.command()),
        Resolved::Column(c) => Ok(c),
        Resolved::Refused { why, .. } => Err(why),
    }
}
