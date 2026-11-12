//! The impure boundary: every place this crate shells out to `spira-lc`, `git`, `systemctl`,
//! `/proc`, or a `lib.sh` function it does not re-implement (DESIGN.md "Non-goals" — `lib.sh`
//! itself is frozen, wave 4's job, never this bead's). The fixture and fake binaries
//! the bash suites put on `PATH` work unmodified against it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The keys `resolve()` computes (or this process itself derives) that [`bootstrap_config`]
/// deliberately never sets into THIS process's own environment, even though the `compgen
/// -v` dump it replaces used to (wave4-decomposition.md row (b): "Hazard: this exports
/// SPIRA_HOME, SPIRA_REPO, SPIRA_REPO_MAP and SPIRA_RUNES to every child cockpit-collect
/// spawns. That is exactly what conf.sh forbids" — conf.sh's own comment: an inherited
/// SPIRA_HOME/SPIRA_REPO is "the seam every test suite drives a fixture through", and
/// letting it leak to a child that was just told, by argument, to source a DIFFERENT
/// lib.sh (`lib_call`'s own `home` parameter) would have that child's nested conf.sh
/// silently keep the parent's stale value instead of deriving its own from the file it
/// was just pointed at). [`SPIRA_MAX_DWARVES`] joins this set for a narrower reason: it is
/// host policy that must never leak to a child's environment either.
///
/// This set is about the EXPORT side only (this process's own `std::env`, and so every
/// child it spawns) — it has no bearing on reading a value in-process. `SPIRA_MAX_DWARVES`
/// and `SPIRA_REPO_MAP` are both ordinary keys in `resolve()`'s own `values` map
/// (`resolve::set!` writes every key there; only the typed shell/export side omits this
/// set), so [`max_dwarves`] and `repo_label_keys` read them straight through
/// `spira_config::process::cfg`, the one door, like any other registered key.
///
/// Anything this crate still needs from this set for the CHILD-PROCESS registry bridge is
/// read through [`Boot`]/[`repo_registry`] instead, never `std::env::var`.
const NEVER_EXPORTED: &[&str] = &["SPIRA_HOME", "SPIRA_REPO", "SPIRA_REPO_DERIVED", "SPIRA_REPO_MAP", "SPIRA_RUNES", "SPIRA_MAX_DWARVES"];

/// [`bootstrap_config`]'s own answer, cached in-process (never in `std::env`) for the one
/// call site that still needs a [`NEVER_EXPORTED`] value for a CHILD process:
/// [`repo_registry`]. Unset outside `main()` — a unit test that exercises that function
/// directly (never calling `bootstrap_config` first) falls back to reading this process's
/// own environment exactly as it did before this bead, so no test needed rewiring for a
/// hazard that only matters once this binary starts spawning children.
struct Boot {
    repo_registry_env: BTreeMap<String, String>,
    repo_map_text: Option<String>,
}

static BOOT: OnceLock<Boot> = OnceLock::new();

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The declared harness home: `SPIRA_HOME`, else `$SPIRA_RELEASE/spira`; neither set is a
/// refusal naming them.
pub fn try_home_dir() -> Result<PathBuf, String> {
    spira_config::resolve::locate_home_for_process().map_err(|e| format!("cockpit-collect: {e}"))
}

/// Panics when [`try_home_dir`] fails; `main` calls [`try_run_dir`], which resolves the home,
/// first and refuses, so only a caller that skipped that check reaches the panic.
pub fn home_dir() -> PathBuf {
    try_home_dir().unwrap_or_else(|e| panic!("{e}"))
}

