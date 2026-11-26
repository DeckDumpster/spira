//! The four phases (DESIGN.md "acceptance", Phases), in the script's order and with its
//! check names, so a log from either compares line for line.

use super::*;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// How long a kick waits for its task to fire before the stage that follows judges anyway.
const KICK_SECS: u64 = 120;

const CADENCE_TIMER: &str = "acceptance-cadence.timer";
const CADENCE_SERVICE: &str = "acceptance-cadence.service";

pub(super) type UnitSnap = Result<UnitTree, String>;

/// One place builds each kind of call, so no call site can be written without its
/// environment (the invariants the `acceptance-run` lint rule used to grep for).
impl Run<'_> {
    fn s(p: &Path) -> String {
        p.display().to_string()
    }

    /// The operator's answers for phase A's fresh install — what a human gives the
    /// installer on a new box (per Ryan 2026-10-07: install PRODUCES the config from
    /// user-provided inputs). `release install-tarball --answers` turns them into the one
    /// config file, every path pinned; nothing here writes config itself. `operated = 0` and
    /// the agent are this run's own: no operator is watching, and the agent is a stub.
    pub(super) fn answers_text(&self, releases: &Path) -> String {
        let db = &self.o.db;
        let data = db.parent().unwrap_or(Path::new("/"));
        let mut c = String::new();
        c.push_str("instance = prod\nid_prefix = sp\n");
        c.push_str(&format!("home_repo = {}\n", self.o.scratch_name()));
        c.push_str(&format!("home_repo_path = {}\n", Self::s(&self.o.a.scratch_repo)));
        c.push_str(&format!("db = {}\n", Self::s(db)));
        c.push_str(&format!("run = {}\n", Self::s(&self.o.spira_run)));
        c.push_str(&format!("releases = {}\n", Self::s(releases)));
        c.push_str(&format!("dolt_data = {}\n", Self::s(&data.join("dolt"))));
        c.push_str("operated = 0\n");
        c.push_str(&format!("release_repo = {}\n", Self::s(&self.o.release_src())));
        if let Some(a) = &self.o.a.agent {
            c.push_str(&format!("agent = {a}\n"));
        }
        c
    }

    /// The release under test's launcher environment — what a launcher gives every Spira
    /// process: `SPIRA_RELEASE` naming `current`, `PATH` with its `bin/` and `spira/` first
    /// (every tool is called by bare name, sp-gypjk), and `SPIRA_TOML`, the one source of config.
    fn launcher_env(&self) -> Vec<(String, String)> {
        let cur = self.o.releases().join("current");
        vec![
            ("SPIRA_TOML".into(), Self::s(&self.o.toml())),
            ("SPIRA_RELEASE".into(), Self::s(&cur)),
            ("PATH".into(), format!("{}:{}:{}", Self::s(&cur.join("bin")), Self::s(&cur.join("spira")), self.o.base_path)),
        ]
    }

    /// `deploy.sh`, `uninstall.sh`, `world.sh`, `doctor`: by bare name on the launcher env.
    pub(super) fn tool(&self, name: &str) -> Cmd {
        Cmd::new(name).envs(&self.launcher_env())
    }

    /// deploy.sh names its forge repository from SPIRA_FORGE_REPO or the checkout's git
    /// remote, and an installed release has no remote: hand it the owner/repo this run
    /// already resolved for its own gh calls, or every upgrade deploy exits 2 (sp-j0vhm).
    fn forge(&self, c: Cmd) -> Cmd {
        match &self.o.gh_repo {
            Some(r) => c.env("SPIRA_FORGE_REPO", r),
            None => c,
        }
    }

    /// `deploy.sh` re-renders units, so it needs the `SPIRA_HOME_REPO` the install had: without
    /// it the round-template units resolve out of the manifest and are pruned.
    fn deploy_sh(&self) -> Cmd {
        self.forge(self.tool("deploy.sh")).env("SPIRA_HOME_REPO", self.o.scratch_name())
    }

    /// `deploy.sh` of the release under test: always `--allow-draft` (acceptance runs before
    /// the draft is published), and the local tarball when this run was handed one.
    fn deploy_tag(&self) -> Cmd {
        let mut c = self.deploy_sh().arg("--allow-draft");
        if let Some(t) = &self.o.a.tarball {
            c = c.arg("--tarball").arg(Self::s(t));
        }
        c.arg(&self.o.a.tag)
    }

    /// `deploy.sh` of the predecessor (a rollback), with its local tarball when handed one.
    fn deploy_prev(&self, prev: &str, tarball: Option<&Path>) -> Cmd {
        let mut c = self.deploy_sh();
        if let Some(t) = tarball {
            c = c.arg("--tarball").arg(Self::s(t));
        }
        c.arg(prev)
    }

    /// `uninstall.sh --yes`, carrying the SAME `SPIRA_HOME_REPO` `install_env()` handed the
    /// install it is undoing. Without it, `owned.sh list` (which `uninstall.sh` asks what it
    /// owns) re-derives `repo_is_git_checkout` from scratch under bare `launcher_env()` —
    /// no home-repo override there, so it resolves a different answer than install saw — and
    /// the round-template units (optional on that flag, manifest.rs) installed under one answer
    /// go unlisted, hence unremoved, under the other.
    fn uninstall(&self) -> Cmd {
        self.tool("uninstall.sh").env("SPIRA_HOME_REPO", self.o.scratch_name()).arg("--yes")
    }

    /// One variable as the release under test's own conf.sh resolves it (DESIGN.md Decision 3).
    fn conf_value(&self, key: &str, extra: &[(&str, &str)]) -> Option<String> {
        let conf_sh = self.o.releases().join("current/spira/conf.sh");
        if !conf_sh.is_file() {
            return None;
        }
        let mut c = Cmd::new("bash")
            .arg("-c")
            .arg(format!(". \"$1\" 2>/dev/null; printf '%s' \"${{{key}:-}}\""))
            .arg("_")
            .arg(Self::s(&conf_sh))
            .envs(&self.launcher_env())
            .env("SPIRA_CONF_LOADED", "");
        for (k, v) in extra {
            c = c.env(*k, *v);
        }
        let out = self.h.run(&c);
        (out.rc == 0 && !out.out.is_empty()).then_some(out.out)
    }

    /// The install environment every phase's `install.sh`/`ready.sh` runs under: the launcher
    /// environment plus the install's own keys. One formula, so `SPIRA_OPERATED=0` cannot go
    /// missing from one phase.
    fn install_env(&self) -> Vec<(String, String)> {
        let mut e = self.launcher_env();
        e.extend([
            ("SPIRA_OPERATED".to_string(), "0".to_string()),
            ("SPIRA_RELEASES".into(), Self::s(&self.o.releases())),
            ("SPIRA_HOME_REPO".into(), self.o.scratch_name()),
        ]);
        if let Some(a) = &self.o.a.agent {
            e.push(("SPIRA_AGENT".into(), a.clone()));
        }
        e
    }

    /// `release install-tarball <tb>` by this binary (DESIGN.md Decision 1), with a run dir of
    /// its own as a sibling of the releases dir.
    ///
    /// `--skip-restart` (sp-r15cf): every call here is immediately followed by `install_sh()`
    /// below, which re-renders and restarts whatever unit's `Exec*` actually changed — the
    /// restart that matters, since `install-tarball`'s own restart happens before any
    /// re-templating and just bounces a unit back onto the release it was already running
    /// (release/DESIGN.md "Render"). Doing both raced a unit's own teardown on phase D's
    /// aged install, over real surviving state where units are already active, and failed
    /// the second restart deterministically enough under host load to make install.sh refuse.
    fn install_tarball(&self, tb: &Path) -> i32 {
        // A fresh box names no harness home and no release: install-tarball takes both from
        // the tarball it installs, exactly as it must for an operator's first install.
        let c = Cmd::new(Self::s(&self.o.release_bin))
            .arg("install-tarball")
            .arg("--skip-restart")
            .arg(Self::s(tb))
            .arg("--answers")
            .arg(Self::s(&self.o.answers()))
            .env("SPIRA_TOML", Self::s(&self.o.toml()))
            .env("SPIRA_RELEASES", Self::s(&self.o.releases()))
            .env("SPIRA_RUN", Self::s(&self.o.tmp.join("run")));
        self.h.show(&c)
    }

    /// `spira-install` (sp-31dm0: a compiled binary at `current/bin/spira-install` now,
    /// replacing the root `install.sh` script — no `bash` wrapper; named `spira-install`,
    /// not `install`, because `/usr/bin/install` is a system command).
    fn install_sh(&self) -> i32 {
        let c = Cmd::new(Self::s(&self.o.releases().join("current/bin/spira-install"))).arg("--skip-build").envs(&self.install_env());
        self.h.show(&c)
    }

    fn ready_sh(&self) -> (PathBuf, Out) {
        let p = self.o.releases().join("current/spira/ready.sh");
        let out = self.h.run(&Cmd::new("bash").arg(Self::s(&p)).envs(&self.install_env()));
        (p, out)
    }

    /// The bead store, through the installed release's content door.
    fn content(&self, args: &[&str]) -> Cmd {
        self.tool("spira-lc").arg("content").args(args.iter().copied())
    }

    fn git(&self, args: &[&str]) -> Cmd {
        Cmd::new("git").arg("-C").arg(Self::s(&self.o.a.scratch_repo)).args(args.iter().copied())
    }

    /// Bring the scratch checkout up to its origin before installing a predecessor. Phase A
    /// lands the probe and pushes to the scratch origin, so the checkout it leaves behind is
    /// behind, and a predecessor's install rightly refuses ("checkout is N commit(s) behind
    /// origin/main") — exactly as an operator's would until they pulled (sp-xp0u2: local
    /// acceptance 2026-10-05, phases B and D both failed on it once phase A's probe landed).
    fn sync_scratch(&mut self, label: &str) {
        let rc = self.h.run(&self.git(&["pull", "--ff-only", "--quiet"])).rc;
        self.is0(&format!("{label}: scratch checkout fast-forwarded to its origin"), rc);
    }

    fn systemctl(&self, args: &[&str]) -> Out {
        self.h.run(&Cmd::new("systemctl").arg("--user").args(args.iter().copied()))
    }

    fn systemctl_json(&self, args: &[&str]) -> Result<String, String> {
        let mut a = args.to_vec();
        a.push("--output=json");
        let o = self.systemctl(&a);
        if o.rc != 0 {
            return Err(format!("systemctl {} exited {}", args.join(" "), o.rc));
        }
        Ok(o.out)
    }

    /// The user manager's unit files matching the systemd glob `glob`.
    pub(super) fn unit_files(&self, glob: &str) -> Result<Vec<UnitFile>, String> {
        let o = self.systemctl(&["list-unit-files", glob, "--output=json"]);
        // list-unit-files exits 1 when nothing matches; that is an empty list, not a failure.
        if o.rc == 1 {
            return if o.out.is_empty() { Ok(vec![]) } else { unit_files_of(&o.out) };
        }
        if o.rc > 1 {
            return Err(format!("systemctl list-unit-files {glob} exited {}", o.rc));
        }
        unit_files_of(&o.out)
    }

    /// `(active, armed)` for `timer`, from `is-active` and the two next-elapse properties. A
    /// `show` that fails is an error, never "not armed".
    fn timer_state(&self, timer: &str) -> Result<(bool, bool), String> {
        let active = self.systemctl(&["is-active", timer]).rc == 0;
        let read = |prop: &str| {
            let o = self.systemctl(&["show", timer, "-p", prop, "--value"]);
            if o.rc == 0 {
                Ok(o.out)
            } else {
                Err(format!("systemctl show {timer} -p {prop} exited {}", o.rc))
            }
        };
        let realtime = read("NextElapseUSecRealtime")?;
        let monotonic = read("NextElapseUSecMonotonic")?;
        Ok((active, timer_armed(&realtime, &monotonic)))
    }

    /// Check `timer` is active with `armed` as wanted.
    fn expect_timer(&mut self, name: &str, timer: &str, armed: bool) {
        match self.timer_state(timer) {
            Err(e) => self.bad(name, &e),
            Ok((active, now)) => self.check(name, active && now == armed, || {
                format!(
                    "{timer} is {} and {}; wanted active and {}",
                    if active { "active" } else { "not active" },
                    if now { "armed" } else { "NOT armed" },
                    if armed { "armed" } else { "NOT armed" }
                )
            }),
        }
    }

    fn cadence_set(&self, timer: &str, interval: &str) -> Out {
        self.h.run(&self.tool("cadence.sh").args(["set", timer, interval]).env("SPIRA_SYSTEMD_USER_DIR", Self::s(&self.o.unit_dir())))
    }

    /// `cadence.sh set` never leaves a started timer without a next elapse, against the box's
    /// real `systemctl --user`: a stub cannot show what systemd reports as a next elapse.
    pub(super) fn cadence_never_disarms(&mut self) {
        println!("\nphase A — cadence.sh never disarms a started timer");
        self.cadence_steps();
        let dir = self.o.unit_dir();
        self.systemctl(&["stop", CADENCE_TIMER]);
        let ignore_missing = |r: std::io::Result<()>, what: &Path| match r {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {e}", what.display())),
            _ => Ok(()),
        };
        let gone = [CADENCE_SERVICE, CADENCE_TIMER]
            .iter()
            .try_for_each(|f| ignore_missing(fs::remove_file(dir.join(f)), &dir.join(f)))
            .and_then(|()| {
                let d = dir.join(format!("{CADENCE_TIMER}.d"));
                ignore_missing(fs::remove_dir_all(&d), &d)
            });
        self.systemctl(&["daemon-reload"]);
        match gone {
            Ok(()) => self.ok("phase A: cadence: fixture timer removed"),
            Err(e) => self.bad("phase A: cadence: fixture timer removed", &e),
        }
    }

    fn cadence_steps(&mut self) {
        let dir = self.o.unit_dir();
        let dropins = dir.join(format!("{CADENCE_TIMER}.d"));
        let t = CADENCE_TIMER;
        let written = fs::create_dir_all(&dir)
            .and_then(|()| fs::write(dir.join(CADENCE_SERVICE), "[Service]\nType=oneshot\nExecStart=/bin/true\n"))
            .and_then(|()| fs::write(dir.join(t), format!("[Timer]\nOnBootSec=9999d\nOnUnitActiveSec=1h\nUnit={CADENCE_SERVICE}\n")));
        if let Err(e) = written {
            self.bad("phase A: cadence: fixture timer written", &format!("{}: {e}", dir.display()));
            return;
        }
        let rc = self.systemctl(&["daemon-reload"]).rc;
        self.is0("phase A: cadence: daemon-reload exits 0", rc);
        let rc = self.systemctl(&["start", t]).rc;
        if rc != 0 {
            self.bad("phase A: cadence: fixture timer starts", &format!("systemctl start {t} exited {rc}"));
            return;
        }
        self.expect_timer("phase A: cadence: fixture timer is active and armed", t, true);

        let naive = dropins.join("naive.conf");
        let written = fs::create_dir_all(&dropins).and_then(|()| fs::write(&naive, "[Timer]\nOnUnitActiveSec=\nOnBootSec=\nOnUnitActiveSec=6h\n"));
        if let Err(e) = written {
            self.bad("phase A: cadence: naive drop-in written", &format!("{}: {e}", naive.display()));
            return;
        }
        self.systemctl(&["daemon-reload"]);
        self.systemctl(&["restart", t]);
        self.expect_timer("phase A: cadence: positive control: a naive drop-in disarms the timer", t, false);
        if let Err(e) = fs::remove_file(&naive) {
            self.bad("phase A: cadence: naive drop-in removed", &format!("{}: {e}", naive.display()));
            return;
        }
        self.systemctl(&["daemon-reload"]);
        self.systemctl(&["restart", t]);
        self.expect_timer("phase A: cadence: removing the naive drop-in arms the timer again", t, true);

        for iv in ["6h", "7h"] {
            let o = self.cadence_set(t, iv);
            self.check(&format!("phase A: cadence: cadence.sh set {iv} exits 0"), o.rc == 0, || format!("exit {}: {}", o.rc, tail(&o.text, 5)));
            self.expect_timer(&format!("phase A: cadence: {t} is active and armed after set {iv}"), t, true);
        }

        let conf = dropins.join("cadence.conf");
        let before = match fs::read_to_string(&conf) {
            Ok(b) => b,
            Err(e) => {
                self.bad("phase A: cadence: the 7h drop-in is readable", &format!("{}: {e}", conf.display()));
                return;
            }
        };
        let o = self.cadence_set(t, "2020-01-01 00:00:00");
        self.check("phase A: cadence: a never-elapsing set exits non-zero and says REFUSED", o.rc != 0 && o.text.contains("REFUSED"), || {
            format!("exit {}: {}", o.rc, tail(&o.text, 5))
        });
        self.expect_timer(&format!("phase A: cadence: {t} is active and armed after the refused set"), t, true);
        let after = fs::read_to_string(&conf).map_err(|e| format!("{}: {e}", conf.display()));
        self.check("phase A: cadence: the refused set restores the 7h drop-in", after.as_deref().map(str::trim_end) == Ok(before.trim_end()), || match &after {
            Ok(a) => format!("{} was\n{before}\nand is now\n{a}", conf.display()),
            Err(e) => e.clone(),
        });
    }

    /// The two unit-directory checks of phase A's install → uninstall round trip.
    pub(super) fn unit_dir_round_trip(&mut self, before: &UnitSnap, installed: &UnitSnap, after: &UnitSnap) {
        let dir = self.o.unit_dir();
        let name = "phase A: positive control: install changes the user unit directory";
        match (before, installed) {
            (Ok(b), Ok(i)) => self.check(name, b != i, || format!("install left {} unchanged, so the comparison below could not see a leftover", dir.display())),
            (Err(e), _) | (_, Err(e)) => self.bad(name, e),
        }
        let name = "phase A: user unit directory byte-identical after uninstall";
        match (before, after) {
            (Ok(b), Ok(a)) => {
                let d = unit_tree_diff(b, a);
                self.check(name, d.is_empty(), || {
                    let more = d.len().saturating_sub(20);
                    let tail = if more > 0 { format!("; and {more} more") } else { String::new() };
                    format!("{}: {}{tail}", dir.display(), d.iter().take(20).cloned().collect::<Vec<_>>().join("; "))
                })
            }
            (Err(e), _) | (_, Err(e)) => self.bad(name, e),
        }
    }

    /// The user manager's loaded units matching `glob`, in `state` when given.
    fn listed_units(&self, state: Option<&str>, glob: &str) -> Result<Vec<String>, String> {
        let flag = state.map(|s| format!("--state={s}"));
        let mut args = vec!["list-units"];
        args.extend(flag.as_deref());
        args.push(glob);
        unit_names_of(&self.systemctl_json(&args)?)
    }

    /// `scheduler status` of the installed release, through `world scheduler-status --json`.
    fn scheduler_status(&self) -> String {
        self.h.run(&self.tool("world").args(["scheduler-status", "--json"])).out
    }

    /// Make the loop take its next step. The loop is the scheduler's tasks: start the
    /// release's scheduler (a no-op when it runs) and wait, bounded, until `scheduler status`
    /// shows `task` fired since the kick. A predecessor that still ships the sentinel unit has
    /// that unit started, as it was. Best-effort either way: the stage that follows judges it.
    pub(super) fn kick(&self, task: &str) {
        if let Some(unit) = self.unit_files("spira-sentinel*.service").unwrap_or_default().into_iter().next() {
            self.systemctl(&["start", &unit.name]);
            return;
        }
        let before = fired_at(&self.scheduler_status(), task);
        let watchers = self.unit_files("spira-runtime*-watch*.service").unwrap_or_default();
        let units = self.unit_files("spira-runtime*.service").unwrap_or_default();
        match units.into_iter().find(|u| !watchers.iter().any(|w| w.name == u.name)) {
            Some(unit) => {
                self.systemctl(&["start", &unit.name]);
            }
            None => eprintln!("acceptance: no spira-runtime unit installed — the scheduler cannot be started"),
        }
        let (fired, t) = self.poll(KICK_SECS, 2, || {
            let now = fired_at(&self.scheduler_status(), task);
            now.is_some() && now != before
        });
        if !fired {
            eprintln!("acceptance: {task} did not fire within {t}s of the kick (scheduler status unchanged)");
        }
    }

    /// A predecessor installs the round-template refresh on any box with a git home repo, and
    /// its refresh needs a round VM host this box does not have: it fails on its first tick and
    /// the predecessor's own deploy.sh then refuses on the failed unit. Take the unit out of
    /// the box the way an operator without a VM host would.
    fn without_round_vm_host(&self) {
        for u in first_fields(&self.systemctl(&["list-unit-files", "spira-round-template*", "--no-legend", "--plain"]).out) {
            self.systemctl(&["disable", "--now", &u]);
            self.systemctl(&["reset-failed", &u]);
        }
    }

    fn gh(&self, args: &[&str]) -> Cmd {
        let mut c = Cmd::new("gh").args(args.iter().copied());
        if let Some(r) = &self.o.gh_repo {
            c = c.arg("--repo").arg(r);
        }
        c
    }

    /// Poll for a `spira-*.tar.gz` asset on `tag` until `timeout` (sp-5olmi: a tag-push run can
    /// start before release.yml's upload finishes).
    fn wait_for_asset(&self, tag: &str, timeout: u64) -> bool {
        let start = self.h.now();
        loop {
            let out = self.h.run(&self.gh(&["release", "view", tag, "--json", "assets"]));
            if out.rc == 0 && tarball_assets_of(&out.out).is_ok_and(|a| !a.is_empty()) {
                return true;
            }
            if self.h.now().saturating_sub(start) >= timeout {
                return false;
            }
            self.h.sleep(15);
        }
    }

    /// Download `tag`'s tarball into `dir`; its path.
    fn download(&self, tag: &str, dir: &Path) -> Option<PathBuf> {
        let _ = fs::create_dir_all(dir);
        let out = self.h.run(&self.gh(&["release", "download", tag, "--pattern", "spira-*.tar.gz", "--dir", &Self::s(dir)]));
        if out.rc != 0 {
            return None;
        }
        tarball_in(dir)
    }

    /// A given tarball is used as is (never a gh call); otherwise download it.
    fn acquire(&self, tag: &str, given: Option<&Path>, dir: &Path) -> Option<PathBuf> {
        match given {
            Some(p) => p.is_file().then(|| p.to_path_buf()),
            None => self.download(tag, dir),
        }
    }

    /// Add a release to the local release source skew.sh reads (`SPIRA_RELEASE_REPO` as a
    /// path): the tarball beside a `<stem>.tag` naming its tag.
    fn stage_release_source(&self, tb: &Path, tag: &str) {
        let dir = self.o.release_src();
        let stem = tb.file_name().map(|n| n.to_string_lossy().trim_end_matches(".tar.gz").to_string()).unwrap_or_default();
        let _ = fs::create_dir_all(&dir);
        if fs::copy(tb, dir.join(format!("{stem}.tar.gz"))).is_ok() {
            let _ = fs::write(dir.join(format!("{stem}.tag")), format!("{tag}\n"));
        }
    }

    /// Start every installed oneshot `spira-*` service once; none may fail.
    fn check_oneshots(&mut self, label: &str) {
        let mut failed = Vec::new();
        let files = match self.unit_files("spira-*.service") {
            Ok(f) => f,
            Err(e) => {
                self.bad(&format!("{label}: oneshots listed"), &e);
                return;
            }
        };
        for UnitFile { name: u, .. } in files {
            let ty = self.systemctl(&["show", "-p", "Type", "--value", &u]);
            if ServiceType::of(ty.rc, &ty.out) != ServiceType::Oneshot {
                continue;
            }
            if self.systemctl(&["start", &u]).rc != 0 {
                failed.push(u);
            }
        }
        let name = format!("{label}: every installed oneshot spira-* unit starts clean");
        self.check(&name, failed.is_empty(), || format!("failed: {}", failed.join(", ")));
    }

    fn unit_set(&self) -> Result<Vec<String>, String> {
        Ok(unit_set(&self.unit_files("spira-*")?))
    }

    /// The model's part is over: its lifecycle history has reached SUBMITTED (a model finishes
    /// with `work submit` and never closes the bead itself).
    fn bead_finished(&self, id: &str) -> bool {
        let hist = self.h.run(&self.tool("spira-lc").args(["history", id]));
        hist.rc == 0 && lifecycle_submitted(&lifecycle_states(&hist.out))
    }

    /// The dwarf ledger's closing line for `id` when the dwarf ran and ended without a branch.
    fn declined(&self, id: &str) -> Option<String> {
        let ledger = fs::read_to_string(self.o.spira_run.join("dwarf-ledger.log")).ok()?;
        let line = ledger.lines().rev().find(|l| l.contains(" done ") && l.contains(id))?;
        let status = line.split_whitespace().find_map(|w| w.strip_prefix("status="))?;
        Some(format!("builder declined: {status}"))
    }

    fn spira_units(&self, state: &str) -> Result<Vec<String>, String> {
        self.listed_units(Some(state), "spira-*")
    }

    /// Wait up to `budget` seconds for the spira units to leave activation or for one to fail.
    fn wait_units_settled(&self, budget: u64) {
        self.poll(budget, 2, || {
            let in_state = |s: &str| self.spira_units(s).map(|u| !u.is_empty());
            // A manager that cannot answer ends the wait; the check after it judges.
            in_state("failed").unwrap_or(true) || !(in_state("activating").unwrap_or(false) || in_state("reloading").unwrap_or(false))
        });
    }

    fn world_halted_line(&self) -> Option<String> {
        let out = self.h.run(&self.tool("world.sh").arg("status"));
        out.text.lines().find(|l| l.contains("HALTED") || l.contains("STOPPED")).map(str::to_string)
    }

    /// Wait until `ok()` or `budget` seconds pass, polling every `every`; the elapsed seconds.
    fn poll(&self, budget: u64, every: u64, mut ok: impl FnMut() -> bool) -> (bool, u64) {
        let t = self.h.now();
        while self.h.now().saturating_sub(t) < budget {
            if ok() {
                return (true, self.h.now().saturating_sub(t));
            }
            self.h.sleep(every);
        }
        (false, self.h.now().saturating_sub(t))
    }

    /// File a probe bead labelled for the builder predicate; its id.
    pub(super) fn file_probe(&self, title: &str, description: &str, labels: &Labels) -> (Option<String>, String) {
        let l = format!("acceptance,{},{},repo:{}", labels.plan, labels.scope, self.o.scratch_name());
        let create = self.content(&["create", "--title", title, "--label", &l, "--type", "task", "--body-file", "-"]).stdin(description);
        let out = self.h.run(&create);
        let Some(id) = extract_bead_id(&out.text) else { return (None, out.text) };
        // `content create` only reports a row it could not write, and a rowless bead is never
        // claimable: a row that cannot be created fails "bead filed", naming why.
        // The store answers late right after an install; create-bead is idempotent, so ask until it does.
        let mut lc = self.h.run(&self.tool("spira-lc").args(["create-bead", &id]));
        self.poll(30, 2, || {
            if lc.rc == 0 {
                return true;
            }
            lc = self.h.run(&self.tool("spira-lc").args(["create-bead", &id]));
            lc.rc == 0
        });
        if lc.rc != 0 {
            return (None, format!("{id} was created, but spira-lc create-bead rc={}: {}\n{}", lc.rc, lc.text.trim_end(), out.text));
        }
        (Some(id), out.text)
    }

    /// Whether a builder takes `id`: the claim view offers it to one, or one has already
    /// claimed it, which takes it out of the view.
    pub(super) fn claimable_by_builder(&self, id: &str) -> bool {
        self.poll(30, 2, || {
            let view = self.h.run(&self.tool("spira-lc").arg("claimable"));
            if view.rc == 0 && builder_would_take(&view.out, id) {
                return true;
            }
            let hist = self.h.run(&self.tool("spira-lc").args(["history", id]));
            hist.rc == 0 && lifecycle_states(&hist.out).iter().any(|s| spira_store::lc_state::is_working(s))
        })
        .0
    }

    /// Stages 2-5 for a filed bead: called (branch), committed, closed, landed by ancestry
    /// on `origin/<land_ref>` past `base`.
    fn follow(&mut self, p: &str, id: &str, land_ref: &str, base: &str, landed_name: &str) {
        self.kick("sentinel-call");
        let branch = format!("refs/heads/spira/{id}");
        let (ok, t) = self.poll(self.o.call_secs, 2, || self.h.run(&self.git(&["show-ref", "--verify", "-q", &branch])).rc == 0);
        if ok {
            self.ok(&format!("{p} stage 2: dwarf called for {id} — branch created ({t}s)"));
        } else {
            let declined = self.declined(id).map(|d| format!("; {d}")).unwrap_or_default();
            self.bad(&format!("{p} stage 2: dwarf called for {id}"), &format!("branch spira/{id} not created after {t}s{declined}"));
        }

        let br = format!("spira/{id}");
        let (ok, t) = self.poll(60, 2, || self.h.run(&self.git(&["log", "--oneline", &br])).out.contains(id));
        let (okn, badn) = if p == "phase A" {
            (format!("{p} stage 3: commit on spira/{id} for {id} ({t}s)"), format!("{p} stage 3: commit on spira/{id} for {id}"))
        } else {
            (format!("{p} stage 3: commit on spira/{id} ({t}s)"), format!("{p} stage 3: commit on spira/{id}"))
        };
        if ok {
            self.ok(&okn);
        } else {
            self.bad(&badn, &format!("no commit naming bead id on branch after {t}s"));
        }

        let (ok, t) = self.poll(30, 2, || self.bead_finished(id));
        if ok {
            self.ok(&format!("{p} stage 4: bead {id} closed ({t}s)"));
        } else {
            self.bad(&format!("{p} stage 4: bead {id} closed"), &format!("not closed after {t}s"));
        }

        // Kick the loop: the landing task dispatches the landing pass on the closed branch.
        self.kick("landing");
        let origin = format!("origin/{land_ref}");
        let (ok, t) = self.poll(120, 5, || {
            self.h.run(&self.git(&["fetch", "origin"]));
            let r = self.h.run(&self.git(&["rev-parse", &origin]));
            let now = if r.rc == 0 { r.out.trim().to_string() } else { base.to_string() };
            !base.is_empty() && now != base && self.h.run(&self.git(&["log", "--format=%s", &format!("{base}..{now}")])).out.contains(id)
        });
        if ok {
            self.ok(&format!("{landed_name} ({t}s)"));
        } else {
            let reason = if p == "phase A" { format!("no commit with bead id on origin/{land_ref} after {t}s") } else { format!("no commit with {id} on origin/{land_ref} after {t}s") };
            self.bad(landed_name, &reason);
        }
    }

    /// A stub builder's handoff through the release's own fast tier (`dwarf fast-tier`, the
    /// code the dwarf's teardown runs before it hands a branch on). The tier only exists in a
    /// harness tree, so the scratch repo cannot host it — a tree carrying `gate.steps` takes
    /// over that repository's landing gate — and the release's own tree, minus what a release
    /// adds, is committed as the base of a temporary repository instead.
    fn fast_tier_handoff(&mut self, label: &str) {
        let cur = self.o.releases().join("current");
        let dir = self.o.tmp.join(format!("fast-tier-{}", label.replace(' ', "-")));
        let (cur_s, dir_s) = (Self::s(&cur), Self::s(&dir));
        let seed = self.h.run(
            &Cmd::new("bash")
                .arg("-c")
                .arg(FAST_TIER_SEED)
                .arg("_")
                .arg(&cur_s)
                .arg(&dir_s)
                .env("GIT_AUTHOR_NAME", "Spira Acceptance")
                .env("GIT_AUTHOR_EMAIL", "acceptance@spira.local")
                .env("GIT_COMMITTER_NAME", "Spira Acceptance")
                .env("GIT_COMMITTER_EMAIL", "acceptance@spira.local"),
        );
        let name = format!("{label}: stub builder's probe committed on a branch of the release's own tree");
        self.check(&name, seed.rc == 0, || format!("exit {}\n{}", seed.rc, tail(&seed.text, 8)));
        if seed.rc != 0 {
            return;
        }
        // The check must be able to fail: without a base, spira-lint's diff rules refuse.
        let bare = self.h.run(&self.tool("bash").arg("-c").arg(FAST_TIER_BARE_LINT).arg("_").arg(&dir_s));
        let name = format!("{label}: positive control: spira-lint with no base refuses");
        self.check(&name, bare.rc != 0, || format!("exit 0 — a bare spira-lint passed:\n{}", tail(&bare.text, 8)));
        let c = self.tool("dwarf").args(["fast-tier", &dir_s, &dir_s, FAST_TIER_BRANCH, "main"]);
        let out = self.h.run(&c);
        let name = format!("{label}: stub builder's handoff passes the fast tier (rebase check, spira-lint against the base, build fence)");
        self.check(&name, out.rc == 0, || format!("exit {}\n{}", out.rc, tail(&out.text, 12)));
    }

    fn rev(&self, r: &str) -> Option<String> {
        let o = self.h.run(&self.git(&["rev-parse", r]));
        (o.rc == 0).then(|| o.out.trim().to_string()).filter(|s| !s.is_empty())
    }

    fn count_beads(&self) -> Option<usize> {
        let o = self.h.run(&self.content(&["list", "--all", "--limit", "0", "--json"]));
        if o.rc != 0 {
            return None;
        }
        listed_json(&o.out).map(|v| v.len())
    }

    fn count_memories(&self) -> Option<usize> {
        let o = self.h.run(&self.content(&["memories", "--json"]));
        if o.rc != 0 {
            return None;
        }
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&o.out).ok().map(|m| m.len())
    }
}

