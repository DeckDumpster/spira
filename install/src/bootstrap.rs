//! Shared setup for the `units-install` and `unit-ensure` binaries: resolving host values and
//! the manifest from the environment a caller (`deploy.sh`, `landing-pass`, a human) already
//! set, exactly as `systemd/install.sh` and `unit-ensure.sh` both did by sourcing the same
//! `conf.sh`/`units.sh`. Kept out of the library's own tested modules (which take everything
//! as plain values) so the environment-reading glue is the only thing duplicated nowhere.

use crate::manifest::{self, Manifest};
use crate::values::HostValues;
use spira_config::process::TOML_ENV;
use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn env_var(k: &str) -> String {
    env::var(k).unwrap_or_default()
}

pub fn nonempty_env(k: &str) -> Option<String> {
    env::var(k).ok().filter(|v| !v.is_empty())
}

/// A REGISTERED key, from the one source of config (per Ryan 2026-10-05) — declared empty is
/// `None`; config that does not resolve is a refusal, never "unset".
pub fn declared(k: &str) -> Option<String> {
    match spira_config::process::cfg(k) {
        Ok(v) => Some(v).filter(|v| !v.is_empty()),
        Err(e) => {
            eprintln!("install: FATAL: {e}");
            std::process::exit(1)
        }
    }
}

pub fn which(prog: &str) -> Option<String> {
    let path = env::var("PATH").ok()?;
    for dir in path.split(':') {
        let p = Path::new(dir).join(prog);
        if p.is_file() {
            return Some(p.to_string_lossy().to_string());
        }
    }
    None
}

/// Resolve host values from the environment (conf.sh's own precedence: explicit environment
/// wins). Derives nothing beyond what conf.sh itself derives with a plain, no-side-effect
/// default for a render-relevant key: `SPIRA_HOME = SPIRA_REPO/spira` (conf.sh: no-colon
/// derivation), `SPIRA_COCKPIT = dirname(SPIRA_HOME)/cockpit` (conf.sh line ~937),
/// `SPIRA_SNAP_STALE_S = 60` (line ~951) — the two
/// `: "${VAR:=default}"` conf.sh lines that matter to rendering. A caller that already
/// sourced conf.sh (a human, `deploy.sh`) exports the real value first, so this default is
/// reached only when nothing did — never a silent override of an explicit setting.
///
/// NEITHER SET (sp-al35q): `SPIRA_REPO/spira` with an empty `SPIRA_REPO` rendered the
/// literal string "/spira" into every unit's substituted content — not a missing file (that
/// would at least be loud), a wrong one. A caller that sets neither (the real-world shape of
/// pre-activate) rendered every unit with this bogus home baked in and reported DIFFERS.
/// Falls back to the same release-relative resolution `templates_dir()` uses — `<release>/
/// spira` beside the binary's own `bin/`, verified by requiring `conf.sh` there, exactly as
/// `templates_dir`'s own fallback requires `spira-runtime.service` under its `systemd/`
/// candidate — and refuses outright when even that fails, rather than ever rendering
/// "/spira" again.
///
/// `repo` falls back to `derive_repo_filesystem` when unset: units substitute it into
/// `ExecStart=`, and an empty value bakes `--repo ` into the installed unit.
/// The repository units bake into `--repo` when `SPIRA_REPO` is unset: the repo map's
/// home-repo root when that is a git checkout, else the filesystem derivation. On a release
/// the derivation is the release directory, which has no `.git`, so a unit baked with it
/// exits 2 ("not a git repository"); the map already names the real checkout.
pub fn units_repo(mapped: Option<String>, derived: String, is_git: impl Fn(&Path) -> bool) -> String {
    match mapped.filter(|m| !m.is_empty() && is_git(Path::new(m))) {
        Some(m) => m,
        None => derived,
    }
}

/// The mailbox names `SPIRA_MAIL_READERS` registers (`name=command` entries, whitespace-
/// separated) — a registered key (spira/conf.d), through the one door, never a
/// competing environment override (per Ryan 2026-10-05: one source of config).
pub fn reader_mailboxes() -> Result<Vec<String>, String> {
    let raw = spira_config::process::cfg("SPIRA_MAIL_READERS")?;
    Ok(parse_reader_mailboxes(&raw))
}

fn parse_reader_mailboxes(raw: &str) -> Vec<String> {
    raw.split_whitespace().filter_map(|e| e.split('=').next()).map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).collect()
}

/// `mail ensure <name>` for every registered reader mailbox (sp-xp0u2). A fresh install made
/// them; an upgrade's re-render (units-install alone) never did, so `mail-health` found
/// `concierge: no such mailbox` after every upgrade and spira-notify failed 3.
pub fn ensure_reader_mailboxes() -> Result<(), String> {
    for name in reader_mailboxes()? {
        // batch-job: first-run mail ensure on a cold host; bounded at 120 s by timeout(1).
        let ok = Command::new("timeout").args(["120", spira_config::release_env::tool("mail")?.as_str(), "ensure", &name]).status().map(|s| s.success()).unwrap_or(false);
        if !ok {
            return Err(format!("could not create the {name} mailbox (mail ensure {name})"));
        }
    }
    Ok(())
}