/// `spira/cockpit.sh` sourced `lib.sh` unconditionally at its own top level, before
/// dispatching to any probe — and `lib.sh` itself sources `conf.sh`
/// (`spira/lib.sh:24`), which resolves the rest of the repo's own config (`SPIRA_WIKI`,
/// `SPIRA_MAIL`, `SPIRA_CI_PARK_MAX`, and everything else in `SPIRA_CONF_KEYS`) into that
/// process's environment. (`config-fence` flags naming the config file's name even in a
/// comment — this crate never opens it, only benefits from `conf.sh`'s own resolution, so
/// the file is described rather than named here.) Every `*_keys` function then read those as plain `${VAR:-...}`,
/// for free.
///
/// UNTIL WAVE 4.8, this ran a `bash -c '. lib.sh; compgen -v ...'` subprocess once at
/// startup and imported its dump — a re-import seam: conf.sh's own resolution had already
/// moved to `spira_config::resolve()` (sp-eekjm/sp-ubcgo), so this was shelling out purely
/// to re-derive, in bash, values a Rust call can compute directly. It also leaked
/// [`NEVER_EXPORTED`] to this process's own environment, and so to every child this crate
/// spawns (`lib_call`'s own nested `. lib.sh`, `bd`, `git`) — exactly the hazard that set's
/// own doc explains.
///
/// NOW: [`spira_config::resolve::resolve_for_process`] in-process, using this crate's own
/// [`home_dir`] and [`spira_config::resolve::derive_home_repo`]/[`derive_repo_filesystem`]
/// for the two per-copy facts `resolve()` itself never self-locates. Every OTHER resolved
/// key is imported into this process's own environment exactly as before (`conf.sh`'s own
/// `${VAR:=default}` pattern means a variable this process already set — explicitly, by a
/// caller or a test — is never overwritten, so this only ever *adds* resolved defaults,
/// never overrides an explicit value); [`NEVER_EXPORTED`] keys are cached in [`BOOT`]
/// instead, read back only by [`repo_registry`] and [`max_dwarves`]. Best-effort: a missing
/// config document/registry or any other resolution failure leaves the environment
/// exactly as it was (the same as every `lib_call` already tolerates a bash failure
/// returning `None`).
pub fn bootstrap_config() {
    let Ok(home) = try_home_dir() else { return };
    let env_map: BTreeMap<String, String> = std::env::vars().collect();
    let repo_derived = spira_config::resolve::derive_repo_filesystem(&home, &env_map);
    let repo = env_map
        .get("SPIRA_REPO")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_derived.clone());

    let home_repo_default = env_map.get("SPIRA_HOME").filter(|s| !s.is_empty()).cloned().unwrap_or_else(|| home.to_string_lossy().into_owned());
    let mut repo_registry_env = env_map.clone();
    repo_registry_env.insert("SPIRA_HOME".into(), home_repo_default);
    repo_registry_env.insert("SPIRA_REPO".into(), repo.to_string_lossy().into_owned());
    repo_registry_env.insert("SPIRA_REPO_DERIVED".into(), repo_derived.to_string_lossy().into_owned());

    let resolved = spira_config::resolve::resolve_for_process(&home, &repo, &env_map).ok();

    let map_text = resolved
        .as_ref()
        .and_then(|r| {
            let p = r.get("SPIRA_REPO_MAP");
            (!p.is_empty()).then(|| p.to_string())
        })
        .and_then(|p| std::fs::read_to_string(p).ok());
    if let Some(r) = &resolved {
        repo_registry_env.insert("SPIRA_HOME_REPO".into(), r.get("SPIRA_HOME_REPO").to_string());
        repo_registry_env.insert("SPIRA_REPO_MAP".into(), r.get("SPIRA_REPO_MAP").to_string());
    }
    let _ = BOOT.set(Boot { repo_registry_env, repo_map_text: map_text });

    let Some(resolved) = resolved else { return };
    for (k, v) in importable(&resolved, &env_map) {
        std::env::set_var(k, v);
    }
}