/// A probe bead's description, shaped like real builder work: the filing contract refuses a builder bead
/// with no `## Build` section, and so does the builder.
fn probe_body(why: &str) -> String {
    format!("{why}\n\n## Build\n\nCreate acceptance-probe-<bead-id>.txt at the root of this repository containing the bead id, commit it on the bead's branch, and submit.\n")
}

const FAST_TIER_BRANCH: &str = "spira/acceptance-fast-tier";

/// `$1` the release, `$2` the new repository: the release's tree without `bin/`, `model-bin/`,
/// MANIFEST or the compat links into `bin/` (all added by a release build, none tracked) is the
/// base on `main`; the stub builder's probe is one commit on its branch.
const FAST_TIER_SEED: &str = r#"set -e
rel="$1"; dir="$2"
rm -rf "$dir"; mkdir -p "$dir"
git init -q -b main "$dir"
(cd "$rel" && find . \( -path ./bin -o -path ./model-bin -o -path ./MANIFEST \) -prune -o ! \( -type l -lname '../bin/*' \) -print0 \
    | tar --null --no-recursion -T - -cf -) | tar --no-same-permissions -xf - -C "$dir"
chmod -R u+w "$dir"
git -C "$dir" add -A
git -C "$dir" commit -q -m "base: the release's own tree"
git -C "$dir" checkout -q -b spira/acceptance-fast-tier
printf 'acceptance-fast-tier\n' > "$dir/acceptance-probe-fast-tier.txt"
git -C "$dir" add acceptance-probe-fast-tier.txt
git -C "$dir" commit -q -m "acceptance-fast-tier: acceptance probe"
"#;

