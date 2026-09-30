//! The four phases (DESIGN.md "acceptance", Phases), in the script's order and with its
//! check names, so a log from either compares line for line.

use super::*;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// One place builds each kind of call, so no call site can be written without its
/// environment (the invariants the `acceptance-run` lint rule used to grep for).
impl Run<'_> {
    fn s(p: &Path) -> String {
        p.display().to_string()
    }

    /// The release under test's launcher environment — what a launcher gives every Spira
    /// process: `SPIRA_RELEASE` naming `current`, `PATH` with its `bin/` and `spira/` first
    /// (every tool is called by bare name, sp-gypjk), and `SPIRA_CONF` (DESIGN.md Decision 2).
    fn launcher_env(&self) -> Vec<(String, String)> {
        let cur = self.o.releases().join("current");
        vec![
            ("SPIRA_CONF".into(), Self::s(&self.o.conf())),
            ("SPIRA_RELEASE".into(), Self::s(&cur)),
            ("PATH".into(), format!("{}:{}:{}", Self::s(&cur.join("bin")), Self::s(&cur.join("spira")), self.o.base_path)),
        ]
    }

    /// `deploy.sh`, `uninstall.sh`, `world.sh`, `doctor.sh`: by bare name on the launcher env.
    fn tool(&self, name: &str) -> Cmd {
        Cmd::new(name).envs(&self.launcher_env())
    }

    /// `deploy.sh` of the release under test: always `--allow-draft` (acceptance runs before
    /// the draft is published), and the local tarball when this run was handed one.
    fn deploy_tag(&self) -> Cmd {
        let mut c = self.tool("deploy.sh").arg("--allow-draft");
        if let Some(t) = &self.o.a.tarball {
            c = c.arg("--tarball").arg(Self::s(t));
        }
        c.arg(&self.o.a.tag)
    }

    /// `deploy.sh` of the predecessor (a rollback), with its local tarball when handed one.
    fn deploy_prev(&self, prev: &str) -> Cmd {
        let mut c = self.tool("deploy.sh");
        if let Some(t) = &self.o.a.prev_tarball {
            c = c.arg("--tarball").arg(Self::s(t));
        }
        c.arg(prev)
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
    fn install_tarball(&self, tb: &Path) -> i32 {
        let c = Cmd::new(Self::s(&self.o.release_bin))
            .arg("install-tarball")
            .arg(Self::s(tb))
            .env("SPIRA_CONF", Self::s(&self.o.conf()))
            .env("SPIRA_RELEASES", Self::s(&self.o.releases()))
            .env("SPIRA_RUN", Self::s(&self.o.tmp.join("run")));
        self.h.show(&c)
    }

    fn install_sh(&self) -> i32 {
        let c = Cmd::new("bash").arg(Self::s(&self.o.releases().join("current/install.sh"))).arg("--skip-build").envs(&self.install_env());
        self.h.show(&c)
    }

    fn ready_sh(&self) -> (PathBuf, Out) {
        let p = self.o.releases().join("current/spira/ready.sh");
        let out = self.h.run(&Cmd::new("bash").arg(Self::s(&p)).envs(&self.install_env()));
        (p, out)
    }

    fn bd(&self, args: &[&str]) -> Cmd {
        Cmd::new("bd").arg("-C").arg(Self::s(&self.o.bd_db)).args(args.iter().copied())
    }

    fn git(&self, args: &[&str]) -> Cmd {
        Cmd::new("git").arg("-C").arg(Self::s(&self.o.a.scratch_repo)).args(args.iter().copied())
    }

    fn systemctl(&self, args: &[&str]) -> Out {
        self.h.run(&Cmd::new("systemctl").arg("--user").args(args.iter().copied()))
    }

    /// Start the sentinel by its installed (instance-suffixed) unit name. Best-effort: the
    /// stage that follows is what judges it.
    fn start_sentinel(&self) {
        let unit = first_fields(&self.systemctl(&["list-unit-files", "spira-sentinel*.service", "--no-legend", "--plain"]).out).into_iter().next().unwrap_or_default();
        self.systemctl(&["start", &unit]);
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
            let out = self.h.run(&self.gh(&["release", "view", tag, "--json", "assets", "--jq", ".assets[].name"]));
            if out.rc == 0 && out.out.lines().any(|l| l.ends_with(".tar.gz")) {
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
        for u in first_fields(&self.systemctl(&["list-unit-files", "--no-legend", "--plain", "spira-*.service"]).out) {
            if self.systemctl(&["show", "-p", "Type", "--value", &u]).out.trim() != "oneshot" {
                continue;
            }
            if self.systemctl(&["start", &u]).rc != 0 {
                failed.push(u);
            }
        }
        let name = format!("{label}: every installed oneshot spira-* unit starts clean");
        self.check(&name, failed.is_empty(), || format!("failed: {}", failed.join(", ")));
    }

    fn unit_set(&self) -> Vec<String> {
        unit_set(&self.systemctl(&["list-unit-files", "--no-legend"]).out)
    }

    fn bead_finished(&self, id: &str) -> bool {
        bead_finished(&self.h.run(&self.bd(&["show", id, "--json"])).out)
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
    fn file_probe(&self, title: &str, description: &str, labels: &Labels) -> (Option<String>, String) {
        let l = format!("acceptance,{},{},repo:{}", labels.plan, labels.scope, self.o.scratch_name());
        let out = self.h.run(&self.bd(&["create", "--title", title, "--description", description, "--label", &l, "--type", "task"]));
        (extract_bead_id(&out.text), out.text)
    }

    /// Stages 2-5 for a filed bead: summoned (branch), committed, closed, landed by ancestry
    /// on `origin/<land_ref>` past `base`.
    fn follow(&mut self, p: &str, id: &str, land_ref: &str, base: &str, landed_name: &str) {
        self.start_sentinel();
        let branch = format!("refs/heads/spira/{id}");
        let (ok, t) = self.poll(self.o.summon_secs, 2, || self.h.run(&self.git(&["show-ref", "--verify", "-q", &branch])).rc == 0);
        if ok {
            self.ok(&format!("{p} stage 2: aeon summoned for {id} — branch created ({t}s)"));
        } else {
            self.bad(&format!("{p} stage 2: aeon summoned for {id}"), &format!("branch spira/{id} not created after {t}s"));
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

        // Kick the sentinel: its landing check dispatches the landing pass on the closed branch.
        self.start_sentinel();
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

    fn rev(&self, r: &str) -> Option<String> {
        let o = self.h.run(&self.git(&["rev-parse", r]));
        (o.rc == 0).then(|| o.out.trim().to_string()).filter(|s| !s.is_empty())
    }

    fn count_beads(&self) -> Option<usize> {
        let o = self.h.run(&self.bd(&["list", "--all", "--json"]));
        if o.rc != 0 {
            return None;
        }
        bd_json(&o.out).map(|v| v.len())
    }

    fn count_memories(&self) -> Option<usize> {
        let o = self.h.run(&self.bd(&["memories"]));
        (o.rc == 0).then(|| o.out.lines().filter(|l| !l.is_empty()).count())
    }
}

/// The builder partition labels the probe bead must carry.
struct Labels {
    plan: String,
    scope: String,
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
    for t in ["bd", "dolt", "git", "gh", "python3"] {
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
    let conf = r.o.conf();
    let _ = fs::create_dir_all(conf.parent().unwrap_or(Path::new("/")));
    let _ = fs::create_dir_all(r.o.release_src());
    let mut c = String::new();
    if let Some(a) = &r.o.a.agent {
        c.push_str(&format!("SPIRA_AGENT = {a}\n"));
    }
    c.push_str("SPIRA_OPERATED = 0\n");
    c.push_str(&format!("SPIRA_RELEASES = {}\n", releases.display()));
    c.push_str(&format!("SPIRA_RELEASE_REPO = {}\n", r.o.release_src().display()));
    if let Err(e) = fs::write(&conf, c) {
        r.bad("phase A: spira.conf written", &format!("{}: {e}", conf.display()));
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
    let rc = match &tarball {
        Some(tb) => r.install_tarball(tb),
        // No tarball is not an install that succeeded (the script's rc stayed 0 here).
        None => 1,
    };
    r.is0("phase A: release install-tarball exits 0", rc);
    if releases.join("current").is_dir() {
        let m = missing_release_bins(&releases.join("current"));
        r.check("phase A: all native binaries present and executable", m.is_empty(), || format!("missing: {m}"));
    }
    let install_rc = if releases.join("current").is_dir() { r.install_sh() } else { 1 };
    r.is0("phase A: install.sh exits 0", install_rc);

    // The builder's label keys, read whenever the release conf exists — not only after a
    // clean install: a failed install must not also mislabel the probe (sp-bn9go). The scope
    // is read as the installed services resolve it (identity from MANIFEST, no HOME_REPO).
    let home_repo = r.o.scratch_name();
    let labels = Labels {
        plan: r.conf_value("SPIRA_PLAN_LABEL", &[("SPIRA_HOME_REPO", &home_repo)]).unwrap_or_else(|| "plan".into()),
        scope: r.conf_value("SPIRA_SCOPE_LABEL", &[]).unwrap_or_else(|| home_repo.clone()),
    };

    let (ready_path, ready) = r.ready_sh();
    r.check("phase A: ready.sh exits 0 after install", ready.rc == 0, || format!("{} exit {}", ready_path.display(), ready.rc));
    if ready.rc != 0 {
        println!("{}", ready.text);
    }

    println!("\nphase A — end-to-end bead: file, sentinel summons aeon, land by ancestry");
    let land_ref = h.run(&r.git(&["rev-parse", "--abbrev-ref", "HEAD"])).out.trim().to_string();
    let land_ref = if land_ref.is_empty() { "main".to_string() } else { land_ref };
    let base = r.rev(&format!("origin/{land_ref}")).or_else(|| r.rev("HEAD")).unwrap_or_default();
    let (id, out) = r.file_probe(
        &format!("acceptance-run: trivial land proof for {tag}"),
        &format!("Acceptance test: commit a file named acceptance-probe-<bead-id>.txt to prove end-to-end landing works. Content: the tag under test is {tag}."),
        &labels,
    );
    match &id {
        Some(i) => r.ok(&format!("phase A: bead filed ({i})")),
        None => r.bad("phase A: bead filed", &format!("bd create output: {}", out.trim_end())),
    }
    if let Some(id) = id {
        r.probe = Some(id.clone());
        // Claimability is the discriminating question: a bead the sentinel can see but no
        // predicate matches is indistinguishable from a silent sentinel.
        let sel = format!("{},{}", labels.scope, labels.plan);
        let ready = h.run(&r.bd(&["ready", "--label", &sel, "--limit", "0", "--json"]));
        let name = format!("phase A: bead is claimable by builder predicate ({sel})");
        r.check(&name, ready_has(&ready.out, &id), || format!("bead {id} not found in 'bd ready --label {sel}' — builder predicate does not match bead labels"));
        let landed = format!("phase A stage 5: bead {id} landed on {}:{land_ref}", scratch.display());
        r.follow("phase A", &id, &land_ref, &base, &landed);
    }

    println!("\nphase A — uninstall and clean state");
    let rc = h.run(&r.tool("uninstall.sh").arg("--yes")).rc;
    r.is0("phase A: uninstall.sh --yes exits 0", rc);
    let left: Vec<String> = first_fields(&r.systemctl(&["list-unit-files", "--no-legend", "--plain"]).out).into_iter().filter(|u| u.starts_with("spira-")).collect();
    r.check("phase A: no spira-* units remain after uninstall", left.is_empty(), || left.join("\n"));

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
                    let prc = r.install_sh();
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
    let verdict = if r.fail == 0 { "PASS" } else { "FAIL" };
    let date = crate::fsutil::rfc3339(h.now());
    println!("verdict: {verdict}  tag={tag}  date={date}");

    if r.o.a.record {
        let name = tarball.as_ref().and_then(|t| t.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "unknown".into());
        let note = Note { verdict, date: &date, tag: &tag, pass: r.pass, fail: r.fail, tarball: sha.as_deref().map(|s| (s, name.as_str())), prev_tag: prev.as_deref(), waived: r.o.a.waive_upgrade }.text();
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
        h.run(&r.bd(&["create", "--title", &title, "--label", "defect,repo:spira,discovered-from:sp-ewwwq", "--type", "bug", "--description", &desc]));
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
    let pre = r.unit_set();
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
    let rc = h.show(&r.deploy_prev(pt));
    r.is0(&format!("phase C: deploy.sh {pt} (rollback) exits 0"), rc);
    r.check_oneshots("phase C");
    let post = r.unit_set();
    let diff: Vec<String> = pre.iter().filter(|u| !post.contains(u)).map(|u| format!("< {u}")).chain(post.iter().filter(|u| !pre.contains(u)).map(|u| format!("> {u}"))).collect();
    if diff.is_empty() {
        r.ok("phase C: unit set after rollback matches pre-upgrade snapshot (no extra/dropped units)");
    } else {
        r.bad("phase C: unit set after rollback matches pre-upgrade snapshot", &diff.iter().take(10).cloned().collect::<Vec<_>>().join("\n"));
    }
    let rc = h.run(&r.tool("uninstall.sh").arg("--yes")).rc;
    r.is0("phase C: uninstall.sh --yes after rollback exits 0", rc);
}

#[allow(clippy::too_many_arguments)]
fn phase_d(r: &mut Run, tag: &str, pt: &str, prev_tb: Option<PathBuf>, prev_dir: &Path, labels: &Labels, land_ref: &str) {
    let h = r.h;
    // Reuse phase B's predecessor tarball.
    let aged = match r.o.a.prev_tarball.clone().or(prev_tb).or_else(|| tarball_in(prev_dir)) {
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
    if let Some(tb) = &aged {
        r.install_tarball(tb);
    }
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

    // Seed: an open bead, a closed bead, two statutes.
    h.run(&r.bd(&["create", "--title", "aged-install: open seed bead (pre-upgrade)", "--label", "acceptance-seed", "--type", "task"]));
    let s2 = h.run(&r.bd(&["create", "--title", "aged-install: closed seed bead (pre-upgrade)", "--label", "acceptance-seed", "--type", "task"]));
    if let Some(id) = extract_bead_id(&s2.text) {
        h.run(&r.bd(&["close", &id, "--reason", "acceptance: closed for aged-install migration test"]));
    }
    for n in [1, 2] {
        h.run(&r.bd(&["remember", &format!("aged-install acceptance seed: statute {n} for migration verification"), "--key", &format!("law-acceptance-aged-seed-{n}")]));
    }
    let pre_beads = r.count_beads();
    let pre_mems = r.count_memories();

    let conf = r.o.conf();
    let val = (20 + r.o.pid % 70).to_string();
    let appended = fs::OpenOptions::new().append(true).open(&conf).and_then(|mut f| write!(f, "\n{OVERRIDE_KEY} = {val}\n"));
    if let Err(e) = appended {
        r.bad("phase D: operator override written", &format!("{}: {e}", conf.display()));
    }

    h.show(&r.tool("world.sh").arg("start"));
    let active = first_fields(&r.systemctl(&["list-units", "--state=active", "--no-legend", "--plain"]).out);
    r.check("phase D: sentinel timer active (world live before upgrade)", active.iter().any(|u| u.contains("spira-sentinel")), || "no spira-sentinel* in active units".into());
    h.sleep(30);

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
        let doc = h.show(&r.tool("doctor.sh"));
        r.is0("phase D: doctor.sh no fatal after aged upgrade", doc);
        let got = fs::read_to_string(&conf).ok().and_then(|t| conf_line_value(&t, OVERRIDE_KEY)).unwrap_or_default();
        r.is_same("phase D: operator override survived aged upgrade", &val, &got);

        h.sleep(120);
        let failed: Vec<String> = first_fields(&r.systemctl(&["list-units", "--state=failed", "--no-legend", "--plain"]).out).into_iter().filter(|u| u.starts_with("spira-")).collect();
        r.check("phase D: no failed spira units 2 min after aged upgrade", failed.is_empty(), || failed.join("\n"));
        match r.world_halted_line() {
            Some(l) => r.bad("phase D: world running after aged upgrade", &l),
            None => r.ok("phase D: world running after aged upgrade"),
        }

        let base = r.rev(&format!("origin/{land_ref}")).unwrap_or_default();
        let (id, out) = r.file_probe(
            &format!("aged-install: post-upgrade land proof ({pt} → {tag})"),
            &format!("Prove world resumed and can land work after aged upgrade from {pt} to {tag}."),
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

    // A migration that prevents downgrade must make deploy REFUSE and name it
    // (law-pin-by-migration-count).
    println!("\nphase D — aged rollback: deploy {pt} (refuse-or-succeed)");
    let rb = h.run(&r.deploy_prev(pt));
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
    let rc = h.run(&r.tool("uninstall.sh").arg("--yes")).rc;
    r.is0("phase D: uninstall.sh exits 0", rc);
}