/// Every registered key below goes through `spira_config::process::cfg`/`cfg_parse` — the
/// declared value in `$SPIRA_TOML`, never a competing environment override (per Ryan
/// 2026-10-05: one source of config). The three historical incidents this module used to
/// cite for "the environment wins" (sp-xp0u2, sp-al35q, sp-7i16g — a predecessor's trimmed
/// deploy environment needing to override a fresh, possibly-wrong resolution) are handled
/// upstream now: the predecessor sets `$SPIRA_TOML` to the right file, rather than this
/// binary trusting an exported override on top of whatever `$SPIRA_TOML` says.
pub fn host_from_env(instance: &str) -> Result<HostValues, String> {
    let env_map: std::collections::BTreeMap<String, String> = env::vars().collect();
    let cfg = |k: &str| spira_config::process::cfg(k);
    host_from(instance, &env_map, &cfg, spira_config::process::spec()?, crate::orchestrate::path_tail().unwrap_or_default())
}

/// [`host_from_env`] over explicit inputs: the environment, the config door and the values
/// read from process state are all passed in, so a test never edits the process environment.
pub fn host_from(instance: &str, env_map: &std::collections::BTreeMap<String, String>, cfg: &dyn Fn(&str) -> Result<String, String>, toml: String, path_tail: String) -> Result<HostValues, String> {
    let get = |k: &str| env_map.get(k).filter(|v| !v.is_empty()).cloned();
    let repo_env = get("SPIRA_REPO");
    let home = resolve_home(get("SPIRA_HOME"), repo_env.clone(), argv0_path().as_deref())?;
    let repo = repo_env.unwrap_or_else(|| {
        let mapped = spira_config::repos::Registry::from_env(env_map.clone(), Path::new(&home)).root("");
        let derived = spira_config::resolve::derive_repo_filesystem(Path::new(&home), env_map).to_string_lossy().into_owned();
        units_repo(mapped, derived, |p| p.join(".git").exists())
    });
    let cockpit = cfg("SPIRA_COCKPIT")?;
    let dav_addr = cfg(release::units::SCCACHE_DAV_ADDR_KEY)?;
    let env_bin = get("HOME").map(|h| spira_config::spira_env_bin(&h)).unwrap_or_default();
    let env_dolt = Path::new(&env_bin).join("dolt");
    let dolt = get("DOLT").or_else(|| env_dolt.is_file().then(|| env_dolt.to_string_lossy().into_owned())).or_else(|| which("dolt")).unwrap_or_default();
    // SPIRA_LC_PASSWORD_FILE is PROCEDURAL (spira/conf.d): its default depends on whether
    // the same-user credential file exists RIGHT NOW, so it has its own door, not `cfg`.
    let lc_password_file = spira_config::resolve::lc_credential_default(env_map);
    // `resolve_run_dir` (not bare `cfg("SPIRA_RUN")`) because it also runs the
    // instance-containment check `cfg()` does not.
    let run = spira_config::resolve::resolve_run_dir(env_map, Path::new(&home)).map(|p| p.to_string_lossy().into_owned())?;
    if run.is_empty() {
        return Err("SPIRA_RUN is unset and spira.run resolved empty — refusing to render units that would log to the filesystem root".to_string());
    }
    let sccache_dav_root = Some(cfg("SPIRA_SCCACHE_DAV_ROOT")?).filter(|r| !r.is_empty()).unwrap_or_else(|| format!("{run}/sccache-dav"));
    let broker_socket = spira_config::broker::socket_path(&run, cfg)?;
    Ok(HostValues {
        home,
        lc_password_file,
        toml,
        repo,
        run,
        db: cfg("SPIRA_DB")?,
        repo_map: cfg("SPIRA_REPO_MAP")?,
        cockpit,
        dolt_data: cfg("SPIRA_DOLT_DATA")?,
        dolt,
        prod: cfg("SPIRA_PROD")?,
        instance: instance.to_string(),
        snap_stale_s: cfg("SPIRA_SNAP_STALE_S")?,
        watchtower_start_timeout_s: cfg("SPIRA_WATCHTOWER_START_TIMEOUT_S")?,
        watchdog_every_s: cfg(watchdog::KEY_EVERY)?,
        unit_user: String::new(),
        path_tail,
        env_bin,
        sccache_dav_addr: dav_addr,
        sccache_dav_root,
        broker_socket,
    })
}

/// A root-owned harness would make the proc-starts unit run as root.
pub fn refuse_root_owner(uid: u32, home: &str) -> Result<(), String> {
    if uid == 0 {
        return Err(format!("{home} is owned by root; proc-starts must run as the operator's own user, not root"));
    }
    Ok(())
}

/// The uid that owns a path. A seam because a round's test sandbox maps uid 0 alone, so no
/// fixture there can be anything but root-owned.
pub type Owner<'a> = &'a dyn Fn(&Path) -> Result<u32, String>;

/// [`Owner`] from the filesystem.
pub fn stat_owner(path: &Path) -> Result<u32, String> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).map(|m| m.uid()).map_err(|e| format!("cannot stat {}: {e}", path.display()))
}

/// A uid's `(user name, home directory)`.
pub type Passwd<'a> = &'a dyn Fn(u32) -> Result<(String, PathBuf), String>;

/// [`Passwd`] from the host's own user database.
pub fn getent_passwd(uid: u32) -> Result<(String, PathBuf), String> {
    let out = spira_config::bounded::bounded("getent").args(["passwd", &uid.to_string()]).output().map_err(|e| format!("cannot run getent: {e}"))?;
    let line = String::from_utf8_lossy(&out.stdout);
    let fields: Vec<&str> = line.trim_end().split(':').collect();
    match (fields.first(), fields.get(5)) {
        (Some(user), Some(home)) if !user.is_empty() && !home.is_empty() => Ok((user.to_string(), PathBuf::from(home))),
        _ => Err(format!("getent passwd {uid} names no user with a home directory")),
    }
}