const FAST_TIER_BARE_LINT: &str = r#"cd "$1" && exec env -u SPIRA_GATE_BASE spira-lint --only plan-matrix"#;

/// The builder partition labels the probe bead must carry.
pub(super) struct Labels {
    pub(super) plan: String,
    pub(super) scope: String,
}

fn tarball_in(dir: &Path) -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().map(|n| n.to_string_lossy()).is_some_and(|n| n.starts_with("spira-") && n.ends_with(".tar.gz")))
        .collect();
    v.sort();
    v.into_iter().next()
}

fn tail(text: &str, n: usize) -> String {
    let l: Vec<&str> = text.lines().collect();
    l[l.len().saturating_sub(n)..].join("\n")
}

/// The commit a tag points at, from the notes repository; None when it cannot be resolved.
fn tag_commit(h: &dyn Host, repo: Option<&Path>, tag: &str) -> Option<String> {
    let repo = repo?.display().to_string();
    let o = h.run(&Cmd::new("git").args(["-C", &repo, "rev-parse", &format!("{tag}^{{commit}}")]));
    let c = o.out.trim().to_string();
    (o.rc == 0 && !c.is_empty()).then_some(c)
}

/// The nearest earlier tag with an acceptance note, when that note records an upgrade waiver:
/// a waiver covers its own cut only.
fn waived_predecessor(h: &dyn Host, repo: Option<&Path>, tag: &str) -> Option<String> {
    let repo = repo?.display().to_string();
    let listed = h.run(&Cmd::new("git").args(["-C", &repo, "tag", "--list", "--sort=-creatordate"]));
    if listed.rc != 0 {
        return None;
    }
    for t in listed.out.lines().map(str::trim).skip_while(|t| *t != tag).skip(1) {
        let note = h.run(&Cmd::new("git").args(["-C", &repo, "notes", "--ref=acceptance", "show", &format!("refs/tags/{t}")]));
        if note.rc == 0 && !note.out.trim().is_empty() {
            return note.out.lines().any(|l| l == WAIVER_LINE).then(|| t.to_string());
        }
    }
    None
}