/// The pure core of [`bootstrap_config`]'s import rule — every resolved key EXCEPT
/// [`NEVER_EXPORTED`] and one already present in `already_set` (conf.sh's own
/// `${VAR:=default}` pattern: a variable this process already had, explicitly, is never
/// overwritten). Split out as a `BTreeMap`-in-`BTreeMap`-out function, rather than inlined
/// into a loop over live `std::env` calls, so a unit test can check the rule itself without
/// mutating this process's real environment.
fn importable(resolved: &spira_config::resolve::Resolved, already_set: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    resolved
        .values
        .iter()
        .filter(|(k, _)| !NEVER_EXPORTED.contains(&k.as_str()))
        .filter(|(k, _)| !already_set.contains_key(k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// The repo registry (`spira_config::repos`, sp-37rmg "wave 4.11") — [`Boot`]'s cached
/// answer, or, for a unit test that exercises this function without ever calling
/// [`bootstrap_config`], this process's own environment, exactly as this read it before
/// this bead (see [`BOOT`]'s own doc). Replaces four of this crate's own `lib_call` round
/// trips (one bash subprocess each) with one file read — the repo registry is the
/// most-called family in the whole wave4 decomposition.
pub fn repo_registry() -> spira_config::repos::Registry {
    let home = home_dir();
    match BOOT.get() {
        // `Boot.repo_registry_env` already carries all four registry keys, correctly
        // resolved by `bootstrap_config` (which ran once, in-process, via
        // `resolve_for_process` — no bash at all) — `Registry::new` here is safe
        // (`#[doc(hidden)]`, test-only elsewhere) only because this specific caller
        // supplies an already-complete snapshot, not a bare environment.
        Some(b) => spira_config::repos::Registry::new(b.repo_map_text.as_deref(), &b.repo_registry_env, &home),
        // Never ran bootstrap_config (a unit test exercising this function alone):
        // Registry::from_env resolves the four keys in-process itself, the same one
        // production door every other crate uses now (sp-k6lku, following the
        // structural fix for sp-z3eyk).
        None => spira_config::repos::Registry::from_env(std::env::vars().collect(), &home),
    }
}

/// `SPIRA_MAX_DWARVES`, the one door: it is an ordinary key in `resolve()`'s own `values`
/// (never exported to a CHILD process — see [`NEVER_EXPORTED`] — but that is a separate
/// concern from reading it here, in-process).
pub fn max_dwarves() -> String {
    spira_config::process::cfg("SPIRA_MAX_DWARVES").unwrap_or_default()
}

pub fn try_run_dir() -> Result<PathBuf, String> {
    let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    spira_config::resolve::resolve_run_dir(&env, &try_home_dir()?).map_err(|e| format!("cockpit-collect: {e}"))
}

/// Panics when [`try_run_dir`] fails; `main` calls `try_run_dir` first and refuses, so only
/// a caller that skipped that check reaches the panic.
pub fn run_dir() -> PathBuf {
    try_run_dir().unwrap_or_else(|e| panic!("{e}"))
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

/// `content <args>`: bead content (title, labels, comments) through the store's `content`
/// verb over its socket, the one door — never `bd` itself. `SPIRA_BDJSON_FIXTURE` set ->
/// `bdsim <fixture> <args>` (the test seam the `test-cockpit-*.sh` suites drive). Retried up
/// to `SPIRA_BDQ_CONN_RETRIES` (default 2) times while bd says "invalid connection". `None`
/// on any failure; the caller renders `?`, never 0.
pub fn content(args: &[&str]) -> Option<String> {
    if let Ok(fixture) = std::env::var("SPIRA_BDJSON_FIXTURE") {
        if !fixture.is_empty() {
            let out = spira_config::bounded::bounded(spira_config::release_env::tool("bdsim").ok()?)
                .arg(&fixture)
                .args(args)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .ok()?;
            if out.status.success() {
                return Some(String::from_utf8_lossy(&out.stdout).into_owned());
            }
            return None;
        }
    }
    let lc = spira_store::lc_call::LcClient::configured().ok()?;
    let tries: u32 = env_or("SPIRA_BDQ_CONN_RETRIES", "2").parse().unwrap_or(2).max(1);
    content_on(&lc, args, tries)
}

/// [`content`] through `lc`: the store's `content` verb, retried up to `tries` times while
/// bd's answer says its connection was invalid.
pub fn content_on(lc: &spira_store::lc_call::LcClient, args: &[&str], tries: u32) -> Option<String> {
    let argv: Vec<&str> = std::iter::once("content").chain(args.iter().copied()).collect();
    let mut attempt: u32 = 1;
    loop {
        let r = lc.call(&argv).ok()?;
        if r.code == 0 {
            return Some(r.out);
        }
        if !bead::bdq::should_retry(r.code, attempt, tries, r.err.contains("invalid connection")) {
            return None;
        }
        attempt += 1;
    }
}

/// Drop any banner lines a wrapper printed before the first line that starts a JSON value.
pub fn json_only(s: &str) -> &str {
    bead::bdq::json_only(s)
}

/// `contentjson <args>` == `content <args> --json | json_only`.
pub fn contentjson(args: &[&str]) -> Option<String> {
    let mut full: Vec<&str> = args.to_vec();
    full.push("--json");
    content(&full).map(|s| json_only(&s).to_string())
}

/// True when probes read the `SPIRA_BDJSON_FIXTURE` simulation rather than the live store.
pub fn fixture_mode() -> bool {
    std::env::var("SPIRA_BDJSON_FIXTURE").is_ok_and(|f| !f.is_empty())
}

pub const PANE_TTL_SECS: i64 = 60;
const HISTORY_TTL_SECS: i64 = 600;
const LOCK_WAIT_MS: u64 = 20_000;

thread_local! {
    static DATA_WRITES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Data writes this thread has issued through [`write_if_changed`].
pub fn data_writes() -> usize {
    DATA_WRITES.with(std::cell::Cell::get)
}

/// Writes `content` to `path` only when it differs from what is there; an identical file gets
/// its mtime refreshed (a metadata update, no data blocks) because readers age snapshots by
/// mtime. Answers whether data was written.
pub fn write_if_changed(path: &Path, content: impl AsRef<[u8]>) -> std::io::Result<bool> {
    let content = content.as_ref();
    if std::fs::read(path).is_ok_and(|have| have == content) {
        let touched = std::fs::OpenOptions::new().write(true).open(path).and_then(|f| f.set_modified(SystemTime::now()));
        if touched.is_ok() {
            return Ok(false);
        }
    }
    DATA_WRITES.with(|c| c.set(c.get() + 1));
    std::fs::write(path, content).map(|()| true)
}

/// [`write_if_changed`] through a temp file and rename, so a reader never sees a partial file.
pub fn replace_if_changed(path: &Path, content: &str) -> std::io::Result<bool> {
    let same = std::fs::read(path).is_ok_and(|have| have == content.as_bytes());
    if same && std::fs::OpenOptions::new().write(true).open(path).and_then(|f| f.set_modified(SystemTime::now())).is_ok() {
        return Ok(false);
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    DATA_WRITES.with(|c| c.set(c.get() + 1));
    if let Err(e) = std::fs::write(&tmp, content) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path).map(|()| true)
}

fn merge_rows(parts:&[Option<String>]) -> Option<String> {
    let mut all: Vec<serde_json::Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for p in parts {
        for row in json_rows(p.clone())? {
            let fresh = row.get("id").and_then(|i| i.as_str()).is_none_or(|i| seen.insert(i.to_string()));
            if fresh {
                all.push(row);
            }
        }
    }
    serde_json::to_string(&all).ok()
}

/// One cross-process read: every probe is its own process, so a memo in memory shares nothing.
/// A fresh file under `dir` is served; otherwise one process holds the lock and fetches while
/// the rest wait for its answer.
pub fn shared_read(dir: &Path, key: &str, ttl: i64, now: i64, fetch: impl FnOnce() -> Option<String>) -> Option<String> {
    let _ = std::fs::create_dir_all(dir);
    let file = dir.join(format!("{key}.json"));
    let lock = dir.join(format!("{key}.lock"));
    let fresh = || {
        let m = std::fs::metadata(&file).ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
        (now - m < ttl).then(|| std::fs::read_to_string(&file).ok()).flatten()
    };
    let started = std::time::Instant::now();
    loop {
        if let Some(hit) = fresh() {
            return Some(hit);
        }
        if std::fs::OpenOptions::new().write(true).create_new(true).open(&lock).is_ok() {
            let got = fetch();
            if let Some(raw) = &got {
                let _ = replace_if_changed(&file, raw);
            }
            let _ = std::fs::remove_file(&lock);
            return got;
        }
        let stale = std::fs::metadata(&lock).ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|e| e.as_millis() as u64 > LOCK_WAIT_MS);
        if stale {
            let _ = std::fs::remove_file(&lock);
        } else if started.elapsed().as_millis() as u64 > LOCK_WAIT_MS {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn with_labels(raw: Option<String>, labels: &str) -> Option<String> {
    let want: Vec<&str> = labels.split(',').filter(|l| !l.is_empty()).collect();
    if want.is_empty() {
        return raw;
    }
    let rows = json_rows(raw)?;
    let kept: Vec<_> = rows
        .into_iter()
        .filter(|r| {
            let have: Vec<&str> = r.get("labels").and_then(|l| l.as_array()).into_iter().flatten().filter_map(|l| l.as_str()).collect();
            want.iter().all(|w| have.contains(w))
        })
        .collect();
    serde_json::to_string(&kept).ok()
}

const LIVE_ARGS: [&str; 4] = ["list", "--limit", "0", "--json"];

fn read_json(args: &[&str]) -> Option<String> {
    content(args).map(|s| json_only(&s).to_string())
}

pub fn ttl_from_env(key: &str, default: i64) -> i64 {
    env_or(key, &default.to_string()).parse().unwrap_or(default)
}

fn base_read(key: &str, ttl: i64, fetch: impl FnOnce() -> Option<String>) -> Option<String> {
    shared_read(&run_dir().join("pane-read"), key, ttl, now(), fetch)
}

fn pane_ttl() -> i64 {
    ttl_from_env("SPIRA_PANE_READ_TTL", PANE_TTL_SECS)
}

/// The one unscoped read of live beads every label-scoped view is filtered from.
fn live_base() -> Option<String> {
    base_read("live", pane_ttl(), || read_json(&LIVE_ARGS))
}

/// Live beads only, what a default `list` answers; its own shared read, so no row's status is compared.
pub fn live_beads(labels: &str) -> Option<String> {
    with_labels(live_base(), labels)
}

/// The window the cockpit's day counts read: THROUGHPUT and the landing funnel.
pub const DAY_WINDOW_SECS: i64 = 86_400;
/// The window of the czar triggers: the lifecycle store serves online data (law-the-lifecycle-store-serves-online-data).
pub const ONLINE_WINDOW_SECS: i64 = 14 * 86_400;

/// Beads carrying `labels` updated in the `window_secs` before `now`, closed ones included, no
/// descriptions. The window and the label filter are the store's.
pub fn history_on(lc: &spira_store::lc_call::LcClient, labels: &str, window_secs: i64, now: i64) -> Option<String> {
    let since = bead::claimdesc::iso_of(now - window_secs);
    let mut args = vec!["list", "--all", "--brief", "--limit", "0", "--json", "--updated-after", &since];
    if !labels.is_empty() {
        args.extend(["--label", labels]);
    }
    content_on(lc, &args, 1).map(|s| json_only(&s).to_string())
}

/// Beads carrying `labels` updated inside `window_secs`, closed ones included, for the probes
/// that count history. Read at most once per `SPIRA_HISTORY_READ_TTL` per label set and window.
pub fn history_beads(labels: &str, window_secs: i64) -> Option<String> {
    let ttl = ttl_from_env("SPIRA_HISTORY_READ_TTL", HISTORY_TTL_SECS);
    let key = format!("history-{}-{window_secs}", labels.replace(|c: char| !c.is_ascii_alphanumeric(), "_"));
    let all = base_read(&key, ttl, || {
        if fixture_mode() {
            return read_json(&["list", "--all", "--closed-after", "1970-01-01", "--json"]);
        }
        let lc = spira_store::lc_call::LcClient::configured().ok()?;
        history_on(&lc, labels, window_secs, now())
    });
    with_labels(merge_rows(&[live_base(), all]), labels)
}

/// Parse a `contentjson` response into a `Vec<Value>`, the shape every probe needs:
/// the answer is either a bare object or an array. Empty/unparseable input (a refusal, per
/// `contentjson`'s contract) returns `None`, never an empty vec — the two are different claims.
pub fn json_rows(raw: Option<String>) -> Option<Vec<serde_json::Value>> {
    let raw = raw?;
    if raw.trim().is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    Some(match v {
        serde_json::Value::Array(a) => a,
        other => vec![other],
    })
}

/// Generic bridge to a `lib.sh` function, the same seam `gate-run`'s `Real` uses for
/// `repo_root`/`spira_landref` (its `REPO_CONTEXT`/`LANDREF_SNIPPET`), generalised: `lib.sh`
/// (wave 4, `law-rust-rewrites-start-from-intent` proposes it last and explicitly "leave
/// lib.sh alone" for now) stays the one place these helpers are defined; this crate calls
/// them exactly as `spira/cockpit.sh` did, never re-derives their logic.
pub fn lib_call(home: &Path, func: &str, args: &[&str]) -> Option<String> {
    lib_call_with_stdin(home, func, args, None)
}

/// The lib.sh names whose bodies are one-line shims onto a library this crate links: the runes
/// registry (`spira-config rune`), `dwarf dwarf-named` and `strand dwarves-live-*`. Answered
/// here with what the shim printed; `None` for a name this does not answer.
fn lib_inproc(home: &Path, func: &str, args: &[&str]) -> Option<Option<String>> {
    let arg = |i: usize| args.get(i).copied().unwrap_or("");
    let roster = || spira_config::process::cfg("SPIRA_RUNES").ok();
    Some(match func {
        "spira_runes" => roster().map(|r| spira_config::runes::spira_runes(home, Some(&r))),
        "rune_get" => roster().map(|_| spira_config::runes::rune_get(home, arg(0), arg(1), arg(2))),
        "persona_model" => roster().map(|_| spira_config::runes::persona_model(arg(0), Some(arg(1)))),
        "dwarf_named" => Some(dwarf::naming::dwarf_named(Path::new(arg(0)))),
        "dwarves_live_total" | "dwarves_live_lanes" => {
            let cfg = strand::config::Config::resolve(&strand::config::Live::load()).ok()?;
            let n = if func == "dwarves_live_total" { strand::probe::dwarves_live_total(&cfg) } else { strand::probe::dwarves_live_lanes(&cfg) };
            Some(n.to_string())
        }
        _ => return None,
    })
}

pub fn lib_call_with_stdin(
    home: &Path,
    func: &str,
    args: &[&str],
    stdin: Option<&str>,
) -> Option<String> {
    // FORK-EDGE: queue_certified_list and queue_sort_rows are shims onto `queue-helpers`;
    // needs a queue library entry for them. Every other lib.sh name is answered in-process
    // by [`lib_inproc`] before this bridge is reached.
    if let Some(answer) = lib_inproc(home, func, args) {
        return answer;
    }
    const SNIPPET: &str = r#"set -uo pipefail
. "$1/lib.sh" >/dev/null 2>&1 || exit 96
f="$2"; shift 2
"$f" "$@"
"#;
    // FORK-EDGE: the queue-helpers lib.sh bridge above; nothing else reaches it.
    let mut cmd = spira_config::bounded::bounded("bash");
    cmd.arg("-c")
        .arg(SNIPPET)
        .arg("cockpit-collect-lib-bridge")
        .arg(home)
        .arg(func)
        .args(args)
        .envs(spira_config::release_env::child_path_env_for_process().ok()?)
        .stderr(Stdio::null());
    if let Some(input) = stdin {
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = cmd.spawn().ok()?;
        use std::io::Write;
        child
            .stdin
            .as_mut()?
            .write_all(input.as_bytes())
            .ok()?;
        let out = child.wait_with_output().ok()?;
        if out.status.success() {
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            None
        }
    } else {
        cmd.stdin(Stdio::null());
        let out = cmd.output().ok()?;
        if out.status.success() {
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            None
        }
    }
}

/// The `slots` row, appended in-process exactly as `tsd-write --family slots --root <run>
/// --field k=v ...` appends it: each value typed as `--field` types it, under the family
/// file's exclusive lock.
pub fn slots_row(run: &Path, fields: &[(&str, String)], ts: &str, host: &str) -> Result<(), String> {
    let parsed = fields.iter().map(|(k, v)| tsd::parse_field(&format!("{k}={v}"))).collect::<Result<Vec<_>, _>>()?;
    let line = tsd::build_row(ts, host, "slots", &parsed)?;
    tsd::append_line(&tsd::family_path(run, "slots"), &line)
}

/// `git -C <repo> <args>`, stdout on success, `None` on any non-zero exit or spawn failure.
pub fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

/// `systemctl --user is-active <unit>` -> `Some(true/false)`, or `None` when the unit is
/// unknown to systemd. A unit that cannot be found must never render as "not active"
/// (law-absence-needs-a-positive-control): the caller renders `?`, not `0`. Read through the
/// `Units` port (the user manager over D-Bus), never a `systemctl` child.
pub fn unit_active(unit: &str) -> Option<bool> {
    unit_active_on(units()?, unit)
}

/// `unit`'s activity as `units` states it: an unknown unit is not active, as `systemctl
/// is-active` printed `inactive` for one; a port that cannot answer is `None`.
pub fn unit_active_on(units: &dyn runtime::units::Units, unit: &str) -> Option<bool> {
    if unit == "?" {
        return None;
    }
    let facts = units.facts(&runtime::units::UnitHandle::named(unit)).ok()?;
    Some(facts.state == runtime::units::UnitState::Active)
}

/// This process's connection to the user manager, opened once; `None` when there is none.
fn units() -> Option<&'static dyn runtime::units::Units> {
    #[cfg(target_os = "linux")]
    {
        static UNITS: OnceLock<Option<Box<dyn runtime::units::Units>>> = OnceLock::new();
        UNITS
            .get_or_init(|| {
                let units = runtime::units::SystemdUnits::connect(std::sync::Arc::new(runtime::ports::WallClock)).ok()?;
                Some(Box::new(units) as Box<dyn runtime::units::Units>)
            })
            .as_deref()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Run an already-on-`PATH` tool by bare name (release/overrides.sh/tokens.sh/yield.sh/
/// tsd-query.sh/bdsim) — every one of these
/// remains exactly what it was: a separate component this bead does not own, invoked the
/// same way `spira/cockpit.sh` invoked it (law-units-build-what-they-exec: never construct
/// a path to it, resolve it off `PATH` like every other caller in the release).
pub fn run_tool(name: &str, args: &[&str], stdin: Option<&str>) -> Option<String> {
    // FORK-EDGE: generic runner for operator scripts and sibling tools (release, overrides.sh,
    // tokens.sh, yield.sh, tsd-query.sh, spira-claim, date); each is ported with its task.
    let mut cmd = spira_config::bounded::bounded(name);
    cmd.args(args).stderr(Stdio::null());
    if let Some(input) = stdin {
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = cmd.spawn().ok()?;
        use std::io::Write;
        child.stdin.as_mut()?.write_all(input.as_bytes()).ok()?;
        let out = child.wait_with_output().ok()?;
        if out.status.success() {
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            None
        }
    } else {
        cmd.stdin(Stdio::null());
        let out = cmd.output().ok()?;
        if out.status.success() {
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            None
        }
    }
}

pub fn read_trim(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim_end_matches('\n').to_string())
}

pub fn mtime_age_secs(path: &Path) -> Option<i64> {
    let m = std::fs::metadata(path).ok()?.modified().ok()?;
    let secs = m.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    Some((now() - secs).max(0))
}

/// `/proc/<pid>` exists.
pub fn proc_exists(pid: i64) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// `/proc/<pid>/cmdline`, NUL-joined bytes turned into a space-joined string.
pub fn proc_cmdline(pid: i64) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let parts: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect();
    Some(parts.join(" "))
}

/// Elapsed seconds since `pid` started, from procfs (`ps -o etimes=` without the `ps`). `None`
/// when the pid is gone or procfs cannot be read.
pub fn proc_etimes(pid: i64) -> Option<i64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let uptime = std::fs::read_to_string("/proc/uptime").ok()?;
    etimes_from(&stat, &uptime, clock_ticks()?)
}

/// `ps`'s etimes: system uptime less the process's start time (field 22 of `stat`, in clock
/// ticks since boot). The command name in field 2 may hold spaces and parentheses, so fields
/// are counted after its last `)`.
fn etimes_from(stat: &str, uptime: &str, ticks: i64) -> Option<i64> {
    let after = &stat[stat.rfind(')')? + 1..];
    let start: i64 = after.split_whitespace().nth(19)?.parse().ok()?;
    let up: f64 = uptime.split_whitespace().next()?.parse().ok()?;
    Some((up as i64 - start / ticks.max(1)).max(0))
}

fn clock_ticks() -> Option<i64> {
    // SAFETY: sysconf reads a constant and has no preconditions.
    let t = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    (t > 0).then_some(t as i64)
}

pub fn sleep(d: Duration) {
    std::thread::sleep(d);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_activity_is_read_through_the_units_port() {
        use runtime::units::{ScriptedUnits, UnitFacts, UnitState};
        let units = ScriptedUnits::new(std::sync::Arc::new(runtime::ports::VirtualClock::new(0)));
        let mut active = UnitFacts::absent();
        active.state = UnitState::Active;
        units.install("spira-sentinel.timer", active);
        let mut failed = UnitFacts::absent();
        failed.state = UnitState::Failed;
        units.install("spira-ops.timer", failed);
        assert_eq!(unit_active_on(&units, "spira-sentinel.timer"), Some(true));
        assert_eq!(unit_active_on(&units, "spira-ops.timer"), Some(false));
        assert_eq!(unit_active_on(&units, "spira-nothing.timer"), Some(false), "an unknown unit is inactive, as is-active said");
        assert_eq!(unit_active_on(&units, "?"), None);
        assert!(units.calls().is_empty(), "a read starts and stops nothing");
    }

    #[test]
    fn etimes_is_uptime_less_the_start_tick_and_survives_a_spaced_command_name() {
        let stat = "4242 (a (weird) name) S 1 4242 4242 0 -1 4194560 100 0 0 0 5 3 0 0 20 0 1 0 50000 1000 200";
        assert_eq!(etimes_from(stat, "1000.42 3999.10\n", 100), Some(500));
        assert_eq!(etimes_from("garbage", "1000.0 1", 100), None);
        assert!(proc_etimes(std::process::id() as i64).is_some(), "this process has an age");
        assert_eq!(proc_etimes(i32::MAX as i64), None);
    }

    #[test]
    fn the_slots_row_is_appended_in_process_as_tsd_write_wrote_it() {
        let dir = testkit::TempDir::new("cockpit-collect-slots-row");
        let fields = [("live", "3".to_string()), ("ceiling", "4".to_string()), ("capacity_paused", "?".to_string())];
        slots_row(dir.path(), &fields, "2026-11-03T12:00:00Z", "h1").unwrap();
        let text = std::fs::read_to_string(tsd::family_path(dir.path(), "slots")).unwrap();
        let row: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(row["family"], "slots");
        assert_eq!(row["ts"], "2026-11-03T12:00:00Z");
        assert_eq!(row["live"], 3);
        assert_eq!(row["capacity_paused"], "?");
    }

    #[test]
    fn dwarf_named_is_answered_in_process_and_the_queue_shims_are_not() {
        let dir = testkit::TempDir::new("cockpit-collect-dwarf-named");
        let pid = dir.path().join("dwarf-builder-1.pid");
        std::fs::write(dir.path().join("dwarf-builder-1.name"), "Brokk").unwrap();
        assert_eq!(lib_inproc(dir.path(), "dwarf_named", &[pid.to_str().unwrap()]), Some(Some("Brokk".to_string())));
        assert_eq!(lib_inproc(dir.path(), "dwarf_named", &["/nonexistent/x.pid"]), Some(Some("?".to_string())));
        assert_eq!(lib_inproc(dir.path(), "queue_certified_list", &["/r"]), None);
    }

    #[test]
    fn json_only_drops_banner_lines() {
        assert_eq!(json_only("warning: thing\n[1,2,3]\n"), "[1,2,3]\n");
        assert_eq!(json_only("{\"a\":1}"), "{\"a\":1}");
        assert_eq!(json_only("just noise"), "");
        assert_eq!(json_only(""), "");
    }

    // Pins the sp-pwmlj collapse: before it, this function's own copy tolerated leading
    // whitespace before `[`/`{` — `sed -n '/^[[{]/,$p'` do not.
    #[test]
    fn json_only_requires_column_one_same_as_the_real_fence() {
        assert_eq!(json_only("  [1]\n"), "");
    }

    fn resolved_with(pairs: &[(&str, &str)]) -> spira_config::resolve::Resolved {
        spira_config::resolve::Resolved {
            values: pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            warnings: vec![],
        }
    }

    #[test]
    fn importable_never_exports_the_forbidden_keys_even_when_resolved() {
        let r = resolved_with(&[
            ("SPIRA_HOME", "/should-never-leak"),
            ("SPIRA_REPO", "/should-never-leak"),
            ("SPIRA_REPO_DERIVED", "/should-never-leak"),
            ("SPIRA_REPO_MAP", "/should-never-leak"),
            ("SPIRA_RUNES", "builder groomer"),
            ("SPIRA_MAX_DWARVES", "12"),
            ("SPIRA_WIKI", "/var/spira/wiki"),
        ]);
        let got = importable(&r, &BTreeMap::new());
        for forbidden in NEVER_EXPORTED {
            assert!(!got.contains_key(*forbidden), "{forbidden} leaked: {got:?}");
        }
        assert_eq!(got.get("SPIRA_WIKI"), Some(&"/var/spira/wiki".to_string()));
    }

    #[test]
    fn importable_never_overrides_an_already_set_key() {
        let r = resolved_with(&[("SPIRA_ASK_LABEL", "from-resolve")]);
        let already = BTreeMap::from([("SPIRA_ASK_LABEL".to_string(), "from-a-test-fixture".to_string())]);
        let got = importable(&r, &already);
        assert!(!got.contains_key("SPIRA_ASK_LABEL"), "{got:?}");
    }

    #[test]
    fn importable_passes_through_every_other_resolved_key() {
        let r = resolved_with(&[("SPIRA_ASK_LABEL", "needs-ryan"), ("SPIRA_CI_PARK_MAX", "5")]); // literal-ok: fixture/fallback
        let got = importable(&r, &BTreeMap::new());
        assert_eq!(got.get("SPIRA_ASK_LABEL"), Some(&"needs-ryan".to_string())); // literal-ok: fixture/fallback
        assert_eq!(got.get("SPIRA_CI_PARK_MAX"), Some(&"5".to_string()));
    }

    // repo_registry_reads_live_env_when_boot_never_ran DELETED (per Ryan 2026-10-05, one
    // source of config): it asserted the registry picks up a SPIRA_REPO_MAP set in the
    // environment, which is exactly what the registry no longer reads.
    // max_dwarves_falls_back_to_live_env_when_boot_never_ran DELETED (per Ryan 2026-10-05, one
    // source of config): it asserted that a bare `SPIRA_MAX_DWARVES` env override reaches
    // `max_dwarves()` without any `SPIRA_TOML` — exactly the behaviour the one-door law
    // removes. `max_dwarves()` is now a one-line call through `spira_config::process::cfg`,
    // whose own resolution/caching is spira-config's tested responsibility; there is no
    // crate-local fallback logic left here to pin with a unit test, and `cfg`'s per-process
    // cache makes a fixture-driven unit test of this one-liner order-dependent rather than
    // meaningful (see the triage guide: only a fresh-binary test may vary config per case).
}

#[cfg(test)]
mod pane_read_tests {
    use super::*;

    #[test]
    fn twelve_probes_in_a_pane_pass_share_one_fetch() {
        let t = testkit::TempDir::new("pane-read");
        let calls = std::cell::Cell::new(0);
        for probe in 0..12 {
            let got = shared_read(&t.join("d"), "live", 30, 1000 + probe, || {
                calls.set(calls.get() + 1);
                Some(r#"[{"id":"a","labels":["plan","spira"]}]"#.to_string())
            });
            assert!(got.is_some());
        }
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn no_probe_reads_all_history_from_the_store() {
        let probes = [
            include_str!("io.rs"),
            include_str!("probes/core_detail.rs"),
            include_str!("probes/mod.rs"),
            include_str!("probes/reachable.rs"),
            include_str!("probes/unsent.rs"),
        ];
        let bounding = ["--id", "--parent", "--updated-after", "--closed-after", "--created-after"].map(|f| format!("\"{f}\""));
        let hits = |s: &str| {
            s.lines()
                .filter(|l| !l.trim_start().starts_with("//") && l.contains(&["\"-", "-all\""].concat()) && !bounding.iter().any(|b| l.contains(b.as_str()))
                    && !l.contains("\"--limit\", \"1\""))
                .count()
        };
        for planted in ["x(&[\"list\", \"--all\", \"--limit\", \"0\"])", "x(&[\"list\", \"--all\", \"--limit\", \"100000\"])"] {
            assert_eq!(hits(planted), 1, "matcher must see a planted offender: {planted}");
        }
        assert_eq!(hits("x(&[\"list\", \"--all\", \"--limit\", \"0\", \"--updated-after\", d])"), 0);
        for p in probes {
            assert_eq!(hits(p), 0);
        }
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;
    use spira_store::lc_call::{fake::Content, LcClient};
    use std::sync::Arc;

    #[test]
    fn history_reads_only_the_window_it_names() {
        let now = 1_794_000_000;
        let row = |id: &str, at: i64| serde_json::json!({"id": id, "status": "closed", "labels": ["plan"], "updated_at": bead::claimdesc::iso_of(at)});
        let fake = Arc::new(Content::new(vec![row("old", now - 30 * 86_400), row("new", now - 3_600)]));
        let lc = LcClient::over(fake.clone(), true);
        let got = history_on(&lc, "plan", DAY_WINDOW_SECS, now).unwrap();
        let ids: Vec<String> = json_rows(Some(got)).unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string()).collect();
        assert_eq!(ids, ["new"]);
        for req in fake.requests() {
            assert!(req.iter().any(|a| a == "--updated-after") && req.iter().any(|a| a == "--brief"), "{req:?}");
            let limits: Vec<&String> = req.windows(2).filter(|w| w[0] == "--limit").map(|w| &w[1]).collect();
            assert!(limits.iter().all(|l| *l == "0"), "{req:?}");
        }
    }
}