/// The environment `spira-install --proc-starts` runs under, and the user its unit runs as,
/// derived from the path it was invoked by: plain `sudo` hands it nothing else. Each value has
/// this one source; an inherited one is only checked against it.
pub fn proc_starts_env(argv0: &Path, env: &BTreeMap<String, String>, owner: Owner, passwd: Passwd) -> Result<(BTreeMap<String, String>, String), String> {
    let release = argv0
        .parent()
        .filter(|bin| bin.file_name().is_some_and(|n| n == "bin"))
        .and_then(Path::parent)
        .filter(|r| r.join("spira/conf.d").is_dir() && r.join("systemd/spira-proc-starts.service").is_file())
        .ok_or_else(|| format!("{} is not a release's bin/; run it by its release path: sudo <release>/bin/spira-install --proc-starts", argv0.display()))?;
    let again = format!("sudo {}/bin/spira-install --proc-starts", release.display());

    let home = release.join("spira");
    let uid = owner(&home).map_err(|e| format!("{e}; run it from a readable release: {again}"))?;
    refuse_root_owner(uid, &home.display().to_string()).map_err(|e| format!("{e}; run it from a release the operator owns: sudo <release>/bin/spira-install --proc-starts"))?;
    let (user, user_home) = passwd(uid).map_err(|e| format!("cannot name the owner of {}: {e}; give uid {uid} a passwd entry, then run: {again}", home.display()))?;

    let toml = installed_spira_toml(&user_home, release)?;
    let (release_s, user_home_s) = (release.display().to_string(), user_home.display().to_string());
    for (key, derived) in [(TOML_ENV, &toml), (spira_config::RELEASE_ENV, &release_s)] {
        if let Some(inherited) = env.get(key).filter(|v| !v.is_empty() && *v != derived) {
            return Err(format!("{key} is {inherited} in the environment but this release and its installed units say {derived}; run it without: {again}"));
        }
    }
    let mut out = env.clone();
    out.insert(spira_config::RELEASE_ENV.into(), release_s);
    out.insert("SPIRA_HOME".into(), home.display().to_string());
    out.insert(TOML_ENV.into(), toml);
    out.insert("HOME".into(), user_home_s);
    Ok((out, user))
}

fn runs_the_scheduler(line: &str) -> bool {
    let Some(command) = line.trim().strip_prefix("ExecStart=") else { return false };
    let mut words = command.split_whitespace();
    words.next().is_some_and(|exe| Path::new(exe).file_name().is_some_and(|n| n == "spira-runtime")) && words.next() == Some("serve")
}

/// The one `SPIRA_TOML` the operator's installed `spira-runtime-*.service` units declare. A
/// unit is the scheduler's by what it runs, not by its file name.
fn installed_spira_toml(user_home: &Path, release: &Path) -> Result<String, String> {
    let home = user_home.display().to_string();
    let dir = crate::orchestrate::default_unit_dir(&|k| (k == "HOME").then(|| home.clone())).ok_or("no unit directory for the operator")?;
    let unlisted = |e: std::io::Error| format!("cannot list {}: {e}; as the operator make it a readable directory, then run: sudo {}/bin/spira-install --proc-starts", dir.display(), release.display());
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries.collect::<Result<Vec<_>, _>>().map_err(unlisted)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(unlisted(e)),
    };
    let mut values = std::collections::BTreeSet::new();
    for entry in entries {
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "service") || !path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}; repair or remove it, then run: sudo {}/bin/spira-install --proc-starts", path.display(), release.display()))?;
        if !text.lines().any(runs_the_scheduler) {
            continue;
        }
        let declared = text.lines().filter_map(|l| l.trim().strip_prefix("Environment=")?.strip_prefix(TOML_ENV)?.strip_prefix('='));
        values.extend(declared.filter(|v| !v.is_empty()).map(str::to_string));
    }
    let (bin, dir, rel) = (release.join("bin"), dir.display(), release.display());
    let mut values = values.into_iter();
    match (values.next(), values.next()) {
        (Some(one), None) => Ok(one),
        (None, _) => Err(format!(
            "no spira-runtime-*.service (a unit running `spira-runtime serve`) naming SPIRA_TOML in {dir}; as the operator run the normal install first (SPIRA_TOML=<your config file> SPIRA_RELEASE={rel} {bin}/spira-install), then sudo {bin}/spira-install --proc-starts",
            bin = bin.display()
        )),
        (Some(a), Some(b)) => Err(format!(
            "the spira-runtime units in {dir} name different SPIRA_TOML values: {}; as the operator re-render them (SPIRA_TOML=<the one you mean> SPIRA_RELEASE={rel} {bin}/units-install) or remove the stale unit, then sudo {bin}/spira-install --proc-starts",
            [a, b].into_iter().chain(values).collect::<Vec<_>>().join(", "),
            bin = bin.display()
        )),
    }
}

/// `SPIRA_SCCACHE_DAV_ADDR`, a registered key — through the one door, never the bare
/// environment alone (law-a-binary-resolves-the-config-it-reads).
pub fn sccache_dav_addr() -> Result<String, String> {
    spira_config::process::cfg(release::units::SCCACHE_DAV_ADDR_KEY)
}