/// PASS means every phase ran: a run that skipped B/C/D without a waiver is PARTIAL.
pub(super) fn verdict_for(fail: u32, has_prev: bool, waived: bool) -> &'static str {
    match (fail, has_prev || waived) {
        (0, true) => "PASS",
        (0, false) => "PARTIAL",
        _ => "FAIL",
    }
}

/// The whole run; the exit code.
pub fn run(h: &dyn Host, o: Opts) -> u8 {
    let mut r = Run::new(h, o);
    let _ = fs::create_dir_all(&r.o.forensics);
    let _ = fs::write(r.o.forensics.join("checks.jsonl"), "");
    let tag = r.o.a.tag.clone();
    let prev = r.o.a.prev_tag.clone();
    println!("release acceptance  tag={tag}");
    println!("forensics:          {}", r.o.forensics.display());

    // ---- prerequisites --------------------------------------------------------------------
    println!("\nprerequisites — real tools on PATH");
    let ghost = format!("spira-acceptance-nonexistent-{}", r.o.pid);
    if h.on_path(&ghost) {
        r.bad("positive-control: command -v catches missing tool", "command -v returned 0 for a nonexistent name");
    } else {
        r.ok("positive-control: command -v catches missing tool");
    }
    for t in ["dolt", "git", "gh", "python3"] {
        let name = format!("prereq: {t} on PATH");
        r.check(&name, h.on_path(t), || "not found; install before running acceptance".into());
    }
    let mgr = r.systemctl(&["is-system-running"]);
    r.check("prereq: systemd --user available", !mgr.out.trim().is_empty(), || {
        "systemctl --user is-system-running produced no output; check XDG_RUNTIME_DIR and DBUS_SESSION_BUS_ADDRESS".into()
    });
    let scratch = r.o.a.scratch_repo.clone();
    if scratch.join(".git").is_dir() || h.run(&r.git(&["rev-parse", "--git-dir"])).rc == 0 {
        r.ok(&format!("prereq: scratch-repo is a git repository ({})", scratch.display()));
    } else {
        r.bad("prereq: scratch-repo is a git repository", &format!("{} is not a git repo", scratch.display()));
    }
    if r.fail > 0 {
        println!("\n{} passed, {} failed", r.pass, r.fail);
        eprintln!("release acceptance: prerequisite failures — cannot continue");
        return 2;
    }

    if prev.is_none() {
        if let Some(w) = waived_predecessor(h, r.o.notes_repo.as_deref(), &tag) {
            // A repeat waiver is allowed only when the operator declared it for this cut's exact
            // commit (law-waive-upgrade-once-when-the-fix-cannot-reach-the-predecessor, amended
            // "keep waiving until you get a release"): the predecessor can still be one whose own
            // install cannot pass the upgrade phases. Anything else is refused as before.
            if r.o.a.standing_waiver {
                println!("release acceptance: {w} was a waived cut; the standing ruling declares the waiver for this cut too");
            } else {
                let declared = r.o.a.declared_waiver.clone().unwrap_or_default();
                let commit = tag_commit(h, r.o.notes_repo.as_deref(), &tag);
                if declared.is_empty() || commit.as_deref() != Some(declared.as_str()) {
                    eprintln!("release acceptance: refusing {tag}: {w} carries an upgrade waiver, so this cut must run phases B/C/D (--prev-tag, no --waive-upgrade), unless the standing ruling is in force (--standing-waiver) or a waiver is declared for this cut's exact commit (--declared-waiver <sha>)");
                    return 2;
                }
                println!("release acceptance: {w} was a waived cut; honouring the repeat waiver declared for {declared}");
            }
        }
    }

    // ---- phase A --------------------------------------------------------------------------
    r.phase("phase-A");
    println!("\nphase A — positive control: binary check catches missing binary");
    let pc = r.o.tmp.join("pc-release");
    let _ = fs::create_dir_all(pc.join("bin"));
    for b in ["loom", "panel", "broker"] {
        let p = pc.join("bin").join(b);
        let _ = fs::write(&p, "#!/bin/sh\n");
        let _ = fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755));
    }
    let got = missing_release_bins(&pc);
    let empty = if got.is_empty() { "empty".to_string() } else { got.clone() };
    r.check("positive-control: binary check names missing binary (spira-supervise)", got.contains("spira-supervise"), || format!("got: [{empty}]"));

    println!("\nphase A — fresh install from {tag} tarball");
    let releases = r.o.releases();
    let _ = fs::create_dir_all(&releases);
    let _ = fs::create_dir_all(r.o.release_src());
    let answers = r.o.answers();
    if let Err(e) = fs::write(&answers, r.answers_text(&releases)) {
        r.bad("phase A: operator answers written", &format!("{}: {e}", answers.display()));
    }

    let tarball = match r.o.a.tarball.clone() {
        Some(given) => {
            let t = r.acquire(&tag, Some(&given), Path::new(""));
            match &t {
                Some(p) => r.ok(&format!("phase A: tarball provided via --tarball (download skipped): {}", p.file_name().unwrap_or_default().to_string_lossy())),
                None => r.bad("phase A: tarball provided via --tarball", &format!("not found: {}", given.display())),
            }
            t
        }
        None => {
            let wait = r.o.asset_wait_secs;
            if r.wait_for_asset(&tag, wait) {
                r.ok(&format!("phase A: release asset appears for {tag}"));
            } else {
                r.bad(&format!("phase A: release asset appears for {tag}"), &format!("no spira-*.tar.gz asset within {wait}s — release.yml's upload never appeared"));
            }
            let dir = r.o.tmp.join("tarball-dl");
            let t = r.acquire(&tag, None, &dir);
            r.is0(&format!("phase A: gh release download {tag}"), if t.is_some() { 0 } else { 1 });
            match &t {
                Some(p) => r.ok(&format!("phase A: tarball found: {}", p.file_name().unwrap_or_default().to_string_lossy())),
                None => r.bad("phase A: tarball found", &format!("no spira-*.tar.gz in {}", dir.display())),
            }
            t
        }
    };
    let mut sha = None;
    if let Some(tb) = &tarball {
        r.stage_release_source(tb, &tag);
        sha = crate::fsutil::sha256_file(tb).ok();
    }
    let unit_dir_before = unit_tree(&r.o.unit_dir());
    let rc = match &tarball {
        Some(tb) => r.install_tarball(tb),
        // No tarball is not an install that succeeded (the script's rc stayed 0 here).
        None => 1,
    };
    r.is0("phase A: release install-tarball exits 0", rc);
    if releases.join("current").is_dir() {
        let m = missing_release_bins(&releases.join("current"));
        r.check("phase A: all native binaries present and executable", m.is_empty(), || format!("missing: {m}"));
        // sp-zf4q3: the model's PATH names model-bin/ only; a tarball without it would
        // leave every dwarf refusing its session.
        let mb = spira_config::release_env::model_bin_problems(&releases.join("current"));
        r.check("phase A: model-bin/ holds only MODEL_BINS", mb.is_empty(), || mb.join("; "));
    }
    let install_rc = if releases.join("current").is_dir() { r.install_sh() } else { 1 };
    r.is0("phase A: install.sh exits 0", install_rc);
    let unit_dir_installed = unit_tree(&r.o.unit_dir());

    // The builder's label keys, read whenever the release conf exists — not only after a
    // clean install: a failed install must not also mislabel the probe (sp-bn9go). The scope
    // is read as the installed services resolve it (identity from MANIFEST, no HOME_REPO).
    let home_repo = r.o.scratch_name();
    let labels = Labels {
        plan: r.conf_value("SPIRA_PLAN_LABEL", &[("SPIRA_HOME_REPO", &home_repo)]).unwrap_or_else(|| "plan".into()),
        scope: r.conf_value("SPIRA_SCOPE_LABEL", &[]).unwrap_or_else(|| home_repo.clone()),
    };

    let land_modes: Vec<(String, String)> = h
        .run(&r.tool("spira-config").args(["repo", "names"]))
        .out
        .lines()
        .map(|n| (n.trim().to_string(), h.run(&r.tool("spira-config").args(["repo", "land", n.trim()])).out.trim().to_string()))
        .collect();
    let missing = land_modes_missing(&land_modes.iter().map(|(_, m)| m.clone()).collect::<Vec<_>>());
    r.check("phase A: scratch setup carries a repository in each land mode (queue, pr, push)", missing.is_empty(), || format!("no configured repository with land mode: {}", missing.join(", ")));

    let (ready_path, ready) = r.ready_sh();
    r.check("phase A: ready.sh exits 0 after install", ready.rc == 0, || format!("{} exit {}", ready_path.display(), ready.rc));
    if ready.rc != 0 {
        println!("{}", ready.text);
    }

    println!("\nphase A — end-to-end bead: file, sentinel calls dwarf, land by ancestry");
    let land_ref = h.run(&r.git(&["rev-parse", "--abbrev-ref", "HEAD"])).out.trim().to_string();
    let land_ref = if land_ref.is_empty() { "main".to_string() } else { land_ref };
    let base = r.rev(&format!("origin/{land_ref}")).or_else(|| r.rev("HEAD")).unwrap_or_default();
    let (id, out) = r.file_probe(
        &format!("acceptance-run: trivial land proof for {tag}"),
        &probe_body(&format!("Acceptance test: prove end-to-end landing works. The tag under test is {tag}.")),
        &labels,
    );
    match &id {
        Some(i) => r.ok(&format!("phase A: bead filed ({i})")),
        None => r.bad("phase A: bead filed", out.trim_end()),
    }
    if let Some(id) = id {
        r.probe = Some(id.clone());
        // Claimability is the discriminating question: a bead the sentinel can see but no
        // predicate matches is indistinguishable from a silent sentinel.
        let sel = format!("{},{}", labels.scope, labels.plan);
        let claimable = r.claimable_by_builder(&id);
        let name = format!("phase A: bead is claimable by builder predicate ({sel})");
        r.check(&name, claimable, || format!("bead {id}: `spira-lc claimable` offers it to no builder and none has claimed it — builder predicate does not match bead labels"));
        let landed = format!("phase A stage 5: bead {id} landed on {}:{land_ref}", scratch.display());
        r.follow("phase A", &id, &land_ref, &base, &landed);
        let mode = land_modes.iter().find(|(n, _)| *n == home_repo).map(|(_, m)| m.clone()).unwrap_or_else(|| "push".into());
        let want = expected_lifecycle(&mode);
        // LANDED is recorded by the hel sweep, one pass
        // after the push — never at the instant stage 5 sees the commit. Read history until it
        // holds the whole sequence or the bound runs out; the last read is what is judged
        // (sp-53own: a single immediate read saw [.., CERTIFIED], missing LANDED).
        let mut hist = h.run(&r.tool("spira-lc").args(["history", &id]));
        let _ = r.poll(120, 5, || {
            hist = h.run(&r.tool("spira-lc").args(["history", &id]));
            hist.rc == 0 && missing_in_order(&want, &lifecycle_states(&hist.out)).is_none()
        });
        let got = lifecycle_states(&hist.out);
        let name = format!("phase A: {id} lifecycle event sequence {} ({mode})", want.join(" -> "));
        r.check(&name, hist.rc == 0 && missing_in_order(&want, &got).is_none(), || {
            format!("spira-lc history {id} rc={} — saw [{}], missing {}", hist.rc, got.join(", "), missing_in_order(&want, &got).unwrap_or("history"))
        });
    }

    if releases.join("current").is_dir() {
        println!("\nphase A — a builder's handoff passes the fast tier");
        r.fast_tier_handoff("phase A");
    }

    println!("\nphase A — no harness tool failed to exec");
    let exec = super::exec_failures(&r.o.spira_run);
    r.check("phase A: no run/ log records an exec failure (os error 2, exit 127)", exec.is_empty(), || exec.iter().take(20).cloned().collect::<Vec<_>>().join("\n"));

    if releases.join("current").is_dir() {
        r.cadence_never_disarms();
    }

    println!("\nphase A — uninstall and clean state");
    let rc = h.run(&r.uninstall()).rc;
    r.is0("phase A: uninstall.sh --yes exits 0", rc);
    match r.unit_files("spira-*") {
        Ok(left) => r.check("phase A: no spira-* units remain after uninstall", left.is_empty(), || left.iter().map(|f| f.name.as_str()).collect::<Vec<_>>().join("\n")),
        Err(e) => r.bad("phase A: no spira-* units remain after uninstall", &e),
    }
    let unit_dir_after = unit_tree(&r.o.unit_dir());
    r.unit_dir_round_trip(&unit_dir_before, &unit_dir_installed, &unit_dir_after);

    // ---- phase B / C ----------------------------------------------------------------------
    r.phase("phase-B");
    let p = prev.clone().unwrap_or_default();
    println!("\nphase B — upgrade: install {p}, deploy to {tag}, verify no rollback");
    // The predecessor's archive.sh dies without ~/.claude/projects (fixed forward); ensure it
    // so phase C's oneshot check exercises the predecessor's unit, not an already-fixed bug.
    let _ = fs::create_dir_all(&r.o.token_projects);
    let prev_dir = r.o.tmp.join("prev-tarball-dl");
    let mut prev_tb: Option<PathBuf> = None;
    match &prev {
        None => println!("  skip  phase B+C: --prev-tag not given"),
        Some(pt) => {
            let given = r.o.a.prev_tarball.clone();
            prev_tb = r.acquire(pt, given.as_deref(), &prev_dir);
            match prev_tb.clone() {
                None => r.bad(&format!("phase B: gh release download {pt}"), "rc=1"),
                Some(tb) => {
                    r.ok(&format!("phase B: prev tarball downloaded: {}", tb.file_name().unwrap_or_default().to_string_lossy()));
                    r.stage_release_source(&tb, pt);
                    r.install_tarball(&tb);
                    r.sync_scratch("phase B");
                    let prc = r.install_sh();
                    r.without_round_vm_host();
                    r.is0(&format!("phase B: install.sh ({pt}) exits 0"), prc);
                    // A failed install leaves the database down; deploy.sh would then read as an
                    // upgrade failure. Attribute it to the install instead.
                    if prc != 0 {
                        r.bad("phase B+C: skipped — install failed; database service not started", &format!("prev_install_rc={prc}"));
                    } else {
                        phase_bc(&mut r, &tag, pt);
                    }
                }
            }
        }
    }

    // ---- phase D --------------------------------------------------------------------------
    r.phase("phase-D");
    println!("\nphase D — aged-install upgrade: {p} with real state → {tag}");
    match &prev {
        None => println!("  skip  phase D: --prev-tag not given"),
        Some(pt) => phase_d(&mut r, &tag, pt, prev_tb, &prev_dir, &labels, &land_ref),
    }

    // ---- verdict --------------------------------------------------------------------------
    r.snapshot("end-of-run");
    println!("\n{} passed, {} failed", r.pass, r.fail);
    let verdict = verdict_for(r.fail, prev.is_some(), r.o.a.waive_upgrade);
    let date = crate::fsutil::rfc3339(h.now());
    println!("verdict: {verdict}  tag={tag}  date={date}");

    if r.o.a.record {
        let name = tarball.as_ref().and_then(|t| t.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "unknown".into());
        let note = Note { verdict, date: &date, tag: &tag, pass: r.pass, fail: r.fail, tarball: sha.as_deref().map(|s| (s, name.as_str())), prev_tag: prev.as_deref(), waived: r.o.a.waive_upgrade, standing: r.o.a.standing_waiver }.text();
        let repo = r.o.notes_repo.clone().unwrap_or_default();
        let c = Cmd::new("git").args(["-C", &repo.display().to_string(), "notes", "--ref=acceptance", "add", "-f", "-m", &note, &format!("refs/tags/{tag}")]);
        if h.run(&c).rc == 0 {
            println!("recorded: git notes --ref=acceptance show refs/tags/{tag}");
        } else {
            println!("warning: could not write git note (verdict still printed above)");
        }
    }
    if r.o.a.file_defects && r.fail > 0 {
        let title = format!("acceptance-run {tag}: {} phase(s) failed", r.fail);
        let desc = format!("release acceptance found {} failure(s) against tag {tag}. See the acceptance log for detail.", r.fail);
        let create = r.content(&["create", "--title", &title, "--label", "defect,repo:spira,discovered-from:sp-ewwwq", "--type", "bug", "--body-file", "-"]).stdin(desc);
        let filed = h.run(&create);
        if filed.rc != 0 {
            println!("warning: the defect was not filed (exit {}): {}", filed.rc, tail(&filed.text, 3));
        }
    }
    println!("forensics: {}", r.o.forensics.display());
    if r.fail == 0 {
        0
    } else {
        1
    }
}

