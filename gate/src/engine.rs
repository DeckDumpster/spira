//! The trial, in the order DESIGN.md "Order of the trial" gives. One way out: [`Trial::finish`].

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
        let Some(skew) = w.which("skew.sh") else {
            return v(
                NOVERDICT,
                "missing-skew",
                "gate: skew.sh is not on PATH — refusing to land unchecked".to_string(),
            );
        };
        let (skew_rc, skew_out) = w.skew_foreign(&skew, &repo, &base_rev, &br);
        if skew_rc == 3 {
            return v(NOVERDICT, "skew-init-fault", format!(
                "gate: skew.sh could not initialize — conf.sh or the database may be unavailable.\ngate: the foreign-harness check did not run; this is a machinery fault, not a branch fault.\n{skew_out}"));
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
        if !self.s.key.is_empty() {
            if let Some(entry) = w.read(&verdict_dir.join(&self.s.key)) {
                if let Some((when, by)) =
                    key::cache_fresh(&entry, ctx.var_or("SPIRA_VERDICT_TTL", "0"), w.now())
                {
                    self.s.pass_suites = key::cached_suites(&entry);
                    return v(PASS, "cached", format!(
                        "gate: this exact tree already passed {name}'s gate at {when} ({by})\ngate: key {} — same tree, same changed files, same command, same harness.\ngate: gate PASS covered suites: {}",
                        self.s.key, key::cached_suites(&entry)));
                }
            }
        }

        let timeout = ctx.var_or("SPIRA_GATE_TIMEOUT", "2700").to_string();
        let lock_wait: u64 = key::digits(ctx.var("SPIRA_GATE_LOCK_WAIT"))
            .unwrap_or_else(|| key::digits(&timeout).unwrap_or(2700) * 4);

        // HOST-WIDE ADMISSION (fences-only certification takes no slot — unless the round
        // named suites against this bead: the re-entry phase runs them, sp-p3srm).
        if suites_mode != "off" || !ejected.trim().is_empty() {
            let dir = PathBuf::from(format!("{}/gate-admission", self.s.run));
            w.mkdir_p(&dir);
            let t0 = w.now();
            'wait: loop {
                let par = self.admission_par(&ctx);
                for slot in 1..=par {
                    if w.admission_try(&dir, slot) {
                        break 'wait;
                    }
                }
                if w.signalled() {
                    return v(
                        NOVERDICT,
                        "died",
                        "gate: signalled while waiting for admission",
                    );
                }
                if w.now().saturating_sub(t0) >= lock_wait {
                    return v(NOVERDICT, "admission-timeout", format!(
                        "gate: all {par} host-wide gate admission slots busy for {lock_wait}s — no verdict on {br}\ngate: this is host-wide gate concurrency (SPIRA_CERTIFY_PAR={par}), not a fault in the branch."));
                }
                w.sleep_ms(1000);
            }
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
        let t0 = w.now();
        loop {
            if w.tree_lock_try() {
                break;
            }
            if w.signalled() {
                return v(
                    NOVERDICT,
                    "died",
                    "gate: signalled while waiting for the gate tree",
                );
            }
            if w.now().saturating_sub(t0) >= lock_wait {
                self.s.waited = w.now() - t0;
                self.s.start = w.now();
                return v(NOVERDICT, "lock-timeout", format!(
                    "gate: another gate has held {} for {lock_wait}s — no verdict on {br}\ngate: this is a queue, not a fault in the branch; retry, or raise SPIRA_GATE_LOCK_WAIT.",
                    tree.display()));
            }
            w.sleep_ms(200);
        }
        self.s.held_lock = true;
        self.s.tree = Some(tree.clone());
        self.s.waited = w.now() - t0;
        self.s.start = w.now();
        if self.s.waited > 0 {
            w.eprint(&format!(
                "gate: waited {}s for {}",
                self.s.waited,
                tree.display()
            ));
        }
        w.write_holder(&PathBuf::from(format!("{}.lock.holder", tree.display())));

        let want = w
            .rev_parse(&repo, &format!("{rev}^{{commit}}"))
            .unwrap_or_default();
        if let Err(e) = self.gate_at(&repo, &tree, &rev, &want) {
            w.eprint(&e);
            return v(NOVERDICT, "tree-unidentified", "");
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
        let re = compose::reentry(&ejected, |s| w.exists(&tree.join("spira").join(s)));
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
        let jobs = compose::jobs(
            key::digits(&ctx.host_cores).unwrap_or(1),
            self.admission_par(&ctx),
        );

        let env = |branch: &str, repeat: &str, c: &Composition| -> Vec<(String, String)> {
            let e = |k: &str, v: &str| (k.to_string(), v.to_string());
            vec![
                // The launcher's PATH first (the release's bin/ and spira/, so a bare tool
                // name is the release's — sp-gypjk); cargo appended, for the tree builds.
                e(
                    "PATH",
                    &format!("{}:{}/.cargo/bin", ctx.var("PATH"), ctx.var("HOME")),
                ),
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
                e("SPIRA_GATE_HOST_CORES", &ctx.host_cores),
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
            ]
        };

        let (rc, out, ph) = run_composed(
            w,
            &tree,
            &comp,
            &with_bins(
                env(&label, ctx.var("SPIRA_VERDICT_REPEAT_CONSIDERED"), &comp),
                tree_def.as_ref(),
                &tree,
            ),
            &timeout,
            &cmd,
            tree_def.as_ref().and_then(|d| d.tools_command(jobs)).as_deref(),
            jobs,
            &re.required,
            "",
        );
        self.s.phases.extend(ph);
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
        if let Some((base_cmd, bdef)) = base_gate
            .as_ref()
            .filter(|_| self.gate_at(&repo, &tree, &base_rev, &base_rev).is_ok())
        {
            // A branch red before its suites step ran (a fence, the selector) is judged on the
            // base's fences only: the base's suites answer no question this red asks, and they
            // were most of every such base trial's wall (sp-govet: base-gate 164-501 s behind
            // a 12 s fence red).
            let base_comp = if !comp.suites_off() && !parse::suites_step_ran(&out) {
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
                    &tree,
                ),
                &timeout,
                base_cmd,
                bdef.as_ref().and_then(|d| d.tools_command(jobs)).as_deref(),
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

        // EACH RED IS JUDGED ON THE BASE ON THAT SUITE (sp-hh5h0). A branch red the base
        // trial did not run — its selection differed, or --deadline deferred it — is run on
        // the base now, by name, before anyone is blamed. A suite the base does not have is
        // the branch's own and needs no run.
        let mut absent: Vec<String> = Vec::new();
        if base_ran {
            let base_ran_set = parse::ran_suites(&base_out);
            let mut unrun: Vec<String> = Vec::new();
            for s in parse::red_suites(&out) {
                if base_ran_set.contains(&s) {
                    continue;
                }
                if w.ls_tree_has(&repo, &base_rev, &format!("spira/{s}")) {
                    unrun.push(s);
                } else {
                    absent.push(s);
                }
            }
            if !unrun.is_empty() {
                let rerun = base_rerun_cmd(&unrun);
                let t = w.now();
                let (r, o) = w.run_gate(
                    &tree,
                    &env(
                        &base_rev,
                        "base trial — the branch's red suites the base trial did not run",
                        &Composition::Suites {
                            why: "base-rerun".into(),
                        },
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
            }
        }
        let spaced = |v: &[String]| v.join(" ");
        match parse::attribute(&out, base_ran, base_rc, &base_out, &absent) {
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
            Attribution::BaseUntestable => v(NOVERDICT, "base-untestable", format!(
                "gate: {name}'s own gate failed: {cmd}\n{out}\ngate: and the same command could not be tried against {base}, so whose fault this is\ngate: cannot be established — refusing to charge it to the branch on a guess.")),
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
        let path = format!("{}:{}/.cargo/bin", ctx.var("PATH"), ctx.var("HOME"));
        self.w
            .cargo_metadata(tree, &path, ctx.var("HOME"))
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

    /// SPIRA_CERTIFY_PAR, else derived from the box, re-read on every pass of the wait.
    fn admission_par(&self, ctx: &Ctx) -> u64 {
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

    /// The one way out: meters, records, certifies and cleans up.
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
        }
        if let Some(f) = self.s.filelist.take() {
            w.remove(&f);
        }
        if let Some(t) = &self.s.tree {
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
                    meter_suffix(&self.s.compose, &self.s.phases)
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
                phases: meter_suffix(&self.s.compose, &self.s.phases)
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

/// The gate.log fields after the reason: ` compose=<label> phases=<name>:<secs>,…`, empty
/// until a composition was chosen. Readers split the note on spaces and take its first word
/// as the reason (yield.sh), so trailing fields are compatible.
pub fn meter_suffix(compose: &str, phases: &[(String, u64)]) -> String {
    if compose.is_empty() {
        return String::new();
    }
    let ph: Vec<String> = phases.iter().map(|(n, t)| format!("{n}:{t}")).collect();
    let ph = if ph.is_empty() {
        "-".to_string()
    } else {
        ph.join(",")
    };
    format!(" compose={compose} phases={ph}")
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
    format!(
        "{BASE_RERUN_MARK}_b=0; testenv --suites {} \"$SPIRA_GATE_BRANCH\" || _b=$?; case \"$_b\" in 2|3|127) exit 75;; *) exit \"$_b\";; esac",
        list.join(",")
    )
}

fn short(rev: &str) -> &str {
    rev.get(..12).unwrap_or(rev)
}

/// The environment with each `bin` of a tree definition pointing at the binary built from
/// `tree` (sp-quu2w), replacing the installed one the context named.
pub fn with_bins(
    mut env: Vec<(String, String)>,
    d: Option<&def::Def>,
    tree: &Path,
) -> Vec<(String, String)> {
    for b in d.map(|d| d.bins.as_slice()).unwrap_or(&[]) {
        let p = def::Def::bin_path(tree, b).to_string_lossy().into_owned();
        env.retain(|(k, _)| k != &b.var);
        env.push((b.var.clone(), p));
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
    tools: Option<&str>,
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
    if let Some(t) = tools {
        let t0 = w.now();
        let (rc, out) = w.run_gate(tree, env, &left(), t);
        phases.push((format!("{prefix}tools"), w.now().saturating_sub(t0)));
        if rc != 0 || w.signalled() {
            return (rc, format!("{out}\ngate: phase 'tools' failed (exit {rc}): {t}"), phases);
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