/// `SPIRA_HOME`, else `SPIRA_REPO/spira`, else the nearest `spira/` holding `conf.sh` above
/// this binary (a release's `bin/../spira`), else refuse — never the bare, unverified
/// `"{repo}/spira"` string sp-al35q let through when `repo` was itself empty. Pure (takes
/// `exe` explicitly) so the no-env refusal and the release-relative fallback are both
/// testable without touching the real environment or `argv0_path()`'s real `argv[0]`.
fn resolve_home(home_env: Option<String>, repo_env: Option<String>, exe: Option<&Path>) -> Result<String, String> {
    if let Some(h) = home_env {
        return Ok(h);
    }
    if let Some(r) = repo_env {
        return Ok(format!("{r}/spira"));
    }
    match exe.and_then(spira_above) {
        Some(p) => Ok(p.to_string_lossy().into_owned()),
        None => Err(
            "cannot resolve SPIRA_HOME: neither SPIRA_HOME nor SPIRA_REPO is set, and no spira/ \
             (holding conf.sh) was found beside this binary's own release"
                .to_string(),
        ),
    }
}

fn spira_above(exe: &Path) -> Option<PathBuf> {
    exe.ancestors().skip(1).map(|a| a.join("spira")).find(|d| d.join("conf.sh").is_file())
}

/// `watchd units` — the watcher manifest. `watchd.sh` is retired (sp-48f6g: rewritten to
/// the `watchd` crate); called by bare name exactly as units.sh did, just the new binary
/// name. `watchd` shells into `conf.sh` itself for everything conf.sh would otherwise
/// derive (`watchd::context`'s one seam), so `SPIRA_WATCHERS`'s own conf.sh default
/// (`$SPIRA_HOME/watchers`) reaches it for free as long as `SPIRA_HOME` is set — this
/// fallback stays as a second line of defense for a minimal env that sets neither.
pub fn watch_names() -> Result<Vec<String>, String> {
    let release = spira_config::release_env::release_root_for_process().unwrap_or_default();
    spira_config::git_refs::held(&release, "watch_names", &manifest_key(), read_watch_names)
}

fn stamp(path: &Path) -> String {
    match std::fs::metadata(path).and_then(|m| m.modified().map(|t| (t, m.len()))) {
        Ok((t, len)) => format!("{}:{:?}:{len}", path.display(), t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default()),
        Err(_) => format!("{}:absent", path.display()),
    }
}

/// What the manifest answer depends on: the manifest, each overlay file, and the config file(s).
fn manifest_key() -> String {
    let path_of = |k: &str| nonempty_env(k).or_else(|| spira_config::process::cfg(k).ok().filter(|v| !v.is_empty()));
    let read = nonempty_env("SPIRA_WATCHERS").or_else(|| nonempty_env("SPIRA_HOME").map(|h| format!("{h}/watchers")));
    let overlay = path_of("SPIRA_WATCHERS_OVERLAY").map(PathBuf::from);
    let mut paths: Vec<PathBuf> = read.into_iter().chain(path_of("SPIRA_WATCHERS")).map(PathBuf::from).chain(overlay.clone()).collect();
    if let Some(dir) = overlay {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
        files.sort();
        paths.extend(files);
    }
    if let Ok(spec) = spira_config::process::spec() {
        paths.extend(spec.split(':').map(PathBuf::from));
    }
    paths.iter().map(|p| stamp(p)).collect::<Vec<_>>().join("|")
}