fn phase_bc(r: &mut Run, tag: &str, pt: &str) {
    let h = r.h;
    let pre = match r.unit_set() {
        Ok(p) => p,
        Err(e) => {
            r.bad("phase B: unit set snapshot before the upgrade", &e);
            return;
        }
    };
    let rc = h.show(&r.deploy_tag());
    r.is0(&format!("phase B: deploy.sh {tag} exits 0 (no rollback)"), rc);
    r.check_oneshots("phase B");

    // deploy.sh writes the full tag to .tags/<release> beside the release dirs. Read
    // SPIRA_RELEASES from the installed conf, as deploy.sh did — never a guessed default.
    let rel_dir = r.conf_value("SPIRA_RELEASES", &[]);
    let name = format!("phase B: .tag sidecar names {tag}");
    match &rel_dir {
        None => r.bad(&name, "SPIRA_RELEASES unreadable from the release's conf.sh"),
        Some(d) => {
            let cur = fs::read_link(Path::new(d).join("current")).map(|p| p.display().to_string()).unwrap_or_default();
            let got = fs::read_to_string(Path::new(d).join(".tags").join(&cur)).unwrap_or_default();
            r.is_same(&name, tag, got.trim_end_matches('\n'));
        }
    }
    let prod = r.conf_value("SPIRA_PROD", &[]).unwrap_or_default();
    match &rel_dir {
        None => r.bad("phase B: SPIRA_PROD updated to releases path", "SPIRA_RELEASES unreadable from the release's conf.sh"),
        Some(d) => r.want("phase B: SPIRA_PROD updated to releases path", d, &prod),
    }

    r.phase("phase-C");
    println!("\nphase C — rollback: deploy {pt}, verify unit set restored");
    let rc = h.show(&r.deploy_prev(pt, r.o.a.prev_tarball.as_deref()));
    r.is0(&format!("phase C: deploy.sh {pt} (rollback) exits 0"), rc);
    r.check_oneshots("phase C");
    let post = match r.unit_set() {
        Ok(p) => p,
        Err(e) => {
            r.bad("phase C: unit set after rollback matches pre-upgrade snapshot", &e);
            return;
        }
    };
    let (pre_set, post_set): (BTreeSet<&String>, BTreeSet<&String>) = (pre.iter().collect(), post.iter().collect());
    let diff: Vec<String> = pre_set.difference(&post_set).map(|u| format!("< {u}")).chain(post_set.difference(&pre_set).map(|u| format!("> {u}"))).collect();
    if diff.is_empty() {
        r.ok("phase C: unit set after rollback matches pre-upgrade snapshot (no extra/dropped units)");
    } else {
        r.bad("phase C: unit set after rollback matches pre-upgrade snapshot", &diff.iter().take(10).cloned().collect::<Vec<_>>().join("\n"));
    }
    let rc = h.run(&r.uninstall()).rc;
    r.is0("phase C: uninstall.sh --yes after rollback exits 0", rc);
}

