//! The trial, in the order DESIGN.md "Order of the trial" gives. One way out: [`Trial::finish`].

use crate::cert;
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

    fn home(&self, f: &str) -> PathBuf {
        self.a.home.join(f)
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
        let range = format!("{base}...{br}");
        let files = match w.diff_names(&repo, &range) {
            Ok(f) => f,
            Err(out) => {
                return v(NOVERDICT, "no-diff", format!(
                    "gate: cannot diff {range} in {name} — one of them does not resolve in this checkout\n{out}"))
            }
        };
        let status_list = w.diff_name_status(&repo, &range);
        let ejected = self.ejected(&ctx);

        // THE MERGE (DESIGN.md "The merge"): what would land is what is judged.
        let merged_tree = match w.merge_tree(&repo, &base, &br) {
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
        let (rev, label) = if w.is_ancestor(&repo, &base, &br) {
            (br.clone(), br.clone())
        } else {
            match w.commit_merge(&repo, &merged_tree, &base, &br) {
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
        let exclude = self.home("exclude.sh");
        if !w.readable(&exclude) {
            return v(
                NOVERDICT,
                "missing-exclude",
                format!(
                    "gate: {} is missing — refusing to land unchecked",
                    exclude.display()
                ),
            );
        }
        let offenders = w.exclude_filter(&exclude, &w.ls_tree_all(&repo, &rev));
        let offenders = offenders.trim_end_matches('\n');
        if !offenders.is_empty() {
            let listed: Vec<String> = offenders.lines().map(|l| format!("gate:   {l}")).collect();
            return v(FAIL, "beads-data", format!(
                "gate: {br} would land beads data in the harness tree:\n{}\ngate: a beads database is never public and belongs in no shared repository.\ngate: remove them from the branch — there is no override for this one.",
                listed.join("\n")));
        }
        let skew = self.home("skew.sh");
        if !w.readable(&skew) {
            return v(
                NOVERDICT,
                "missing-skew",
                format!(
                    "gate: {} is missing — refusing to land unchecked",
                    skew.display()
                ),
            );
        }
        let (skew_rc, skew_out) = w.skew_foreign(&skew, &repo, &base, &br);
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

        // LAYER 2 — the repository's own gate.
        let cmd = ctx.gate_cmd.clone();
        if cmd.is_empty() {
            return v(PASS, "syntax-only", "");
        }
        for p in parse::bash_paths(&cmd) {
            if !w.ls_tree_has(&repo, &base, &p) {
                return v(NOVERDICT, "cmd-missing-file", format!(
                    "gate: {name}'s gate command names 'bash {p}' but {p} is absent from {base}.\ngate: command: {cmd}\ngate: this is a gate configuration error, not a fault in any branch.\ngate: land {p} on the base before wiring it into the gate command, or remove it."));
            }
        }

        // THE MODE (DESIGN.md "Composition"): typed, per repository, through spira-config.
        let mode = match w.gate_mode(&name) {
            Ok(m) => m,
            Err(e) => {
                w.eprint(&format!("gate: spira.toml does not validate ({e})\ngate: composing as gate_mode=suites, today's whole sequence — the stronger check."));
                GateMode::Suites
            }
        };
        // A unit-mode PASS is not a suites-mode PASS: the mode is part of what was judged.
        // Suites mode hashes the command alone, so today's keys are unchanged.
        let key_cmd = match mode {
            GateMode::Unit => format!("{cmd}\n# gate_mode=unit"),
            GateMode::Suites => cmd.clone(),
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

        // HOST-WIDE ADMISSION (fences-only certification takes no slot).
        if suites_mode != "off" {
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
        let sweep = self.home("gate-sweep.sh");
        if w.readable(&sweep) {
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
        let comp = self.composition(mode, &ctx, &repo, &base, &rev, &tree, &ejected);
        self.s.compose = comp.label();
        w.eprint(&describe(&comp));
        let jobs = compose::jobs(
            key::digits(&ctx.host_cores).unwrap_or(1),
            self.admission_par(&ctx),
        );

        let env = |branch: &str, repeat: &str, c: &Composition| -> Vec<(String, String)> {
            let e = |k: &str, v: &str| (k.to_string(), v.to_string());
            vec![
                e(
                    "PATH",
                    &format!("{}/.cargo/bin:{}", ctx.var("HOME"), ctx.var("PATH")),
                ),
                e("HOME", ctx.var("HOME")),
                e("TERM", "dumb"),
                e("SPIRA_GATE_REPO", &repo.to_string_lossy()),
                e("SPIRA_GATE_REPO_NAME", &name),
                e("SPIRA_GATE_BRANCH", branch),
                e("SPIRA_GATE_BASE", &base),
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
                // fences and build fence and selects nothing (gate-touched.sh). The always-
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
                e("SPIRA_LINT_BIN", ctx.var("SPIRA_LINT_BIN")),
                e("SPIRA_TESTENV_BIN", ctx.var("SPIRA_TESTENV_BIN")),
            ]
        };

        let (rc, out, ph) = run_composed(
            w,
            &tree,
            &comp,
            &env(&label, ctx.var("SPIRA_VERDICT_REPEAT_CONSIDERED"), &comp),
            &timeout,
            &cmd,
            jobs,
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
        if rc == 0 {
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
            return v(
                PASS,
                "pass",
                format!("gate: gate PASS covered suites: {pass_suites}"),
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
        if rc == NOVERDICT {
            return v(NOVERDICT, "harness-fault", format!(
                "gate: {name}'s own gate reported a harness fault (exit {NOVERDICT}) — container or install failed.\ngate: command: {cmd}\n{out}"));
        }

        // WHOSE FAULT: the same command on the landing ref itself.
        let mut base_ran = false;
        let (mut base_rc, mut base_out) = (1, String::new());
        let base_want = w
            .rev_parse(&repo, &format!("{base}^{{commit}}"))
            .unwrap_or_default();
        if self.gate_at(&repo, &tree, &base, &base_want).is_ok() {
            let base_comp = self.base_composition(&comp, &ctx, &tree);
            let (r, o, ph) = run_composed(
                w,
                &tree,
                &base_comp,
                &env(
                    &base,
                    "base trial — confirming whether base is independently red",
                    &base_comp,
                ),
                &timeout,
                &cmd,
                jobs,
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
        let spaced = |v: &[String]| v.join(" ");
        match parse::attribute(&out, base_ran, base_rc, &base_out) {
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
        ejected: &str,
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
            ejected,
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
        let path = format!("{}/.cargo/bin:{}", ctx.var("HOME"), ctx.var("PATH"));
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
            let y = self.home("yield.sh");
            if w.readable(&y) {
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
            "gate: composition=unit — fences (suites off), then cargo test on the host for: {} (touched: {})",
            crates.join(" "),
            touched.join(" ")
        ),
    }
}

/// Run a composition's phases in order, each under what is left of `timeout`, stopping at
/// the first non-zero status. Returns (status, the phases' output joined, the phase walls).
#[allow(clippy::too_many_arguments)]
pub fn run_composed<W: World>(
    w: &W,
    tree: &Path,
    comp: &Composition,
    env: &[(String, String)],
    timeout: &str,
    cmd: &str,
    jobs: u64,
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
    let first = if comp.suites_off() { "fences" } else { "gate" };
    let t = w.now();
    let (rc, mut out) = w.run_gate(tree, env, &left(), cmd);
    phases.push((format!("{prefix}{first}"), w.now().saturating_sub(t)));
    let Composition::Unit { crates, .. } = comp else {
        return (rc, out, phases);
    };
    if rc != 0 || w.signalled() {
        return (rc, out, phases);
    }
    for (name, c) in compose::unit_commands(crates, jobs) {
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
            out.push_str(&format!(
                "\ngate: unit phase '{name}' failed (exit {r}): {c}"
            ));
            return (r, out, phases);
        }
        if w.signalled() {
            return (r, out, phases);
        }
    }
    (0, out, phases)
}