fn read_watch_names() -> Result<Vec<String>, String> {
    let watchers = nonempty_env("SPIRA_WATCHERS").or_else(|| nonempty_env("SPIRA_HOME").map(|h| format!("{h}/watchers")));
    // batch-job: watchd shells into conf.sh, which outlasts a 5s call deadline on a loaded host
    let mut cmd = std::process::Command::new("timeout");
    cmd.args(["120", spira_config::release_env::tool("watchd")?.as_str(), "units"]);
    if let Some(w) = watchers {
        cmd.env("SPIRA_WATCHERS", w);
    }
    let out = cmd.output().map_err(|e| format!("cannot run watchd: {e}"))?;
    if out.status.code() == Some(124) {
        return Err("watchd units timed out — the host is too loaded to read the watcher manifest; not a manifest fault".to_string());
    }
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        return Err(format!("the watcher manifest is malformed: {}", why.trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.split_whitespace().map(|u| u.strip_prefix("spira-watch@").unwrap_or(u).strip_suffix(".service").unwrap_or(u).to_string()).collect())
}

/// `ctrl suspended`'s TSV (`subject<TAB>reason` per line, sp-6onps) — the control plane
/// moved from a bash library (`CTRL_LIB=1 . ctrl.sh`) to a compiled binary, which cannot be
/// sourced, so install.sh now reads every suspension once this way instead of parsing
/// `ctrl.sh list --json`'s render. A missing/failing `ctrl` means nothing is suspended,
/// matching the bash fallback.
pub fn suspended_set() -> std::collections::BTreeSet<String> {
    let out = spira_config::bounded::bounded("ctrl").arg("suspended").output();
    let Ok(out) = out else { return Default::default() };
    if !out.status.success() {
        return Default::default();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().filter_map(|l| l.split('\t').next()).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

pub fn unit_dir() -> PathBuf {
    if let Some(d) = nonempty_env("SPIRA_UNIT_DIR") {
        return PathBuf::from(d);
    }
    crate::orchestrate::default_unit_dir(&nonempty_env).unwrap_or_else(|| PathBuf::from(".config/systemd/user"))
}

pub fn templates_dir() -> PathBuf {
    let exe = argv0_path();
    templates_dir_from(nonempty_env("SPIRA_HOME"), nonempty_env("SPIRA_REPO"), exe.as_deref())
}

/// `SPIRA_HOME/../systemd`, else `SPIRA_REPO/systemd`, else the nearest `systemd/` holding unit
/// templates above this binary (a release's `bin/../systemd`), else a cwd-relative `systemd`.
/// The binary's own location is what the retired systemd/install.sh used (its own dirname);
/// without it, pre-activate — which sets neither variable — looked in its cwd and every
/// release from sp-31dm0 on failed verify (sp-w1r4f).
pub fn templates_dir_from(home: Option<String>, repo: Option<String>, exe: Option<&Path>) -> PathBuf {
    let d = home
        .map(|h| PathBuf::from(h).join("../systemd"))
        .or_else(|| repo.map(|r| PathBuf::from(r).join("systemd")))
        .or_else(|| exe.and_then(systemd_above))
        .unwrap_or_else(|| PathBuf::from("systemd"));
    d.canonicalize().unwrap_or(d)
}

fn systemd_above(exe: &Path) -> Option<PathBuf> {
    exe.ancestors().skip(1).map(|a| a.join("systemd")).find(|d| d.join("spira-runtime.service").is_file())
}

/// `argv[0]`, resolved to where it actually sits, but with every symlink component left
/// exactly as invoked — never `env::current_exe()`'s fully resolved path.
///
/// `current_exe()` canonicalizes every symlink in the path; testenv's own release staging
/// (and the gate's fixture release) link `bin/<tool>` to wherever cargo actually built it
/// and `systemd/`/`spira/` to the real checkout — two unrelated directories once resolved,
/// so `systemd_above`'s walk from a `current_exe()` path landed in cargo's own target/
/// directory, which has no `systemd/` sibling at all, and `templates_dir()` fell through to
/// a cwd-relative guess (every unit showed DIFFERS because the "clean" comparison rendered from the wrong templates).
///
/// A bare name (no `/`) is NOT already the resolved path the way it is when a shell execs a
/// PATH-found command: bash rewrites its own `$0` to the full path PATH search landed on
/// before exec, but a non-shell caller — `env PATH=... units-install --diff`, a direct
/// `execvp`/`posix_spawnp` — calls `execvp` directly, which resolves the PATH search
/// internally but passes argv[0] through to the new process completely unchanged.
/// A refusal when `exe` lives in a release that is not the one `current` names: such a
/// binary would re-render every installed unit back to its own, older release.
pub fn stale_release_refusal(tool: &str, exe: &Path) -> Option<String> {
    let skew = spira_config::release_skew::skew(exe.parent()?.parent()?)?;
    Some(format!(
        "{tool}: REFUSING to write units — {}; an old release never downgrades the installed units. Run it from {}/bin",
        skew.describe(),
        skew.current.display()
    ))
}

pub fn refuse_if_stale_release(tool: &str) -> Option<String> {
    stale_release_refusal(tool, &std::env::current_exe().ok()?)
}

pub fn argv0_path() -> Option<PathBuf> {
    let arg0 = env::args_os().next()?;
    let p = PathBuf::from(&arg0);
    if p.components().count() > 1 {
        return if p.is_absolute() { Some(p) } else { Some(env::current_dir().ok()?.join(p)) };
    }
    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(&arg0);
        if is_exec(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_exec(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// Build this box's manifest from the environment, printing units.sh's own informational
/// notes to stderr.
pub fn manifest_from_env(instance: &str) -> Result<Manifest, String> {
    // Same resolution host_from_env gives `--repo` (the repo map's home-repo root when it is
    // a git checkout, else the release dir): a failure to resolve it at all means the rest of
    // the install fails too, so treating it as "not a checkout" here costs nothing extra.
    let repo_is_git_checkout = host_from_env(instance).map(|h| Path::new(&h.repo).join(".git").exists()).unwrap_or(false);
    let m = manifest::build(&manifest::Inputs {
        instance: instance.to_string(),
        dolt_data_set: declared("SPIRA_DOLT_DATA").is_some(),
        dolt_durable_set: declared("SPIRA_DOLT_DURABLE").is_some(),
        sccache_dav_addr_set: !sccache_dav_addr()?.is_empty(),
        repo_is_git_checkout,
        lc_system_mode: spira_config::resolve::lc_system_mode(),
        watch_names: watch_names(),
    })?;
    for note in &m.notes {
        eprintln!("note: {note}");
    }
    Ok(m)
}

/// The halt stamp, or cutover mode's record: a world held for a cutover is still stopped, so an
/// install enables its units without starting them.
pub fn world_halted() -> bool {
    declared("SPIRA_RUN").map(|r| halted_at(Path::new(&r))).unwrap_or(false)
}

fn halted_at(run: &Path) -> bool {
    run.join("world.halted").exists() || run.join("world.cutover-mode").exists()
}

#[cfg(test)]
mod templates_dir_tests {
    use super::*;

    #[test_id(tvivmpo)]
    #[test]
    fn with_no_env_the_templates_are_found_beside_the_binarys_release() {
        let t = testkit::TempDir::new("tpl-dir");
        let rel = t.path().join("rel");
        std::fs::create_dir_all(rel.join("bin")).unwrap();
        std::fs::create_dir_all(rel.join("systemd")).unwrap();
        std::fs::write(rel.join("systemd/spira-runtime.service"), "[Unit]\n").unwrap();
        let got = templates_dir_from(None, None, Some(&rel.join("bin/units-install")));
        assert_eq!(got, rel.join("systemd").canonicalize().unwrap());
    }

    #[test_id(ty7lrz2)]
    #[test]
    fn the_environment_still_wins_over_the_binarys_location() {
        let t = testkit::TempDir::new("tpl-dir-env");
        let got = templates_dir_from(None, Some(t.path().display().to_string()), Some(Path::new("nonexistent/bin/x")));
        assert_eq!(got, t.path().join("systemd"));
    }
}

#[cfg(test)]
mod resolve_home_tests {
    use super::*;

    #[test_id(triuurr)]
    #[test]
    fn spira_home_wins_over_everything() {
        let got = resolve_home(Some("explicit/spira".into()), Some("repo".into()), Some(Path::new("bin/x")));
        assert_eq!(got, Ok("explicit/spira".to_string()));
    }

    #[test_id(t7x8m2f)]
    #[test]
    fn spira_repo_wins_over_the_binarys_location_when_home_is_unset() {
        let got = resolve_home(None, Some("repo".into()), Some(Path::new("nonexistent/bin/x")));
        assert_eq!(got, Ok("repo/spira".to_string()));
    }

    /// NEITHER SET, exe resolves (sp-al35q): derives `<release>/spira` beside the binary's
    /// own location, verified by requiring conf.sh there — the same resolution
    /// `templates_dir()` uses for `systemd/`, applied to `spira/` instead.
    #[test_id(tc1bg5q)]
    #[test]
    fn with_no_env_home_is_found_beside_the_binarys_release() {
        let t = testkit::TempDir::new("home-dir");
        let rel = t.path().join("rel");
        std::fs::create_dir_all(rel.join("bin")).unwrap();
        std::fs::create_dir_all(rel.join("spira")).unwrap();
        std::fs::write(rel.join("spira/conf.sh"), "# conf.sh\n").unwrap();
        let got = resolve_home(None, None, Some(&rel.join("bin/units-install")));
        assert_eq!(got, Ok(rel.join("spira").to_string_lossy().into_owned()));
    }

    /// NEITHER SET, exe does not resolve to a real spira/ (sp-al35q): refuses outright --
    /// returns Err, never the bare, unverified "/spira" string the bug rendered into every
    /// unit's substituted content before this fix (an empty SPIRA_REPO formatted straight
    /// into "{repo}/spira" with no existence check at all).
    #[test_id(texgbj5)]
    #[test]
    fn with_no_env_and_no_resolvable_exe_it_refuses_rather_than_guessing() {
        let got = resolve_home(None, None, Some(Path::new("nonexistent/bin/units-install")));
        assert!(got.is_err(), "{got:?}");
    }

    #[test_id(t8tqhyd)]
    #[test]
    fn with_no_env_and_no_exe_at_all_it_refuses() {
        let got = resolve_home(None, None, None);
        assert!(got.is_err(), "{got:?}");
    }
}

#[cfg(test)]
mod stale_release_tests {
    use super::*;

    #[test_id(tkzemb9)]
    #[test]
    fn an_old_release_binary_refuses_to_write_units() {
        let t = testkit::TempDir::new("stale-rel");
        let d = t.path().to_path_buf();
        for r in ["old", "new"] {
            std::fs::create_dir_all(d.join(r).join("bin")).unwrap();
        }
        std::os::unix::fs::symlink("new", d.join("current")).unwrap();
        let msg = stale_release_refusal("unit-ensure", &d.join("old/bin/unit-ensure")).expect("old release must refuse");
        assert!(msg.contains("REFUSING") && msg.contains("/new"), "{msg}");
        assert_eq!(stale_release_refusal("unit-ensure", &d.join("new/bin/unit-ensure")), None);
        assert_eq!(stale_release_refusal("unit-ensure", &d.join("missing/bin/unit-ensure")), None);
    }

    fn host_over_fixture(cfg: &dyn Fn(&str) -> Result<String, String>) -> Result<HostValues, String> {
        let t = testkit::TempDir::new("host-from-root");
        let toml = spira_config::resolve::locate_home_for_process().unwrap().join("testlib/complete.toml");
        let env: std::collections::BTreeMap<String, String> = [("SPIRA_HOME", t.path().join("h")), ("SPIRA_REPO", t.path().join("h")), ("SPIRA_TOML", toml)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.display().to_string()))
            .collect();
        host_from("prod", &env, cfg, "t/cfg".into(), String::new())
    }

    fn rendered_cache_root(configured: &str) -> String {
        let cfg = |k: &str| {
            Ok(match k {
                "SPIRA_SCCACHE_DAV_ROOT" => configured.to_string(),
                "SPIRA_BROKER_SOCKET" => "broker.sock".to_string(),
                _ => String::new(),
            })
        };
        let host = host_over_fixture(&cfg).expect("host values");
        let tmpl = spira_config::resolve::checkout_root_for_process().unwrap().join("systemd/sccache-dav.service");
        let unit = crate::values::render_file(&tmpl, &host, None).expect("render");
        let line = unit.lines().find(|l| l.starts_with("Environment=SCCACHE_DAV_ROOT=")).expect("root line").to_string();
        line.replace("Environment=SCCACHE_DAV_ROOT=", "")
    }

    #[test_id(tpi80ir)]
    #[test]
    fn the_cache_root_in_the_unit_is_the_configured_key_or_the_run_directory() {
        assert_eq!(rendered_cache_root("mnt/other-disk/store"), "mnt/other-disk/store");
        assert_eq!(rendered_cache_root(""), std::path::Path::new(std::path::MAIN_SEPARATOR_STR).join("fixture/userhome/spira/run/sccache-dav").display().to_string());
    }

    #[test_id(t9ig6fu)]
    #[test]
    fn the_broker_socket_install_renders_is_under_the_run_dir_unless_absolute() {
        let socket = |configured: &str| {
            let cfg = |k: &str| Ok(if k == "SPIRA_BROKER_SOCKET" { configured.to_string() } else { String::new() });
            host_over_fixture(&cfg).map(|h| (h.run, h.broker_socket))
        };
        let (run, relative) = socket("nested/b.sock").expect("host values");
        assert_eq!(relative, format!("{run}/nested/b.sock"));
        let absolute = format!("{}b.sock", std::path::MAIN_SEPARATOR);
        assert_eq!(socket(&absolute).expect("host values").1, absolute);
        let err = socket("").expect_err("an empty socket must refuse");
        assert!(err.contains("SPIRA_BROKER_SOCKET"), "{err}");
    }

    #[test_id(thfj3gy)]
    #[test]
    fn units_repo_prefers_a_mapped_git_checkout_over_the_derived_release_dir() {
        let git = |p: &Path| p == Path::new("h/harness");
        assert_eq!(units_repo(Some("h/harness".into()), "r/rel".into(), git), "h/harness");
        assert_eq!(units_repo(Some("h/not-git".into()), "r/rel".into(), git), "r/rel", "a mapped path that is not a checkout is not used");
        assert_eq!(units_repo(None, "r/rel".into(), git), "r/rel", "no map: the derivation, as before");
        assert_eq!(units_repo(Some(String::new()), "r/rel".into(), git), "r/rel");
    }
}

#[cfg(test)]
mod halt_tests {
    #[test_id(tf5avf1)]
    #[test]
    fn a_world_in_cutover_mode_installs_as_a_halted_one() {
        let d = testkit::TempDir::new("install-cutover-halted");
        assert!(!super::super::halted_at(d.path()));
        std::fs::write(d.path().join("world.cutover-mode"), "{}").unwrap();
        assert!(super::super::halted_at(d.path()), "the stamp is lifted for the window, the world is not running");
    }
}

#[cfg(test)]
mod watch_names_tests {
    use super::*;

    struct Rig {
        dir: testkit::TempDir,
        _env: testkit::EnvGuard,
    }

    fn rig(tag: &str) -> Rig {
        let dir = testkit::TempDir::new(tag);
        std::fs::create_dir_all(dir.join("rel/bin")).unwrap();
        std::fs::create_dir_all(dir.join("overlay")).unwrap();
        std::fs::write(dir.join("watchers"), "a\n").unwrap();
        std::fs::write(dir.join("toml"), "x\n").unwrap();
        let calls = dir.join("calls");
        testkit::write_exe(dir.join("rel/bin/watchd"), &format!("#!/bin/sh\necho x >> {c}\nif [ -f {d}/broken ]; then echo 'bad row' >&2; exit 1; fi\necho spira-watch@a.service\n", c = calls.display(), d = dir.display()));
        let env = testkit::env(&[
            ("SPIRA_RELEASE", dir.join("rel").to_str()),
            ("SPIRA_WATCHERS", dir.join("watchers").to_str()),
            ("SPIRA_WATCHERS_OVERLAY", dir.join("overlay").to_str()),
            ("SPIRA_TOML", dir.join("toml").to_str()),
        ]);
        Rig { dir, _env: env }
    }

    fn calls(r: &Rig) -> usize {
        std::fs::read_to_string(r.dir.join("calls")).unwrap_or_default().lines().count()
    }

    #[test_id(tkzludf)]
    #[test]
    fn an_unchanged_manifest_starts_watchd_once_and_a_touched_one_starts_it_again() {
        let r = rig("wn-held");
        for _ in 0..3 {
            assert_eq!(watch_names().unwrap(), vec!["a".to_string()]);
        }
        assert_eq!(calls(&r), 1);
        std::fs::write(r.dir.join("watchers"), "a\nb\n").unwrap();
        watch_names().unwrap();
        watch_names().unwrap();
        assert_eq!(calls(&r), 2);
        std::fs::write(r.dir.join("overlay/x.watchers"), "c\n").unwrap();
        watch_names().unwrap();
        assert_eq!(calls(&r), 3);
    }

    #[test_id(tfuhjir)]
    #[test]
    fn a_malformed_manifest_is_an_error_on_every_pass() {
        let r = rig("wn-bad");
        std::fs::write(r.dir.join("broken"), "").unwrap();
        for n in 1..=3 {
            let e = watch_names().unwrap_err();
            assert!(e.contains("malformed") && e.contains("bad row"), "{e}");
            assert_eq!(calls(&r), n);
        }
    }
}

#[cfg(test)]
mod proc_starts_env_tests {
    use super::*;

    struct Rig {
        td: testkit::TempDir,
        release: PathBuf,
        units: PathBuf,
    }

    fn rig(runtime_units: &[(&str, &str)]) -> Rig {
        let td = testkit::TempDir::new("proc-starts-env");
        let release = td.path().join("release");
        for d in ["spira/conf.d", "systemd", "bin"] {
            std::fs::create_dir_all(release.join(d)).unwrap();
        }
        std::fs::write(release.join("systemd/spira-proc-starts.service"), "[Service]\n").unwrap();
        let units = td.path().join("home/.config/systemd/user");
        std::fs::create_dir_all(&units).unwrap();
        for (instance, toml) in runtime_units {
            std::fs::write(units.join(format!("spira-runtime-{instance}.service")), format!("[Service]\nEnvironment=SPIRA_TOML={toml}\nExecStart=rel/bin/spira-runtime serve\n")).unwrap();
        }
        Rig { td, release, units }
    }

    fn derive(r: &Rig, argv0: &Path, env: &[(&str, &str)]) -> Result<(BTreeMap<String, String>, String), String> {
        derive_owned_by(1000, r, argv0, env)
    }

    fn derive_owned_by(uid: u32, r: &Rig, argv0: &Path, env: &[(&str, &str)]) -> Result<(BTreeMap<String, String>, String), String> {
        let env = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let home = r.td.path().join("home");
        let harness = r.release.join("spira");
        let owner = move |p: &Path| if p == harness { Ok(uid) } else { Err(format!("asked who owns {}", p.display())) };
        proc_starts_env(argv0, &env, &owner, &move |_| Ok(("operator".to_string(), home.clone())))
    }

    #[test_id(tex5q96)]
    #[test]
    fn the_owner_read_from_the_filesystem_is_the_uid_that_made_the_file() {
        let td = testkit::TempDir::new("proc-starts-owner");
        assert_eq!(stat_owner(td.path()), Ok(spira_config::resolve::current_uid()));
    }

    #[test_id(tnmuczz)]
    #[test]
    fn a_release_whose_harness_root_owns_is_refused_with_the_way_past_it() {
        let r = rig(&[("prod", "cfg/a.toml")]);
        let e = derive_owned_by(0, &r, &r.release.join("bin/spira-install"), &[]).unwrap_err();
        assert!(e.contains("owned by root") && e.contains(&r.release.join("spira").display().to_string()) && e.contains("sudo <release>/bin/spira-install --proc-starts"), "{e}");
    }

    #[test_id(tkm5zmi)]
    #[test]
    fn a_binary_outside_a_release_bin_is_refused_with_the_command_that_works() {
        let r = rig(&[("prod", "cfg/a.toml")]);
        let stray = r.td.path().join("target/debug/spira-install");
        let e = derive(&r, &stray, &[]).unwrap_err();
        assert!(e.contains(&stray.display().to_string()) && e.contains("sudo <release>/bin/spira-install --proc-starts"), "{e}");
    }

    #[test_id(tt6we6a)]
    #[test]
    fn with_no_installed_runtime_unit_the_refusal_names_the_unit_directory() {
        let r = rig(&[]);
        let e = derive(&r, &r.release.join("bin/spira-install"), &[]).unwrap_err();
        assert!(e.contains(&r.units.display().to_string()) && e.contains("--proc-starts"), "{e}");
        let rel = r.release.display();
        assert!(e.contains(&format!("SPIRA_TOML=<your config file> SPIRA_RELEASE={rel} {rel}/bin/spira-install")), "{e}");
    }

    #[test_id(tiabljm)]
    #[test]
    fn a_unit_directory_that_cannot_be_listed_is_reported_as_that_and_not_as_missing_units() {
        let r = rig(&[]);
        std::fs::remove_dir(&r.units).unwrap();
        std::fs::write(&r.units, "").unwrap();
        let e = derive(&r, &r.release.join("bin/spira-install"), &[]).unwrap_err();
        assert!(e.contains("cannot list") && e.contains(&r.units.display().to_string()) && e.contains("--proc-starts"), "{e}");
    }

    #[test_id(tm2w4a0)]
    #[test]
    fn runtime_units_that_disagree_on_the_config_are_refused_naming_each() {
        let r = rig(&[("prod", "cfg/a.toml"), ("two", "cfg/b.toml")]);
        let e = derive(&r, &r.release.join("bin/spira-install"), &[]).unwrap_err();
        assert!(e.contains("cfg/a.toml") && e.contains("cfg/b.toml"), "{e}");
        let rel = r.release.display();
        assert!(e.contains(&format!("SPIRA_TOML=<the one you mean> SPIRA_RELEASE={rel} {rel}/bin/units-install")), "{e}");
    }

    #[test_id(t1xmxv8)]
    #[test]
    fn an_inherited_value_is_checked_against_the_derived_one_and_never_preferred() {
        let r = rig(&[("prod", "cfg/a.toml")]);
        let argv0 = r.release.join("bin/spira-install");
        // A unit that runs something else has no say in which config is installed.
        std::fs::write(r.units.join("other.service"), "[Service]\nEnvironment=SPIRA_TOML=cfg/neighbour.toml\nExecStart=rel/bin/other serve\n").unwrap();
        for key in ["SPIRA_TOML", "SPIRA_RELEASE"] {
            let e = derive(&r, &argv0, &[(key, "cfg/other")]).unwrap_err();
            let derived = if key == "SPIRA_TOML" { "cfg/a.toml".to_string() } else { r.release.display().to_string() };
            assert!(e.contains("cfg/other") && e.contains(&derived), "{e}");
        }
        let release = r.release.display().to_string();
        let (env, user) = derive(&r, &argv0, &[("SPIRA_TOML", "cfg/a.toml"), ("SPIRA_RELEASE", &release), ("HOME", "root-home")]).unwrap();
        assert_eq!(user, "operator");
        assert_eq!(env["SPIRA_HOME"], format!("{release}/spira"));
        assert_eq!(env["HOME"], r.td.path().join("home").display().to_string());
    }
}