#[allow(clippy::too_many_arguments)]
fn phase_d(r: &mut Run, tag: &str, pt: &str, prev_tb: Option<PathBuf>, prev_dir: &Path, labels: &Labels, land_ref: &str) {
    let h = r.h;
    // The aged base is --aged-tag when given (a release many cuts behind), else phase B's predecessor.
    let (pt, aged): (String, Option<PathBuf>) = match r.o.a.aged_tag.clone() {
        Some(at) => {
            let t = r.acquire(&at, r.o.a.aged_tarball.clone().as_deref(), &prev_dir.join("aged"));
            r.is0(&format!("phase D: aged base tarball present ({at})"), if t.is_some() { 0 } else { 1 });
            if let Some(tb) = &t {
                r.stage_release_source(tb, &at);
            }
            (at, t)
        }
        None => {
            let t = match r.o.a.prev_tarball.clone().or(prev_tb).or_else(|| tarball_in(prev_dir)) {
                Some(t) => {
                    r.ok(&format!("phase D: prev tarball already present: {}", t.file_name().unwrap_or_default().to_string_lossy()));
                    Some(t)
                }
                None => {
                    let t = r.download(pt, prev_dir);
                    r.is0(&format!("phase D: gh release download {pt} (aged base)"), if t.is_some() { 0 } else { 1 });
                    t
                }
            };
            (pt.to_string(), t)
        }
    };
    let pt = pt.as_str();
    if let Some(tb) = &aged {
        r.install_tarball(tb);
    }
    r.sync_scratch("phase D");
    let arc = if r.o.releases().join("current").is_dir() { r.install_sh() } else { 1 };
    r.is0(&format!("phase D: install.sh ({pt}, aged) exits 0"), arc);
    let (ready_path, ready) = r.ready_sh();
    if ready.rc == 0 {
        r.ok("phase D: ready.sh exits 0 after aged install");
    } else {
        r.bad("phase D: ready.sh exits 0 after aged install", &format!("{} exit {}", ready_path.display(), ready.rc));
        println!("{}", ready.text);
    }
    if arc != 0 {
        return;
    }
    r.without_round_vm_host();

    // Seed: an open bead, a closed bead, two statutes.
    let mut written = Vec::new();
    let mut seed = |what: &str, c: Cmd| {
        let o = h.run(&c);
        let id = extract_bead_id(&o.text);
        if o.rc != 0 {
            written.push(format!("{what} exit {}: {}", o.rc, tail(&o.text, 3)));
        }
        id
    };
    let create = |title: &str| r.content(&["create", "--title", title, "--label", "acceptance-seed", "--type", "task"]);
    let open = seed("create the open bead", create("aged-install: open seed bead (pre-upgrade)"));
    let closed = seed("create the closed bead", create("aged-install: closed seed bead (pre-upgrade)"));
    if let Some(id) = &closed {
        // The machine's close: the content door refuses `close`, a bead's state being its row's.
        seed("close", r.tool("spira-lc").args(["close", id, "--reason", "acceptance: closed for aged-install migration test"]));
    }
    for n in [1, 2] {
        let key = format!("law-acceptance-aged-seed-{n}");
        seed("remember", r.content(&["remember", "--key", &key]).stdin(format!("aged-install acceptance seed: statute {n} for migration verification")));
    }
    let seeds: Vec<String> = [open, closed].into_iter().flatten().collect();
    r.check("phase D: aged-install seeds written: two beads, one closed, two memories", written.is_empty() && seeds.len() == 2, || {
        format!("{} bead id(s); {}", seeds.len(), written.join("; "))
    });
    let pre_beads = r.count_beads();
    let pre_mems = r.count_memories();

    // The operator's override goes where every operator edit goes: the one config file.
    let conf = r.o.toml();
    let path = format!("spira.{}", OVERRIDE_KEY.trim_start_matches("SPIRA_").to_ascii_lowercase());
    let val = (20 + r.o.pid % 70).to_string();
    if let Err(e) = spira_config::set_paths_in_file(&conf, &[(&path, &val)]) {
        r.bad("phase D: operator override written", &format!("{}: {e}", conf.display()));
    }

    h.show(&r.tool("world.sh").arg("start"));
    let active = first_fields(&r.systemctl(&["list-units", "--state=active", "--no-legend", "--plain"]).out);
    // The loop is the scheduler's tasks (spira-runtime); an older predecessor still runs it on
    // the sentinel timer.
    let live = |u: &&String| u.contains("spira-sentinel") || (u.starts_with("spira-runtime") && !u.contains("-watch"));
    r.check("phase D: loop active (world live before upgrade)", active.iter().any(|u| live(&u)), || "neither spira-runtime* nor spira-sentinel* in active units".into());
    r.wait_units_settled(30);

    let drc = h.show(&r.deploy_tag());
    r.is0(&format!("phase D: deploy.sh {tag} (aged upgrade) exits 0 — no rollback"), drc);
    if drc == 0 {
        match (pre_beads, r.count_beads()) {
            (Some(a), Some(b)) if b >= a => r.ok(&format!("phase D: bead count preserved through migration ({a} → {b})")),
            (Some(a), Some(b)) => r.bad("phase D: bead count preserved through migration", &format!("before={a} after={b} — rows lost")),
            (a, b) => r.bad("phase D: bead count preserved through migration", &format!("could not count beads: before={a:?} after={b:?}")),
        }
        match (pre_mems, r.count_memories()) {
            (Some(a), Some(b)) if b >= a => r.ok(&format!("phase D: memory count preserved through migration ({a} → {b})")),
            (Some(a), Some(b)) => r.bad("phase D: memory count preserved through migration", &format!("before={a} after={b} — rows lost")),
            (a, b) => r.bad("phase D: memory count preserved through migration", &format!("could not count memories: before={a:?} after={b:?}")),
        }
        for id in &seeds {
            // The row, not its history: a row nothing has moved has none.
            let row = h.run(&r.tool("spira-lc").args(["show", id]));
            r.check(&format!("phase D: pre-upgrade bead {id} has lifecycle rows after the aged upgrade"), row.rc == 0, || {
                format!("spira-lc show {id} rc={} — {}", row.rc, tail(&row.text, 3))
            });
        }
        let doc = h.show(&r.tool("doctor"));
        r.is0("phase D: doctor no fatal after aged upgrade", doc);
        let got = spira_config::load(&conf).ok().and_then(|d| spira_config::get_path(&d, &path)).unwrap_or_default();
        r.is_same("phase D: operator override survived aged upgrade", &val, &got);

        r.wait_units_settled(120);
        match r.spira_units("failed") {
            Ok(failed) => r.check("phase D: no failed spira units once settled after aged upgrade", failed.is_empty(), || failed.join("\n")),
            Err(e) => r.bad("phase D: no failed spira units once settled after aged upgrade", &e),
        }
        match r.world_halted_line() {
            Some(l) => r.bad("phase D: world running after aged upgrade", &l),
            None => r.ok("phase D: world running after aged upgrade"),
        }

        let base = r.rev(&format!("origin/{land_ref}")).unwrap_or_default();
        let (id, out) = r.file_probe(
            &format!("aged-install: post-upgrade land proof ({pt} → {tag})"),
            &probe_body(&format!("Prove world resumed and can land work after aged upgrade from {pt} to {tag}.")),
            labels,
        );
        match id {
            Some(id) => {
                r.ok(&format!("phase D: post-upgrade bead filed ({id})"));
                let landed = format!("phase D stage 5: bead {id} landed by ancestry");
                r.follow("phase D", &id, land_ref, &base, &landed);
            }
            None => r.bad("phase D: post-upgrade bead filed", &format!("output: {}", out.trim_end())),
        }
    }

    if drc == 0 {
        r.fast_tier_handoff("phase D");
    }

    // A migration that prevents downgrade must make deploy REFUSE and name it
    // (law-pin-by-migration-count).
    println!("\nphase D — aged rollback: deploy {pt} (refuse-or-succeed)");
    let rb = h.run(&r.deploy_prev(pt, aged.as_deref()));
    if rb.rc != 0 {
        if rb.text.to_lowercase().contains("migrat") {
            r.ok("phase D: rollback refused — names migration (law-pin-by-migration-count)");
        } else {
            r.bad("phase D: rollback refused but output does not name migration", &tail(&rb.text, 5));
        }
    } else {
        r.ok(&format!("phase D: rollback to {pt} succeeded"));
        match r.world_halted_line() {
            Some(l) => r.bad("phase D: world running after aged rollback", &l),
            None => r.ok("phase D: world running after aged rollback"),
        }
    }
    let rc = h.run(&r.uninstall()).rc;
    r.is0("phase D: uninstall.sh exits 0", rc);
}
