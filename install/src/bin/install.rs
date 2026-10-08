//! `spira-install` — the root installer (`install.sh`, in Rust). Named `spira-install`, not
//! `install`: `/usr/bin/install` is a system command, and a release refuses to ship a binary
//! that shadows one (release::build::clashes). Nine phases: preflight, conflict checks,
//! config, build, database, units, hooks, cockpit, verify. Every tool this phase sequence
//! calls that has not itself moved to Rust yet (`doctor.sh`, `configure.sh`, `build.sh`,
//! `seed.sh`, `mail`, `release session-hook`, `release intake`, `exclude.sh`, `ready.sh`)
//! is invoked by bare name on the launcher `PATH`, exactly as `install.sh` did — none of
//! them are this bead's scope.
//!
//! usage: spira-install [<instance>] [--dry-run] [--ephemeral] [--laptop] [--skip-build]
//!                       [--no-session-hook] [--system-user] [--populate-lifecycle]
//!   --system-user is standalone: root, phase 6.5 only.
//!   --populate-lifecycle is standalone: phase 4.6 only (the one-time lifecycle population,
//!   idempotent — the same step every full install runs after the lifecycle store).

use install::bootstrap::{self, nonempty_env};
use install::checks;
use install::install_units::{self, Ctx};
use install::systemctl::RealSystemctl;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

struct Opts {
    instance: Option<String>,
    dry: bool,
    ephemeral: bool,
    laptop: bool,
    skip_build: bool,
    no_session_hook: bool,
    system_user: bool,
    populate_lifecycle: bool,
}

fn parse_args() -> Result<Opts, String> {
    let mut o = Opts { instance: None, dry: false, ephemeral: false, laptop: false, skip_build: false, no_session_hook: false, system_user: false, populate_lifecycle: false };
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--dry-run" => o.dry = true,
            "--ephemeral" => o.ephemeral = true,
            "--laptop" => o.laptop = true,
            "--skip-build" => o.skip_build = true,
            "--no-session-hook" => o.no_session_hook = true,
            "--system-user" => o.system_user = true,
            "--populate-lifecycle" => o.populate_lifecycle = true,
            s if s.starts_with("--") => return Err(format!("unknown flag: {s}")),
            s if o.instance.is_none() => o.instance = Some(s.to_string()),
            s => return Err(format!("extra argument: {s}")),
        }
    }
    Ok(o)
}

fn phase(n: &str) {
    println!("\n[{n}]");
}
/// The pinned duckdb, the same version spira/testenv/Containerfile installs (DUCKDB_VERSION).
const DUCKDB_VERSION: &str = "1.5.5";

/// The pinned mozilla/sccache release (sp-x6v17). Same version doctor/src/real.rs's test
/// fixture (REAL_SCCACHE_018_HELP) captures `sccache --help` output for, so the webdav-feature
/// text this code checks at runtime is text an existing test already pins independently.
/// mozilla/sccache's Cargo.toml sets `default = ["all"]`, and `all` lists `webdav` — its
/// release CI (ci.yml's `build` job) builds the plain `sccache` binary for
/// x86_64/aarch64-unknown-linux-musl with no `--no-default-features`, so the published
/// tarball's binary already has the backend doctor::check_sccache requires. (The
/// `--no-default-features --features=dist-server` row in that matrix builds a DIFFERENT
/// binary, `sccache-dist`, not this one.)
const SCCACHE_VERSION: &str = "0.18.0";

/// The pinned inotify-tools release (sp-x6v17). inotify-tools is normally a distro package
/// (`apt install inotify-tools`) and install carries no sudo, so apt cannot be the
/// provisioning path here. Its own release workflow (.github/workflows/build.yml) builds
/// inotifywait with `ALL_STATIC=1` and asserts `file -L "$bin" | grep -q static` before
/// publishing — i.e. upstream already guarantees the binary this tarball carries has no
/// shared-library dependency on the box's libc/distro, which is what makes vendoring it
/// (rather than building from source, which would need a full distro toolchain) safe.
const INOTIFY_TOOLS_VERSION: &str = "4.26.262";

/// Map `std::env::consts::ARCH` to mozilla/sccache's release target triple. Upstream only
/// publishes Linux x86_64/aarch64 (musl) assets.
fn sccache_target(arch: &str) -> Result<&'static str, String> {
    match arch {
        "x86_64" => Ok("x86_64-unknown-linux-musl"),
        "aarch64" => Ok("aarch64-unknown-linux-musl"),
        other => Err(format!("no sccache build for architecture {other}")),
    }
}

/// Map `std::env::consts::ARCH` to inotify-tools' release arch label (its asset names use
/// the bare arch, unlike sccache's target triple).
fn inotify_tools_arch(arch: &str) -> Result<&'static str, String> {
    match arch {
        "x86_64" => Ok("x86_64"),
        "aarch64" => Ok("aarch64"),
        other => Err(format!("no inotify-tools build for architecture {other}")),
    }
}

fn sccache_url(target: &str) -> String {
    format!("https://github.com/mozilla/sccache/releases/download/v{SCCACHE_VERSION}/sccache-v{SCCACHE_VERSION}-{target}.tar.gz")
}

fn inotify_tools_url(arch: &str) -> String {
    format!("https://github.com/inotify-tools/inotify-tools/releases/download/{INOTIFY_TOOLS_VERSION}/inotify-tools-{INOTIFY_TOOLS_VERSION}-{arch}-linux.tar.gz")
}

/// The path of the `sccache` member inside its own release tarball — `tar -O` pulls just
/// this file out without ever extracting the rest to disk. (mozilla/sccache's release.yml
/// `Create release assets` step: `tar -zcvf "$d.tar.gz" "$d"` where `$d` is this exact
/// directory name — the same string the published asset's filename is built from.)
fn sccache_tar_member(target: &str) -> String {
    format!("sccache-v{SCCACHE_VERSION}-{target}/sccache")
}

/// Parses `sccache --help`'s "Enabled features" block the same way doctor::check_sccache
/// does (doctor/src/lib.rs) — reimplemented rather than shared, since install does not
/// otherwise depend on the doctor crate (it invokes doctor as an external binary, phase 0).
fn sccache_help_has_webdav(help: &str) -> bool {
    help.lines().find(|l| l.trim_start().starts_with("WebDAV:")).is_some_and(|l| l.trim_end().ends_with("true"))
}

/// The pinned aerc tag (git.sr.ht/~rjarry/aerc). NOT a "v"-prefixed go module version (its
/// tags are bare "0.22.0", which `go install git.sr.ht/~rjarry/aerc@0.22.0` cannot resolve —
/// go module version queries require a canonical `vX.Y.Z` — and aerc publishes no prebuilt
/// binary release either), so `spira/build-tarball.sh` builds it ONCE, at release-build
/// time, from the tag's own source archive, and ships the resulting binary inside the
/// release tarball at [`AERC_VENDORED_REL`]. Install's phase -1 (`install_aerc`, below)
/// only ever COPIES that already-built binary — never a network fetch, never a Go
/// toolchain, never `go install`, not even when no usable one is already present.
///
/// Before sp-41so3 this constant named a version install itself built from source at
/// install time: a fetched Go toolchain, then `go install` resolving ~95 of aerc's own
/// module dependencies over the network. law-install-installs-every-dependency still
/// requires install to provide aerc — there is no optional tier — but an install that needs
/// ~95 network round-trips to proxy.golang.org is fragile by construction: on 2026-10-04
/// local acceptance failed when every one of those fetches hit "net/http: TLS handshake
/// timeout" (this host's IPv6 is unreachable; load was high) — the identical build had
/// passed earlier the same day. Building once, on the release-build machine, and shipping
/// the binary removes the fragility without touching the law: install still provides aerc,
/// it just never has to build it.
const AERC_VERSION: &str = "0.22.0";

/// Where the release build (`spira/build-tarball.sh`) ships the aerc binary it built once,
/// relative to the release root — the same ancestor whose `bin/` holds this very
/// `spira-install` binary, and what
/// `spira_config::release_env::own_release_root_for_process` resolves to for it. Deliberately
/// NOT under `bin/`: `release::build::clashes` (and anything else that assumes `bin/` holds
/// only this workspace's own `[[bin]]` targets) never has to learn that aerc is not one of
/// them, and `spira/build-tarball.sh`'s own compat-symlink pass over `bin/` never has to
/// skip it either.
const AERC_VENDORED_REL: &str = "vendor/bin/aerc";

/// Parses `aerc -v`'s stdout (`"aerc 0.22.0 (go1.27.1 amd64 linux)\n"`, from main.go's
/// `ShowVersion`/`buildInfo` — main.Version is what `-ldflags "-X main.Version=..."` sets)
/// and reports whether it names exactly `version`.
fn aerc_reports_version(output: &str, version: &str) -> bool {
    output.split_whitespace().nth(1) == Some(version)
}

/// Whether `install_aerc` can skip the copy entirely: the aerc already at
/// `~/.local/bin/aerc` (the exact path `install_aerc` would overwrite) already reports the
/// pinned AERC_VERSION. `current` is `None` when that path is absent or did not run.
fn aerc_install_can_be_skipped(current: Option<&str>) -> bool {
    current.is_some_and(|s| aerc_reports_version(s, AERC_VERSION))
}

/// Phase -1: put ~/.local/bin on PATH (for the doctor this process runs next), set dolt's
/// metrics.disabled, and fetch every dependency doctor would otherwise FAIL on
/// (law-install-installs-every-dependency, operator ruling 2026-10-03: install provides every
/// dependency, there is no optional tier) — duckdb, sccache (with its webdav backend),
/// inotifywait, and aerc (copied from this release's own vendored binary — sp-41so3; see
/// AERC_VERSION's comment — never built here, never a network fetch).
fn dependencies(dry: bool) -> Result<(), String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is unset".to_string())?;
    let bin = Path::new(&home).join(".local/bin");
    let path = std::env::var("PATH").unwrap_or_default();
    if !path.split(':').any(|p| Path::new(p) == bin) {
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));
    }

    let metrics_off = Command::new("timeout")
        .args(["5", "dolt", "config", "--global", "--get", "metrics.disabled"])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false);
    if metrics_off {
        skip("dolt metrics.disabled already true");
    } else if dry {
        would("run: dolt config --global --add metrics.disabled true");
    } else {
        run_ok("timeout", &["5", "dolt", "config", "--global", "--add", "metrics.disabled", "true"])
            .map_err(|e| format!("cannot set dolt metrics.disabled: {e}"))?;
        info("set dolt metrics.disabled true (dolt config --global)");
    }

    fetch_duckdb(dry, &bin)?;
    fetch_sccache(dry, &bin)?;
    fetch_inotifywait(dry, &bin)?;
    install_aerc(dry, &bin, spira_config::release_env::own_release_root_for_process().as_deref())?;
    install_sendmail(dry, &home, &bin)?;
    Ok(())
}

fn render_sendmail_wrapper(releases: &str, toml: &str) -> String {
    format!(
        "#!/bin/sh\n\
         # spira-sendmail — aerc's outgoing command; written by spira-install, rewritten on every install.\n\
         R=$(readlink -f '{releases}/current') || exit 1\n\
         [ -x \"$R/bin/mail\" ] || {{ echo \"spira-sendmail: $R/bin/mail is not executable\" >&2; exit 1; }}\n\
         [ \"${{1:-}}\" = --check ] && exit 0\n\
         exec env SPIRA_RELEASE=\"$R\" SPIRA_TOML='{toml}' PATH=\"$R/bin:$R/spira:/usr/local/bin:/usr/bin:/bin\" \"$R/bin/mail\" sendmail \"$@\"\n"
    )
}

/// Returns `conf` with every `outgoing =` line pointing at `wrapper`, or `None` when there was
/// nothing to change.
fn rewrite_aerc_outgoing(conf: &str, wrapper: &str) -> Option<String> {
    let want = format!("outgoing = {wrapper}");
    let mut changed = false;
    let out: Vec<String> = conf
        .lines()
        .map(|l| {
            let is_outgoing = l.trim_start().strip_prefix("outgoing").is_some_and(|r| r.trim_start().starts_with('='));
            if is_outgoing && l.trim() != want {
                changed = true;
                want.clone()
            } else {
                l.to_string()
            }
        })
        .collect();
    changed.then(|| out.join("\n") + "\n")
}

/// Installs `spira-sendmail` into `bin` and points an existing aerc account at it, so a release
/// cut cannot leave the operator's reply path at a binary that no longer exists. An absent
/// accounts.conf is left alone: no aerc account, nothing to repoint.
fn install_sendmail(dry: bool, home: &str, bin: &Path) -> Result<(), String> {
    let (Some(releases), Some(toml)) = (nonempty_env("SPIRA_RELEASES"), nonempty_env("SPIRA_TOML")) else {
        info("spira-sendmail NOT installed — SPIRA_RELEASES or SPIRA_TOML is unset");
        return Ok(());
    };
    let wrapper = bin.join("spira-sendmail");
    let body = render_sendmail_wrapper(releases.trim_end_matches('/'), &toml);
    if dry {
        would(&format!("write {}", wrapper.display()));
    } else if std::fs::read_to_string(&wrapper).ok().as_deref() == Some(body.as_str()) {
        skip(&format!("{} is current", wrapper.display()));
    } else {
        std::fs::create_dir_all(bin).map_err(|e| format!("mkdir {}: {e}", bin.display()))?;
        let tmp = bin.join(".spira-sendmail.new");
        std::fs::write(&tmp, &body).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| format!("chmod spira-sendmail: {e}"))?;
        std::fs::rename(&tmp, &wrapper).map_err(|e| format!("install {}: {e}", wrapper.display()))?;
        info(&format!("installed {}", wrapper.display()));
    }

    let conf_dir = nonempty_env("XDG_CONFIG_HOME").unwrap_or_else(|| format!("{home}/.config"));
    let conf = Path::new(&conf_dir).join("aerc/accounts.conf");
    let Ok(text) = std::fs::read_to_string(&conf) else {
        skip(&format!("no {} — nothing to repoint", conf.display()));
        return Ok(());
    };
    match rewrite_aerc_outgoing(&text, &wrapper.display().to_string()) {
        None => skip(&format!("aerc outgoing already {}", wrapper.display())),
        Some(_) if dry => would(&format!("repoint aerc outgoing in {} at {}", conf.display(), wrapper.display())),
        Some(new) => {
            std::fs::write(&conf, new).map_err(|e| format!("write {}: {e}", conf.display()))?;
            info(&format!("repointed aerc outgoing in {} at {}", conf.display(), wrapper.display()));
        }
    }
    Ok(())
}

/// Fetch duckdb into ~/.local/bin when no duckdb is on PATH.
fn fetch_duckdb(dry: bool, bin: &Path) -> Result<(), String> {
    let have_duckdb = Command::new("timeout").args(["5", "duckdb", "--version"]).output().map(|o| o.status.success()).unwrap_or(false);
    if have_duckdb {
        skip("duckdb on PATH");
        return Ok(());
    }
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => return Err(format!("no duckdb build for architecture {other}")),
    };
    let url = format!("https://github.com/duckdb/duckdb/releases/download/v{DUCKDB_VERSION}/duckdb_cli-linux-{arch}.gz");
    let dest = bin.join("duckdb");
    if dry {
        would(&format!("fetch duckdb {DUCKDB_VERSION} from {url} into {}", dest.display()));
        return Ok(());
    }
    std::fs::create_dir_all(bin).map_err(|e| format!("cannot create {}: {e}", bin.display()))?;
    let gz = bin.join(".duckdb.download.gz");
    // batch-job: install downloads a pinned dependency once per host; bounded by curl --max-time 300.
    run_ok(
        "curl",
        &["-fsSL", "--retry", "3", "--retry-all-errors", "--connect-timeout", "10", "--max-time", "300", "-o", &gz.to_string_lossy(), &url],
    )
    .map_err(|e| format!("cannot fetch duckdb from {url}: {e}"))?;
    // batch-job: unpacking the downloaded duckdb once per install; bounded at 120 s.
    let out = Command::new("timeout").args(["120", "gunzip", "-c", &gz.to_string_lossy()]).output().map_err(|e| format!("gunzip: {e}"))?;
    let _ = std::fs::remove_file(&gz);
    if !out.status.success() || out.stdout.is_empty() {
        return Err(format!("cannot unpack duckdb from {url}"));
    }
    let tmp = bin.join(".duckdb.new");
    std::fs::write(&tmp, &out.stdout).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| format!("chmod duckdb: {e}"))?;
    std::fs::rename(&tmp, &dest).map_err(|e| format!("cannot install {}: {e}", dest.display()))?;
    let ok = Command::new("timeout").args(["5", dest.to_str().unwrap_or("duckdb"), "--version"]).output().map(|o| o.status.success()).unwrap_or(false);
    if !ok {
        return Err(format!("installed {} does not run", dest.display()));
    }
    info(&format!("installed duckdb {DUCKDB_VERSION} at {}", dest.display()));
    Ok(())
}

/// Fetch sccache into ~/.local/bin unless the sccache already resolvable on PATH already
/// reports the webdav backend (doctor::check_sccache's own bar — not a version comparison,
/// since that is the actual property both doctor and every build need). No `cargo` is
/// guaranteed present on a fresh box (acceptance run 37222619835 had neither cargo nor
/// sccache on PATH), so `cargo install sccache --features webdav` cannot be the
/// provisioning path install itself takes; fetch mozilla/sccache's own prebuilt release
/// binary instead, which already carries the feature (see SCCACHE_VERSION's comment).
fn fetch_sccache(dry: bool, bin: &Path) -> Result<(), String> {
    let current_help = Command::new("timeout")
        .args(["5", "sccache", "--help"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    if current_help.as_deref().is_some_and(sccache_help_has_webdav) {
        skip("sccache on PATH with webdav backend");
        return Ok(());
    }
    let target = sccache_target(std::env::consts::ARCH)?;
    let url = sccache_url(target);
    let dest = bin.join("sccache");
    if dry {
        would(&format!("fetch sccache {SCCACHE_VERSION} from {url} into {}", dest.display()));
        return Ok(());
    }
    std::fs::create_dir_all(bin).map_err(|e| format!("cannot create {}: {e}", bin.display()))?;
    let tgz = bin.join(".sccache.download.tar.gz");
    // batch-job: install downloads a pinned dependency once per host; bounded by curl --max-time 300.
    run_ok(
        "curl",
        &["-fsSL", "--retry", "3", "--retry-all-errors", "--connect-timeout", "10", "--max-time", "300", "-o", &tgz.to_string_lossy(), &url],
    )
    .map_err(|e| format!("cannot fetch sccache from {url}: {e}"))?;
    let member = sccache_tar_member(target);
    // batch-job: pulling the one binary member out of the downloaded tarball; bounded at 60 s.
    let out = Command::new("timeout").args(["60", "tar", "-xzf", &tgz.to_string_lossy(), "-O", &member]).output().map_err(|e| format!("tar: {e}"))?;
    let _ = std::fs::remove_file(&tgz);
    if !out.status.success() || out.stdout.is_empty() {
        return Err(format!("cannot unpack sccache from {url}"));
    }
    let tmp = bin.join(".sccache.new");
    std::fs::write(&tmp, &out.stdout).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| format!("chmod sccache: {e}"))?;
    std::fs::rename(&tmp, &dest).map_err(|e| format!("cannot install {}: {e}", dest.display()))?;
    let help = Command::new("timeout").args(["5", dest.to_str().unwrap_or("sccache"), "--help"]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    if !sccache_help_has_webdav(&help) {
        return Err(format!("installed {} does not report the webdav backend", dest.display()));
    }
    info(&format!("installed sccache {SCCACHE_VERSION} (webdav) at {}", dest.display()));
    Ok(())
}

/// Fetch inotifywait into ~/.local/bin unless it is already on PATH. No sudo, so this
/// vendors inotify-tools' own statically-linked release binary rather than `apt install
/// inotify-tools` (see INOTIFY_TOOLS_VERSION's comment on why that binary is safe to vendor).
fn fetch_inotifywait(dry: bool, bin: &Path) -> Result<(), String> {
    if which_prog("inotifywait").is_some() {
        skip("inotifywait on PATH");
        return Ok(());
    }
    let arch = inotify_tools_arch(std::env::consts::ARCH)?;
    let url = inotify_tools_url(arch);
    let dest = bin.join("inotifywait");
    if dry {
        would(&format!("fetch inotifywait {INOTIFY_TOOLS_VERSION} from {url} into {}", dest.display()));
        return Ok(());
    }
    std::fs::create_dir_all(bin).map_err(|e| format!("cannot create {}: {e}", bin.display()))?;
    let tgz = bin.join(".inotify-tools.download.tar.gz");
    // batch-job: install downloads a pinned dependency once per host; bounded by curl --max-time 300.
    run_ok(
        "curl",
        &["-fsSL", "--retry", "3", "--retry-all-errors", "--connect-timeout", "10", "--max-time", "300", "-o", &tgz.to_string_lossy(), &url],
    )
    .map_err(|e| format!("cannot fetch inotify-tools from {url}: {e}"))?;
    // batch-job: pulling the one binary member out of the downloaded tarball; bounded at 60 s.
    let out = Command::new("timeout").args(["60", "tar", "-xzf", &tgz.to_string_lossy(), "-O", "usr/bin/inotifywait"]).output().map_err(|e| format!("tar: {e}"))?;
    let _ = std::fs::remove_file(&tgz);
    if !out.status.success() || out.stdout.is_empty() {
        return Err(format!("cannot unpack inotifywait from {url}"));
    }
    let tmp = bin.join(".inotifywait.new");
    std::fs::write(&tmp, &out.stdout).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| format!("chmod inotifywait: {e}"))?;
    std::fs::rename(&tmp, &dest).map_err(|e| format!("cannot install {}: {e}", dest.display()))?;
    // inotifywait --help exits 1 by design (inotify-tools' own release check relies on this
    // too); its first line names the binary, which is all that is worth asserting here.
    let help = Command::new("timeout").args(["5", dest.to_str().unwrap_or("inotifywait"), "--help"]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    if !help.contains("inotifywait") {
        return Err(format!("installed {} does not run", dest.display()));
    }
    info(&format!("installed inotifywait {INOTIFY_TOOLS_VERSION} at {}", dest.display()));
    Ok(())
}

/// Phase -1's aerc step (sp-41so3): copy the release's own vendored aerc
/// ([`AERC_VENDORED_REL`], built once at release-build time by
/// `spira/build-tarball.sh`) to `bin`/aerc, unless the aerc already there already
/// reports [`AERC_VERSION`]. No network, no Go toolchain, no `go install` — ever; see
/// `AERC_VERSION`'s comment for why.
///
/// `release_root` is this `spira-install` binary's own release
/// (`spira_config::release_env::own_release_root_for_process`, resolved once by the
/// caller). `None` means this process is not running from a built release at all — a dev
/// checkout, `cargo run --bin spira-install` with no `spira/build-tarball.sh` output
/// anywhere above it — and that is a FAIL-CLOSED condition: law-install-installs-every-
/// dependency still requires install to provide aerc, so this names exactly what is
/// missing and how to produce it rather than silently skipping the dependency. Likewise
/// when `release_root` resolves but that release's tarball predates sp-41so3 (or was built
/// by something other than `spira/build-tarball.sh`) and simply has no
/// `AERC_VENDORED_REL` file.
fn install_aerc(dry: bool, bin: &Path, release_root: Option<&Path>) -> Result<(), String> {
    let dest = bin.join("aerc");
    let current = Command::new("timeout")
        .args(["5", dest.to_str().unwrap_or("aerc"), "-v"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    if aerc_install_can_be_skipped(current.as_deref()) {
        skip(&format!("aerc {AERC_VERSION} at {}", dest.display()));
        return Ok(());
    }
    // SPIRA_INSTALL_AERC_CONSIDERED (sp-41so3, same shape as phase 4.5's
    // SPIRA_INSTALL_LC_STORE_CONSIDERED): an install-suite fixture that installs from a
    // plain tree or a `release build --bin-dir` stage carries no AERC_VENDORED_REL at all
    // — that path is for production's own build-tarball.sh tarballs, never these fixtures
    // — so the fail-closed refusal below would stop every such suite at phase -1, long
    // before the phase it actually means to test. This is a NAMED, loud opt-out: one line
    // saying aerc was not installed and why, never a silent skip, and it never fires
    // against a real release (where no caller has reason to set it).
    if nonempty_env("SPIRA_INSTALL_AERC_CONSIDERED").is_some() {
        info("aerc NOT installed — SPIRA_INSTALL_AERC_CONSIDERED is set (a fixture with no release-vendored binary); aerc will not run until install runs without it");
        return Ok(());
    }
    let Some(release_root) = release_root else {
        return Err(format!(
            "cannot install aerc {AERC_VERSION}: this process is not running from a built release (no ancestor holds both bin/ and spira/conf.sh), so there is no {AERC_VENDORED_REL} to copy — build a release tarball first (bash spira/build-tarball.sh build --workspace <repo>), which builds aerc {AERC_VERSION} once and ships it there; install never builds it"
        ));
    };
    let src = release_root.join(AERC_VENDORED_REL);
    if !src.is_file() {
        return Err(format!(
            "release at {} does not carry {AERC_VENDORED_REL} — rebuild its tarball with spira/build-tarball.sh, which builds aerc {AERC_VERSION} once at release-build time and ships the binary there; install never builds it",
            release_root.display()
        ));
    }
    if dry {
        would(&format!("copy aerc {AERC_VERSION} from {} to {}", src.display(), dest.display()));
        return Ok(());
    }
    std::fs::create_dir_all(bin).map_err(|e| format!("cannot create {}: {e}", bin.display()))?;
    let tmp = bin.join(".aerc.new");
    std::fs::copy(&src, &tmp).map_err(|e| format!("cannot copy {} to {}: {e}", src.display(), tmp.display()))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| format!("chmod aerc: {e}"))?;
    std::fs::rename(&tmp, &dest).map_err(|e| format!("cannot install {}: {e}", dest.display()))?;
    let verify = Command::new("timeout")
        .args(["5", dest.to_str().unwrap_or("aerc"), "-v"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    if !aerc_reports_version(&verify, AERC_VERSION) {
        return Err(format!("installed {} does not report version {AERC_VERSION}", dest.display()));
    }
    info(&format!("installed aerc {AERC_VERSION} (copied from the release) at {}", dest.display()));
    Ok(())
}

fn info(s: &str) {
    println!("  {s}");
}
fn skip(s: &str) {
    println!("  already done: {s}");
}
fn would(s: &str) {
    println!("  would: {s}");
}

/// Run a bare-name tool, inheriting stdio, returning its exit code (127 on a spawn failure).
fn tool_status(name: &str, args: &[&str]) -> i32 {
    // batch-job: this runs whatever its caller names, as long as that takes
    Command::new(name).args(args).status().map(|s| s.code().unwrap_or(1)).unwrap_or(127)
}

fn tool_output(name: &str, args: &[&str], extra_env: &[(&str, &str)]) -> (i32, String) {
    // batch-job: this runs whatever its caller names, as long as that takes
    let mut c = Command::new(name);
    c.args(args);
    for (k, v) in extra_env {
        c.env(k, v);
    }
    match c.output() {
        Ok(o) => (o.status.code().unwrap_or(1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
        Err(e) => (127, format!("cannot run {name}: {e}")),
    }
}

/// `spira.instance` from the config alone — `SPIRA_INSTANCE` removed from the environment it is
/// resolved against, since the override would otherwise answer for the file. `None` while no
/// config file exists yet.
fn configured_instance() -> Option<String> {
    let spec = spira_config::process::spec().ok()?;
    if !spec.split(':').any(|p| Path::new(p).is_file()) {
        return None;
    }
    let mut env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    env.remove("SPIRA_INSTANCE");
    let home = spira_config::resolve::locate_home_for_process().ok()?;
    spira_config::resolve::resolve_instance(&env, &home).ok()
}

fn refuse_instance(c: &install::guards::Conflict) -> ExitCode {
    eprintln!("install: {}", c.message);
    eprintln!("install:   remedy: {}", c.remedy);
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("install: {e}");
            return ExitCode::from(2);
        }
    };
    let requested = opts.instance.clone().or_else(|| nonempty_env("SPIRA_INSTANCE"));
    let ephemeral = opts.ephemeral && requested.is_none();
    if let Some(p) = nonempty_env("CONFIGURE_PROD") {
        std::env::set_var("SPIRA_PROD", p);
    }
    let mut instance = if ephemeral {
        format!("eph-{}", std::process::id())
    } else {
        match install::guards::settle_instance(requested.as_deref(), configured_instance().as_deref()) {
            Ok(i) => i,
            Err(c) => return refuse_instance(&c),
        }
    };
    std::env::set_var("SPIRA_INSTANCE", &instance);

    if opts.system_user {
        return standalone_system_user(&instance, opts.dry);
    }
    if opts.populate_lifecycle {
        phase("phase 4.6: lifecycle population");
        return match populate_phase(opts.dry) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("install: phase lifecycle population failed — {e}");
                ExitCode::from(2)
            }
        };
    }

    // ---- phase -1: dependencies ----------------------------------------------------------
    // Install provides what its own doctor demands BEFORE the preflight judges the host
    // (law-install-installs-every-dependency, sp-k0n0j): a fresh host used to fail preflight on
    // two things install could have supplied — dolt's telemetry flag and duckdb.
    phase("phase -1: dependencies");
    if let Err(e) = dependencies(opts.dry) {
        eprintln!("install: {e}");
        return ExitCode::from(1);
    }

    // ---- phase 0: preflight -------------------------------------------------------------
    phase("phase 0: preflight");
    // doctor is a compiled binary now (sp-yyk47, merged on top of this crate's own
    // sp-31dm0) — was doctor.sh.
    let (rc, out) = tool_output("doctor", &[], &[("SPIRA_DOCTOR_INSTALLING", "1")]);
    for l in out.lines() {
        println!("  {l}");
    }
    if rc != 0 {
        eprintln!("\ninstall: preflight failed — see doctor output above");
        return ExitCode::from(1);
    }
    info("preflight passed");

    // ---- phase 0.5: conflicts -------------------------------------------------------------
    if nonempty_env("SPIRA_INSTALL_CONFLICT_CONSIDERED").is_none() {
        phase("phase 0.5: conflict checks");
        if let Err(c) = conflicts(&instance) {
            eprintln!("install: CONFLICT — {}", c.message);
            eprintln!("install:   remedy: {}", c.remedy);
            eprintln!("install:   override: SPIRA_INSTALL_CONFLICT_CONSIDERED=1");
            return ExitCode::from(5);
        }
        info("conflict checks clear");
    }

    if let Some(p) = nonempty_env("CONFIGURE_PROD") {
        let has_conf = Path::new(&p).join("conf.sh").is_file();
        if let Err(e) = install::guards::configure_prod_guard(&p, has_conf) {
            eprintln!("install: {e}");
            return ExitCode::from(1);
        }
    }

    // ---- phase 1: config --------------------------------------------------------------
    phase("phase 1: config");
    // The one config (per Ryan 2026-10-07): the file SPIRA_TOML names — normally
    // produced by `release install-tarball` from the operator's answers. One already there is
    // validated and used, never overwritten; a missing one is produced here (asked for on a
    // terminal; with no terminal the refusal names each missing input).
    let mut changes = 0u32;
    if opts.dry {
        would("ensure the config SPIRA_TOML names (spira-config init)");
    } else {
        let conf_d = match spira_config::resolve::locate_home_for_process() {
            Ok(h) => spira_config::resolve::default_conf_d(&h),
            Err(e) => {
                eprintln!("install: phase config failed — {e}");
                return ExitCode::from(2);
            }
        };
        match spira_config::init::ensure_from_cli(None, None, &Default::default(), &conf_d) {
            Ok(spira_config::init::Outcome::Existing(p)) => skip(&format!("config exists at {}", p.display())),
            Ok(spira_config::init::Outcome::Written(p)) => {
                info(&format!("wrote {}", p.display()));
                changes += 1;
            }
            Err(e) => {
                eprintln!("install: phase config failed — {e}");
                return ExitCode::from(2);
            }
        }
    }

    if !ephemeral {
        match install::guards::settle_instance(requested.as_deref(), configured_instance().as_deref()) {
            Ok(i) => {
                instance = i;
                std::env::set_var("SPIRA_INSTANCE", &instance);
            }
            Err(c) => return refuse_instance(&c),
        }
    }

    // Resolve the locations the rest of install reads, now that the config exists. They
    // were read straight from this process's environment, which a fresh host never sets,
    // so the database phase initialised "" (sp-xbxcb). install.sh used to source conf.sh here.
    for (key, required) in [("SPIRA_DB", true), ("SPIRA_RUN", true), ("SPIRA_DOLT_DATA", false), ("SPIRA_RELEASES", false)] {
        if nonempty_env(key).is_some() {
            continue;
        }
        match spira_config::resolve::key_for_process(key) {
            Ok(v) if !v.trim().is_empty() => {
                info(&format!("{key} = {v} (resolved from config)"));
                std::env::set_var(key, v);
            }
            Ok(_) | Err(_) if !required => {}
            Ok(_) => {
                eprintln!("install: {key} resolved empty — refusing to continue");
                return ExitCode::from(2);
            }
            Err(e) => {
                eprintln!("install: cannot resolve {key}: {e}");
                return ExitCode::from(2);
            }
        }
    }

    // ---- phase 1.5: same-user spira_lc credential ---------------------------------------
    phase("phase 1.5: spira-lc same-user credential");
    {
        let cred = same_user_credential_path();
        for path in [cred.clone(), format!("{cred}-ro")] {
            if Path::new(&path).is_file() {
                skip(&format!("{path} already exists"));
            } else if opts.dry {
                would(&format!("would create {path} (0600)"));
            } else if let Err(e) = create_same_user_credential(&path) {
                eprintln!("install: phase credential failed — {e}");
                return ExitCode::from(2);
            } else {
                info(&format!("create {path}"));
            }
        }
    }

    // ---- phase 2: build -----------------------------------------------------------------
    phase("phase 2: build");
    if opts.skip_build || opts.ephemeral {
        skip("build skipped (--skip-build or --ephemeral)");
    } else if opts.dry {
        would("run: spira/build.sh");
    } else {
        info("running build.sh");
        if tool_status("build.sh", &[]) != 0 {
            eprintln!("install: phase build failed — build.sh exited non-zero");
            return ExitCode::from(2);
        }
    }

    // ---- phase 3: database --------------------------------------------------------------
    phase("phase 3: database");
    let db = nonempty_env("SPIRA_DB").unwrap_or_default();
    let dolt_data = nonempty_env("SPIRA_DOLT_DATA");
    {
        let own_remotes = git_remotes(&db.to_string());
        let ancestor_git = ancestor_git_dir(Path::new(&db).parent());
        if let Err(e) = install::guards::db_git_guard(&db, &own_remotes, ancestor_git.as_deref()) {
            eprintln!("install: {e}");
            return ExitCode::from(2);
        }
    }
    let mut seed_deferred = false;
    let mut db_fresh = false;
    let mut dolt_bg: Option<std::process::Child> = None;
    let mut dolt_port: u16 = 3307;
    if let Some(dd) = &dolt_data {
        if !Path::new(dd).is_dir() {
            if opts.dry {
                would(&format!("create Dolt data directory: {dd}"));
            } else {
                info(&format!("create Dolt data directory: {dd}"));
                let _ = std::fs::create_dir_all(dd);
                changes += 1;
            }
        } else {
            skip(&format!("Dolt data directory exists: {dd}"));
        }
        let yaml_dest = format!("{dd}/dolt-server.yaml");
        let yaml_tpl = format!("{}/dolt-server.yaml", bootstrap::templates_dir().display());
        if !Path::new(&yaml_dest).is_file() && Path::new(&yaml_tpl).is_file() {
            if opts.dry {
                would(&format!("write dolt-server.yaml to {yaml_dest}"));
            } else if let Ok(tpl) = std::fs::read_to_string(&yaml_tpl) {
                let _ = std::fs::write(&yaml_dest, tpl.replace("@SPIRA_DOLT_DATA@", dd));
                info(&format!("wrote dolt-server.yaml to {yaml_dest}"));
                changes += 1;
            }
        } else {
            skip(&format!("dolt-server.yaml already at {yaml_dest}"));
        }
        dolt_port = read_yaml_port(&yaml_dest).unwrap_or(3307);
    }

    if Path::new(&db).join(".beads").is_dir() {
        if let Ok(meta) = std::fs::read_to_string(Path::new(&db).join(".beads/metadata.json")) {
            if meta.contains("\"dolt_mode\"") && meta.contains("\"embedded\"") {
                eprintln!("install: phase database failed — existing store at {db} is embedded (one lock, all clients queue); set SPIRA_DOLT_DATA in spira.conf and re-run install");
                return ExitCode::from(2);
            }
        }
        // A bead count, not just an existence check (original: `bd -C $SPIRA_DB list --limit
        // 0 --json`, 10s timeout, length of the JSON array, "?" on any failure to parse or
        // to run at all). This ALSO proves the post-init skip check keeps -C — only the
        // fresh-init call below lost it.
        let bead_count = Command::new("timeout")
            .args(["5", "bd", "-C", &db, "list", "--limit", "0", "--json"])
            .output()
            .ok()
            .and_then(|o| o.status.success().then(|| o.stdout))
            .and_then(|out| serde_json::from_slice::<serde_json::Value>(&out).ok())
            .and_then(|v| v.as_array().map(|a| a.len().to_string()))
            .unwrap_or_else(|| "?".to_string());
        skip(&format!("database exists at {db} ({bead_count} bead(s))"));
    } else {
        db_fresh = true;
        if opts.dry {
            if dolt_data.is_some() {
                would(&format!("start dolt server and run: (cd {db} && bd init --server --external)"));
            } else {
                would(&format!("run: (cd {db} && bd init)"));
            }
        } else {
            info(&format!("initialising database at {db}"));
            let _ = std::fs::create_dir_all(&db);
            if let Some(_dd) = &dolt_data {
                if which_prog("dolt").is_none() {
                    eprintln!("install: phase database failed — dolt is not on PATH — required to init the database");
                    return ExitCode::from(2);
                }
                if !tcp_up(dolt_port) {
                    info(&format!("starting dolt server on port {dolt_port} for database init"));
                    let yaml = format!("{}/dolt-server.yaml", dolt_data.clone().unwrap());
                    // batch-job: child is spawned or exec-replaced, not awaited under a deadline
                    dolt_bg = Command::new("dolt").args(["sql-server", "--config", &yaml]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().ok();
                    if !wait_tcp(dolt_port, 30) {
                        if let Some(mut c) = dolt_bg.take() {
                            let _ = c.kill();
                        }
                        eprintln!("install: phase database failed — dolt server did not start on port {dolt_port} within 30s");
                        return ExitCode::from(2);
                    }
                }
                let ready_max: u64 = nonempty_env("SPIRA_INSTALL_DOLT_READY_WAIT").and_then(|v| v.parse().ok()).unwrap_or(120);
                if !wait_dolt_query(dolt_port, ready_max) {
                    if let Some(mut c) = dolt_bg.take() {
                        let _ = c.kill();
                    }
                    eprintln!("install: phase database failed — dolt server on port {dolt_port} did not answer queries within {ready_max}s");
                    return ExitCode::from(2);
                }
                let dbname = Path::new(&db).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let max_tries: u32 = nonempty_env("SPIRA_INSTALL_BD_INIT_TRIES").and_then(|v| v.parse().ok()).unwrap_or(3).max(1);
                let mut tries = 0;
                loop {
                    tries += 1;
                    let (rc, out) = bd_output(&db, &["init", "--non-interactive", "--prefix", "sp", "--skip-agents", "--skip-hooks", "--server", "--server-host", "127.0.0.1", "--server-port", &dolt_port.to_string(), "--database", &dbname, "--external", "-q"]);
                    if rc == 0 {
                        break;
                    }
                    clean_partial_init(&db, &dbname, dolt_port);
                    if tries < max_tries && out.contains("invalid connection") {
                        info("  bd init hit an invalid connection — removed the partial database, retrying");
                        std::thread::sleep(std::time::Duration::from_secs(2 * tries as u64));
                        continue;
                    }
                    eprint!("{out}");
                    if let Some(mut c) = dolt_bg.take() {
                        let _ = c.kill();
                    }
                    eprintln!("install: phase database failed — bd init (server mode) failed");
                    return ExitCode::from(2);
                }
                let _ = spira_config::bounded::bounded("git").args(["-C", &db, "config", "beads.role", "maintainer"]).status();
            } else {
                // cwd == SPIRA_DB, not -C: see bd_output's doc comment above for why.
                if bd_init_bounded().current_dir(&db).arg("init").status().map(|s| s.success()).unwrap_or(false) != true {
                    eprintln!("install: phase database failed — bd init failed");
                    return ExitCode::from(2);
                }
                let _ = spira_config::bounded::bounded("git").args(["-C", &db, "config", "beads.role", "maintainer"]).status();
            }
            changes += 1;
        }
    }

    // bd reads beads.role from the cwd's git config, not from -C; every caller runs from some
    // other checkout, so only the user-global value reaches all of them.
    let role_set = spira_config::bounded::bounded("git").args(["config", "--global", "--get", "beads.role"]).output().map(|o| o.status.success() && !o.stdout.is_empty()).unwrap_or(false);
    if role_set {
        skip("beads.role already set (git config --global)");
    } else if opts.dry {
        would("run: git config --global beads.role maintainer");
    } else {
        if spira_config::bounded::bounded("git").args(["config", "--global", "beads.role", "maintainer"]).status().map(|s| s.success()).unwrap_or(false) {
            info("set beads.role maintainer (git config --global)");
            changes += 1;
        } else {
            eprintln!("install: could not set beads.role — run: git config --global beads.role maintainer");
        }
    }

    let metrics_off = spira_config::bounded::bounded("dolt").args(["config", "--global", "--get", "metrics.disabled"]).output().map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true").unwrap_or(false);
    if metrics_off {
        skip("dolt metrics.disabled already true");
    } else if opts.dry {
        would("run: dolt config --global --add metrics.disabled true");
    } else if spira_config::bounded::bounded("dolt").args(["config", "--global", "--add", "metrics.disabled", "true"]).status().map(|s| s.success()).unwrap_or(false) {
        info("set dolt metrics.disabled true (dolt config --global)");
        changes += 1;
    } else {
        eprintln!("install: could not set metrics.disabled — run: dolt config --global --add metrics.disabled true");
    }

    if Path::new(&db).join(".beads").is_dir() || !opts.dry {
        let server_mode = dolt_data.is_some();
        let server_up = server_mode && tcp_up(dolt_port);
        if opts.dry {
            would("run: spira/seed.sh (skips statutes already in force)");
        } else if install::guards::seed_when(db_fresh, server_mode, server_up) == install::guards::SeedWhen::Defer {
            seed_deferred = true;
            info("seeding statutes after phase 4 — the database server is not running yet");
        } else {
            info("seeding statutes");
            let (rc, out) = tool_output("seed.sh", &[], &[]);
            print!("{out}");
            if let Some(mut c) = dolt_bg.take() {
                let _ = c.kill();
            }
            if dolt_data.is_some() {
                let close_wait: u64 = nonempty_env("SPIRA_INSTALL_DOLT_CLOSE_WAIT").and_then(|v| v.parse().ok()).unwrap_or(30);
                wait_tcp_down(dolt_port, close_wait);
            }
            if rc != 0 && db_fresh {
                eprintln!("install: phase database failed — seed.sh failed — statutes not seeded on fresh database");
                return ExitCode::from(2);
            }
        }
    }

    // ---- phase 4: units -------------------------------------------------------------------
    phase("phase 4: units");
    let prod = nonempty_env("SPIRA_PROD").unwrap_or_default();
    if nonempty_env("SPIRA_INSTALL_PROD_GIT_CONSIDERED").is_none() {
        let is_git = is_git_checkout(&prod);
        let releases = nonempty_env("SPIRA_RELEASES").unwrap_or_default();
        if let Err(e) = install::guards::prod_guard(&prod, is_git, false, &releases) {
            eprintln!("install: {e}");
            return ExitCode::from(2);
        }
    }
    if opts.dry {
        would("run: mail ensure operator");
    } else if tool_status("mail", &["ensure", "operator"]) != 0 {
        eprintln!("install: phase units failed — could not create the operator mailbox (mail ensure operator)");
        return ExitCode::from(2);
    }
    if !opts.dry {
        if let Err(e) = bootstrap::ensure_reader_mailboxes() {
            eprintln!("install: phase units failed — {e}");
            return ExitCode::from(2);
        }
    }

    let manifest = match bootstrap::manifest_from_env(&instance) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("install: {e}");
            return ExitCode::from(2);
        }
    };
    let host = match bootstrap::host_from_env(&instance) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("install: {e}");
            return ExitCode::from(2);
        }
    };
    let systemctl = RealSystemctl::from_env();

    if nonempty_env("SPIRA_INSTALL_FORCE").is_none() {
        if let Err(lines) = checks::preflight(&instance, &host, &systemctl) {
            for l in lines {
                eprintln!("install: {l}");
            }
            return ExitCode::from(2);
        }
    }

    if opts.dry {
        let dir = bootstrap::unit_dir();
        let templates_dir = bootstrap::templates_dir();
        let suspended = bootstrap::suspended_set();
        let no = |s: &str| suspended.contains(s);
        let ctx = Ctx { unit_dir: &dir, templates_dir: &templates_dir, host: &host, manifest: &manifest, systemctl: &systemctl, suspended: &no, world_halted: bootstrap::world_halted(), skip_migrate_watchers: false };
        match install_units::diff(&ctx) {
            Ok(lines) if lines.is_empty() => skip("all units match what would be rendered"),
            Ok(lines) => {
                for l in lines {
                    println!("  {l}");
                }
            }
            Err(e) => println!("  {e}"),
        }
    } else {
        info(&format!("installing units for instance '{instance}'"));
        let dir = bootstrap::unit_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Some(run) = nonempty_env("SPIRA_RUN") {
            let _ = std::fs::create_dir_all(&run);
            let _ = std::fs::create_dir_all(Path::new(&run).join("watchd"));
        }
        let templates_dir = bootstrap::templates_dir();
        let suspended = bootstrap::suspended_set();
        let no = |s: &str| suspended.contains(s);
        let ctx = Ctx { unit_dir: &dir, templates_dir: &templates_dir, host: &host, manifest: &manifest, systemctl: &systemctl, suspended: &no, world_halted: bootstrap::world_halted(), skip_migrate_watchers: false };
        let report = install_units::run(&ctx);
        for e in &report.errors {
            eprintln!("install: {e}");
        }
        if !report.errors.is_empty() {
            eprintln!("install: phase units failed");
            return ExitCode::from(2);
        }
        for u in &report.written {
            println!("installed {u}");
        }
        changes += 1;
        if let Some(dd) = &dolt_data {
            let bp = read_yaml_port(&format!("{dd}/dolt-server.yaml")).unwrap_or(3307);
            info(&format!("waiting for dolt-beads.service on port {bp}"));
            let pwt: u64 = nonempty_env("SPIRA_INSTALL_DOLT_WAIT").and_then(|v| v.parse().ok()).unwrap_or(60);
            if !wait_tcp(bp, pwt) {
                eprintln!("install: phase units failed — dolt-beads.service is active but not listening on {bp} after {pwt}s");
                return ExitCode::from(2);
            }
            info(&format!("dolt-beads.service listening on port {bp}"));
            let _ = spira_config::bounded::bounded("bd").args(["-C", &db, "doctor", "--fix", "--yes"]).env("BD_NON_INTERACTIVE", "1").status();
            let db_max: u64 = nonempty_env("SPIRA_INSTALL_DB_WAIT").and_then(|v| v.parse().ok()).unwrap_or(30);
            if !wait_bd_list(&db, db_max) {
                eprintln!("install: phase units failed — bd did not accept connections within {db_max}s after dolt-beads.service started");
                return ExitCode::from(2);
            }
            info("bd store accepting connections");
        }
    }

    if seed_deferred {
        info("seeding statutes (deferred from phase 3 — the database server is up now)");
        let (rc, out) = tool_output("seed.sh", &[], &[]);
        print!("{out}");
        if rc != 0 {
            eprintln!("install: phase units failed — seed.sh failed with the database server running");
            return ExitCode::from(2);
        }
    }

    // ---- phase 4.5: lifecycle store -------------------------------------------------------
    // sp-xfqnr: a fresh same-user install never built spira_lifecycle, so an aeon was
    // summoned but could not claim or submit, and `spira-lc history` exited 2. The lifecycle
    // machine is the only mode (sp-v62vn): there is no switch to turn on afterwards. Here,
    // after phase 4 has dolt-beads.service listening: schema, migrations, grants —
    // idempotent, and fatal when it cannot be done (no silent skip).
    phase("phase 4.5: lifecycle store");
    if spira_config::resolve::lc_system_mode() {
        skip("spira-lc runs as a system service (--system-user) — its store and grants are spira/cutover-deploy.sh's, with the credentials under /etc/spira-lc");
    } else if nonempty_env("SPIRA_INSTALL_LC_STORE_CONSIDERED").is_some() {
        info("lifecycle store NOT built — SPIRA_INSTALL_LC_STORE_CONSIDERED is set (a fixture with no real Dolt server); spira-lc will not answer until install runs without it");
    } else if opts.dry {
        would("apply lifecycle/schema.sql, lifecycle/migrations/*.sql (spira-lc admin-migrate) and lifecycle/grants.sql as the Dolt admin");
        if let Err(e) = populate_phase(true) {
            eprintln!("install: phase lifecycle population failed — {e}");
            return ExitCode::from(2);
        }
    } else {
        let port = lc_port(dolt_data.as_deref());
        match lifecycle_store_phase(port) {
            Ok(lines) => {
                for l in lines {
                    info(&l);
                }
                changes += 1;
                // ---- phase 4.6: lifecycle population (sp-k62xz8) --------------------------
                // The store now exists and its user can write: every bead already in the
                // beads database gets its row, once — before lc-serve starts answering for it.
                phase("phase 4.6: lifecycle population");
                if let Err(e) = populate_phase(false) {
                    eprintln!("install: phase lifecycle population failed — {e}");
                    return ExitCode::from(2);
                }
                // lc-serve.service was enabled in phase 4, before this store existed, and may
                // have given up (StartLimitBurst): restart it now and require it to be active —
                // fail closed, since an aeon cannot claim without it (sp-xfqnr).
                // batch-job: one restart of a service during install; bounded by timeout 30.
                let _ = Command::new("timeout").args(["30", "systemctl", "--user", "restart", "lc-serve.service"]).status();
                let mut active = false;
                for _ in 0..20 {
                    active = Command::new("timeout")
                        .args(["5", "systemctl", "--user", "is-active", "--quiet", "lc-serve.service"])
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false);
                    if active {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
                if !active {
                    eprintln!("install: phase lifecycle store failed — lc-serve.service is not active after the store was built (journalctl --user -u lc-serve.service)");
                    return ExitCode::from(2);
                }
                info("lc-serve.service active on the lifecycle store");
            }
            Err(e) => {
                eprintln!("install: phase lifecycle store failed — {e}");
                return ExitCode::from(2);
            }
        }
    }

    // Linger.
    let linger_user = nonempty_env("USER").unwrap_or_else(whoami);
    let cur_linger = spira_config::bounded::bounded("loginctl").args(["show-user", &linger_user, "-p", "Linger"]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    if cur_linger == "Linger=yes" {
        skip(&format!("linger already enabled for {linger_user}"));
    } else if opts.dry {
        would(&format!("run: loginctl enable-linger {linger_user}"));
    } else {
        let _ = spira_config::bounded::bounded("loginctl").args(["enable-linger", &linger_user]).status();
        if let Some(run) = nonempty_env("SPIRA_RUN") {
            let _ = std::fs::create_dir_all(&run);
            let _ = std::fs::write(Path::new(&run).join("install-linger-enabled"), "");
        }
        info(&format!("enabled linger for {linger_user}"));
        changes += 1;
    }

    // ---- phase 5: hooks -------------------------------------------------------------------
    phase("phase 5: hooks");
    let repo = nonempty_env("SPIRA_REPO").unwrap_or_default();
    if Path::new(&repo).join(".git").is_dir() || Path::new(&repo).join(".git").is_file() {
        if opts.dry {
            would(&format!("run: spira/exclude.sh install {repo}"));
        } else {
            let (rc, out) = tool_output("exclude.sh", &["install", &repo], &[]);
            print!("{out}");
            if rc != 0 {
                eprintln!("install: phase hooks failed — exclude.sh install failed — core.hooksPath not set");
                return ExitCode::from(2);
            }
        }
    } else {
        skip(&format!("no git checkout at {repo} — no commit hooks to arm"));
    }

    if opts.ephemeral || opts.no_session_hook {
        skip("session hook skipped (--ephemeral or --no-session-hook)");
    } else if opts.dry {
        would("run: release session-hook install");
    } else {
        info("installing session hook");
        if tool_status("release", &["session-hook", "install"]) != 0 {
            eprintln!("install: phase hooks failed — release session-hook install failed");
            return ExitCode::from(2);
        }
        changes += 1;
    }

    if let Some(glob) = nonempty_env("SPIRA_ALERT_GLOB") {
        if opts.ephemeral {
            skip("alert intake skipped (--ephemeral)");
        } else if opts.dry {
            would(&format!("run: release intake install (SPIRA_ALERT_GLOB={glob})"));
        } else {
            info(&format!("wiring alert intake for SPIRA_ALERT_GLOB={glob}"));
            let (_, out) = tool_output("release", &["intake", "install"], &[]);
            print!("{out}");
        }
    } else {
        skip("alert intake skipped (SPIRA_ALERT_GLOB not set or --ephemeral)");
    }

    // ---- phase 6: cockpit -------------------------------------------------------------------
    phase("phase 6: cockpit");
    let home = nonempty_env("SPIRA_HOME").unwrap_or_default();
    let cockpit_dir = Path::new(&home).parent().map(|p| p.join("cockpit")).unwrap_or_default();
    let bin_dir = nonempty_env("HOME").map(|h| PathBuf::from(h).join(".local/bin")).unwrap_or_default();
    for prog in ["cockpit", "cockpit-remote"] {
        let src = cockpit_dir.join(prog);
        let link = bin_dir.join(prog);
        if !src.is_file() {
            skip(&format!("{prog} not found at {} — skipping", src.display()));
            continue;
        }
        if std::fs::read_link(&link).map(|t| t == src).unwrap_or(false) {
            skip(&format!("{} -> {}", link.display(), src.display()));
            continue;
        }
        if opts.dry {
            would(&format!("link: {} -> {}", link.display(), src.display()));
        } else {
            let _ = std::fs::create_dir_all(&bin_dir);
            let _ = std::fs::remove_file(&link);
            if std::os::unix::fs::symlink(&src, &link).is_ok() {
                info(&format!("linked {} -> {}", link.display(), src.display()));
                changes += 1;
            }
        }
    }
    if which_prog("tmux").is_some() && spira_config::bounded::bounded("tmux").args(["list-panes", "-a"]).output().map(|o| o.status.success()).unwrap_or(false) {
        let panel = spira_config::bounded::bounded("tmux").args(["list-panes", "-a", "-F", "#{@cockpit}"]).output().map(|o| String::from_utf8_lossy(&o.stdout).lines().filter(|l| *l == "panel").count()).unwrap_or(0);
        if panel > 0 {
            skip("cockpit panes already present");
        } else if opts.dry {
            would("run: layout up");
        } else {
            info("building cockpit panes");
            let _ = tool_status("layout", &["up"]);
            changes += 1;
        }
    } else {
        info("tmux server not reachable — build the cockpit when ready:");
        info("  layout up");
    }

    // ---- phase 7: verify --------------------------------------------------------------------
    phase("phase 7: verify");
    if opts.dry {
        if changes > 0 {
            info(&format!("dry-run complete — {changes} change(s) would be made"));
        } else {
            info("dry-run complete — no changes needed");
        }
        info("running ready.sh in read-only mode to show current state");
        let (_, out) = tool_output("ready.sh", &[], &[]);
        for l in out.lines() {
            println!("  {l}");
        }
        return ExitCode::SUCCESS;
    }

    println!();
    let ready_rc = tool_status("ready.sh", &[]);
    if ready_rc == 0 {
        println!("\ninstall: done — Spira is ready.");
        ExitCode::SUCCESS
    } else {
        eprintln!("\ninstall: installation complete but ready.sh exited {ready_rc} — installed but NOT ready");
        eprintln!("install: see the output above for what is preventing the loop from receiving work");
        ExitCode::from(3)
    }
}

fn whoami() -> String {
    spira_config::bounded::bounded("id").arg("-un").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

fn which_prog(p: &str) -> Option<String> {
    bootstrap::which(p)
}

fn is_root() -> bool {
    spira_config::bounded::bounded("id").arg("-u").output().map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0").unwrap_or(false)
}

fn is_git_checkout(mut path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    loop {
        if Path::new(path).join(".git").exists() {
            return true;
        }
        let Some(parent) = Path::new(path).parent() else { return false };
        if parent.as_os_str().is_empty() || parent == Path::new("/") {
            return false;
        }
        path = parent.to_str().unwrap_or("");
        if path.is_empty() {
            return false;
        }
    }
}

fn git_remotes(db: &str) -> Vec<String> {
    if !Path::new(db).join(".git").exists() {
        return Vec::new();
    }
    // batch-job: git history or network operation, as long as the repository is large
    Command::new("git").args(["-C", db, "remote"]).output().map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect()).unwrap_or_default()
}

fn ancestor_git_dir(mut dir: Option<&Path>) -> Option<String> {
    while let Some(d) = dir {
        if d.join(".git").exists() {
            return Some(d.to_string_lossy().to_string());
        }
        dir = d.parent();
    }
    None
}

fn read_yaml_port(path: &str) -> Option<u16> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("port:") {
            return rest.trim().parse().ok();
        }
    }
    None
}

fn tcp_up(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port)), std::time::Duration::from_secs(1)).is_ok()
}

fn clean_partial_init(db: &str, dbname: &str, port: u16) {
    let _ = std::fs::remove_dir_all(Path::new(db).join(".beads"));
    let _ = std::fs::remove_dir_all(Path::new(db).join(".dolt"));
    if dbname.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') && !dbname.is_empty() {
        // batch-job: dropping the partial database; bounded at 30 s.
        let _ = Command::new("timeout").args(["30", "dolt", "--host", "127.0.0.1", "--port", &port.to_string(), "--no-tls", "--user", "root", "--password", "", "sql", "-q", &format!("DROP DATABASE IF EXISTS `{dbname}`")]).stdin(Stdio::null()).output();
    }
}

fn wait_tcp(port: u16, max_secs: u64) -> bool {
    let start = std::time::Instant::now();
    loop {
        if tcp_up(port) {
            return true;
        }
        if start.elapsed().as_secs() >= max_secs {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn wait_tcp_down(port: u16, max_secs: u64) {
    let start = std::time::Instant::now();
    while tcp_up(port) && start.elapsed().as_secs() < max_secs {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn wait_dolt_query(port: u16, max_secs: u64) -> bool {
    let start = std::time::Instant::now();
    loop {
        let ok = spira_config::bounded::bounded("dolt").args(["--host", "127.0.0.1", "--port", &port.to_string(), "--no-tls", "--user", "root", "--password", "", "sql", "-q", "select 1"]).stdin(Stdio::null()).output().map(|o| o.status.success()).unwrap_or(false);
        if ok {
            return true;
        }
        if start.elapsed().as_secs() >= max_secs {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn wait_bd_list(db: &str, max_secs: u64) -> bool {
    let start = std::time::Instant::now();
    let mut attempt: u64 = 0;
    loop {
        let ok = spira_config::bounded::bounded("bd").args(["-C", db, "list", "--json"]).env("BD_NON_INTERACTIVE", "1").output().map(|o| o.status.success()).unwrap_or(false);
        if ok {
            return true;
        }
        if start.elapsed().as_secs() >= max_secs {
            return false;
        }
        attempt += 1;
        info(&format!("  db probe: attempt {attempt} of {max_secs}: retrying"));
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// `bd`, bounded for `bd init`: creating a fresh database's schema takes far longer than the
/// 5 s a call that must merely answer gets — under that bound `bd init` was killed mid-schema
/// on every fresh install ("failed to initialize schema: context canceled").
const BD_INIT_SECS: &str = "300";
fn bd_init_bounded() -> Command {
    let mut c = Command::new("timeout");
    c.arg(BD_INIT_SECS).arg("bd");
    c
}

/// Runs `bd <args>` with `db` as cwd, not `-C db` — `bd init`'s own remote-less repository
/// is allowed (db_git_guard, above), and `-C` makes a fresh `bd init` look inside a
/// directory that does not have a beads project yet, which is exactly what `bd` refuses
/// (original: `( cd "$SPIRA_DB" && ... bd init ... )`, install.sh lines ~639/~660 before
/// retirement). Only the init call needs this; every other `bd` call in this binary keeps
/// `-C` for the already-initialised database it is allowed to name from outside.
fn bd_output(db: &str, args: &[&str]) -> (i32, String) {
    let mut c = bd_init_bounded();
    c.current_dir(db).args(args).env("BD_NON_INTERACTIVE", "1");
    match c.output() {
        Ok(o) => (o.status.code().unwrap_or(1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
        Err(e) => (127, format!("cannot run bd: {e}")),
    }
}

/// Conflict checks 1–5 (root `install.sh` phase 0.5), resolved with real `/proc`, `flock` and
/// a TCP probe, decided by [`install::guards`].
fn conflicts(instance: &str) -> Result<(), install::guards::Conflict> {
    use install::guards::*;
    let home = nonempty_env("SPIRA_HOME").unwrap_or_default();
    let unit_dir = bootstrap::unit_dir();
    let our_dir = Path::new(&home).canonicalize().map(|p| p.to_string_lossy().to_string()).unwrap_or(home.clone());

    // Conflict 1: a foreign harness copy. Tried against SPIRA_HOME first, then against
    // dirname(SPIRA_PROD or SPIRA_HOME)/bin — an older, cutover-era unit ExecStarts
    // $SPIRA_HOME/sentinel.sh directly, while a current one ExecStarts the release's
    // bin/sentinel; comparing only SPIRA_HOME refused every reinstall of a cutover install
    // as foreign (install.sh's own comment, preserved here: either match clears it).
    let sentinel_file = unit_dir.join(format!("spira-sentinel-{instance}.service"));
    let installed_exec_dir = std::fs::read_to_string(&sentinel_file).ok().and_then(|t| {
        t.lines().find_map(|l| l.strip_prefix("ExecStart=")).map(|v| v.split_whitespace().next().unwrap_or("").to_string()).and_then(|exe| Path::new(&exe).parent().map(|p| p.to_string_lossy().to_string())).and_then(|d| Path::new(&d).canonicalize().ok()).map(|p| p.to_string_lossy().to_string())
    });
    let prod = nonempty_env("SPIRA_PROD").unwrap_or_else(|| home.clone());
    let bin_dir = Path::new(&prod).parent().map(|p| p.join("bin")).map(|p| p.to_string_lossy().to_string()).map(|p| Path::new(&p).canonicalize().map(|c| c.to_string_lossy().to_string()).unwrap_or(p)).unwrap_or_default();
    if conflict_foreign(instance, installed_exec_dir.as_deref(), &our_dir).is_err() {
        conflict_foreign(instance, installed_exec_dir.as_deref(), &bin_dir)?;
    }

    // Conflict 2: a live aeon under this installation.
    let cmdlines = proc_cmdlines();
    conflict_aeon(&home, cmdlines.iter().map(|(p, c)| (p.as_str(), c.as_str())))?;

    // Conflict 3: the landing gate's tree lock.
    let repo_base = nonempty_env("SPIRA_REPO").map(|r| Path::new(&r).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()).unwrap_or_default();
    let run = nonempty_env("SPIRA_RUN").unwrap_or_default();
    let lock = format!("{run}/worktree/.gate.{repo_base}.lock");
    let held = Path::new(&lock).is_file() && !flock_free(&lock);
    conflict_lock(&lock, held)?;

    // Conflict 4: instance argument / run-dir collision.
    let conf_file = nonempty_env("SPIRA_CONF_FILE").unwrap_or_default();
    let conf_instance = std::fs::read_to_string(&conf_file).ok().and_then(|t| t.lines().find_map(|l| l.trim_start().strip_prefix("SPIRA_INSTANCE").map(str::to_string))).and_then(|l| l.split('=').nth(1).map(|v| v.trim().trim_matches('"').trim_matches('\'').to_string()));
    conflict_instance_arg(instance, conf_instance.as_deref(), &conf_file)?;
    let our_unit = format!("spira-sentinel-{instance}.service");
    let mut others = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&unit_dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with("spira-sentinel-") && n.ends_with(".service") && n != our_unit {
                if let Ok(t) = std::fs::read_to_string(e.path()) {
                    if let Some(rd_line) = t.lines().find_map(|l| l.strip_prefix("StandardOutput=append:").or_else(|| l.strip_prefix("StandardError=append:"))) {
                        if let Some(dir) = Path::new(rd_line).parent() {
                            let real = dir.canonicalize().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| dir.to_string_lossy().to_string());
                            others.push((n.clone(), real));
                        }
                    }
                }
            }
        }
    }
    let run_real = Path::new(&run).canonicalize().map(|p| p.to_string_lossy().to_string()).unwrap_or(run.clone());
    conflict_instance_run(&run_real, &our_unit, others.iter().map(|(a, b)| (a.as_str(), b.as_str())))?;

    // Conflict 5: a Dolt port collision.
    let dolt_data = nonempty_env("SPIRA_DOLT_DATA").unwrap_or_default();
    let port = read_yaml_port(&format!("{dolt_data}/dolt-server.yaml")).unwrap_or(3307);
    let listening = !dolt_data.is_empty() && tcp_up(port);
    let servers = dolt_servers();
    conflict_dolt(&dolt_data, port, listening, servers.iter().map(|(a, b)| (a.as_str(), b.as_str())))?;

    Ok(())
}

fn proc_cmdlines() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/proc") else { return out };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if let Ok(cmd) = std::fs::read(e.path().join("cmdline")) {
            out.push((name, String::from_utf8_lossy(&cmd).to_string()));
        }
    }
    out
}

fn flock_free(path: &str) -> bool {
    spira_config::bounded::bounded("flock").arg("-n").arg(path).arg("true").status().map(|s| s.success()).unwrap_or(true)
}

fn dolt_servers() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (_, cmd) in proc_cmdlines() {
        let args: Vec<&str> = cmd.split('\0').filter(|s| !s.is_empty()).collect();
        if !args.iter().any(|a| a.contains("sql-server")) {
            continue;
        }
        let mut data_dir = None;
        if let Some(i) = args.iter().position(|a| *a == "--data-dir") {
            data_dir = args.get(i + 1).map(|s| s.to_string());
        } else if let Some(i) = args.iter().position(|a| *a == "--config") {
            if let Some(cfg) = args.get(i + 1) {
                if let Ok(text) = std::fs::read_to_string(cfg) {
                    for l in text.lines() {
                        if let Some(v) = l.trim_start().strip_prefix("data_dir:") {
                            data_dir = Some(v.trim().trim_matches('"').to_string());
                        }
                    }
                }
            }
        }
        if let Some(d) = data_dir {
            let real = Path::new(&d).canonicalize().map(|p| p.to_string_lossy().to_string()).unwrap_or(d);
            out.push((cmd.clone(), real));
        }
    }
    out
}

/// Phase 6.5 (design §3.6.4): the one root-requiring step — a Unix user of its own for the
/// spira_lifecycle credential. Refuses rather than half-installing.
fn system_user_phase(host: &install::values::HostValues) -> Result<(), String> {
    let user = nonempty_env("SPIRA_LC_UNIX_USER").unwrap_or_else(|| "spira-lc".into());
    let group = nonempty_env("SPIRA_LC_UNIX_GROUP").unwrap_or_else(|| "spira".into());
    let home = host.home.as_str();

    let bin = format!("{}/bin/spira-lc", host.to_map()["SPIRA_PROD_ROOT"]);
    let group_gid = group_gid(&group);
    if let Some(dir) = install::guards::untraversable_ancestor(Path::new(&bin), group_gid) {
        return Err(format!(
            "{bin} sits under {}, which the service user {user} cannot traverse — install a release outside any home directory (or grant group {group} execute on it) before --system-user",
            dir.display()
        ));
    }

    if spira_config::bounded::bounded("getent").arg("group").arg(&group).status().map(|s| !s.success()).unwrap_or(true) {
        info(&format!("create group {group}"));
        run_ok("groupadd", &["--system", &group])?;
    }
    if spira_config::bounded::bounded("id").arg("-u").arg(&user).status().map(|s| !s.success()).unwrap_or(true) {
        info(&format!("create user {user} (system, no login, no home)"));
        run_ok("useradd", &["--system", "--no-create-home", "--shell", "/usr/sbin/nologin", "--gid", &group, &user])?;
    }

    let cred_dir = "/etc/spira-lc";
    let cred_file = format!("{cred_dir}/credential");
    if !Path::new(&cred_file).is_file() {
        info(&format!("create {cred_dir}"));
        run_ok("install", &["-d", "-m", "0750", "-o", &user, "-g", &group, cred_dir])?;
        write_random_credential(&cred_file, 0o600, Some((&user, &group)))?;
    } else {
        skip(&format!("{cred_file} already exists"));
    }
    let ro_file = format!("{cred_dir}/credential-ro");
    if !Path::new(&ro_file).is_file() {
        write_random_credential(&ro_file, 0o644, Some((&user, &group)))?;
    } else {
        skip(&format!("{ro_file} already exists"));
    }

    let host_values = host;
    for unit in ["spira-lc.service", "spira-lc.socket"] {
        let text = install::values::render_file(&Path::new(home).join("systemd").join(unit), host_values, None)?;
        let dest = format!("/etc/systemd/system/{unit}");
        std::fs::write(format!("{dest}.tmp"), text).map_err(|e| e.to_string())?;
        run_ok("install", &["-m", "0644", &format!("{dest}.tmp"), &dest])?;
        let _ = std::fs::remove_file(format!("{dest}.tmp"));
        info(&format!("install {dest}"));
    }
    run_ok("systemctl", &["daemon-reload"])?;
    info("reload systemd");
    run_ok("systemctl", &["enable", "--now", "spira-lc.socket"])?;
    info("enable spira-lc.socket (spira-lc.service itself is socket-activated, not enabled directly)");
    Ok(())
}

fn run_ok(prog: &str, args: &[&str]) -> Result<(), String> {
    // batch-job: this runs whatever its caller names, as long as that takes
    Command::new(prog).args(args).status().map_err(|e| format!("cannot run {prog}: {e}")).and_then(|s| if s.success() { Ok(()) } else { Err(format!("{prog} {} failed ({s})", args.join(" "))) })
}

fn standalone_system_user(instance: &str, dry: bool) -> ExitCode {
    phase("phase 6.5: spira-lc system user");
    let host = match bootstrap::host_from_env(instance) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("install: {e}");
            return ExitCode::from(1);
        }
    };
    if dry {
        would("would create the spira-lc system user, group and install spira-lc.service/.socket");
        return ExitCode::SUCCESS;
    }
    if !is_root() {
        eprintln!("install: --system-user requires root and runs only the system-user phase");
        return ExitCode::from(1);
    }
    match system_user_phase(&host) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("install: phase spira-lc failed — {e}");
            ExitCode::from(2)
        }
    }
}

fn group_gid(group: &str) -> Option<u32> {
    let out = spira_config::bounded::bounded("getent").args(["group", group]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().split(':').nth(2)?.parse().ok()
}

/// The same-user spira_lc credential: `SPIRA_LC_PASSWORD_FILE`, else the default path under
/// the operator's config dir. Its read-only sibling is this path with `-ro` appended. Phase
/// 1.5 creates both; phase 4.5 spends them.
fn same_user_credential_path() -> String {
    nonempty_env("SPIRA_LC_PASSWORD_FILE").unwrap_or_else(|| {
        let env_map: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        spira_config::resolve::lc_credential_default(&env_map)
    })
}

/// The port the lifecycle store's Dolt listens on: `SPIRA_LC_PORT`, else the beads Dolt's own
/// `dolt-server.yaml` (the store shares that server), else 3307.
fn lc_port(dolt_data: Option<&str>) -> u16 {
    nonempty_env("SPIRA_LC_PORT").and_then(|p| p.parse().ok()).or_else(|| dolt_data.and_then(|dd| read_yaml_port(&format!("{dd}/dolt-server.yaml")))).unwrap_or(3307)
}

/// Phase 4.6 (sp-k62xz8): the one-time lifecycle population — every bead in the beads
/// database with no lifecycle row gets one, by spira-lc's own classifier
/// (`install::populate`). Runs as the lifecycle user every other spira-lc caller connects as,
/// over a direct connection (classify is never forwarded to lc-serve). Idempotent: a second
/// run creates nothing. `SPIRA_INSTALL_LC_POPULATE_CONSIDERED` skips it, saying so.
fn populate_phase(dry: bool) -> Result<(), String> {
    if nonempty_env("SPIRA_INSTALL_LC_POPULATE_CONSIDERED").is_some() {
        info("lifecycle population NOT run — SPIRA_INSTALL_LC_POPULATE_CONSIDERED is set; beads already in the database have no lifecycle row until install runs without it");
        return Ok(());
    }
    let need = |k: &str| bootstrap::declared(k).ok_or_else(|| format!("{k} is declared empty — cannot locate what the population reads"));
    let run_dir = need("SPIRA_RUN")?;
    let inputs = install::populate::Inputs {
        bd_bin: bootstrap::declared("SPIRA_BD").unwrap_or_else(|| "bd".into()),
        bd_db: need("SPIRA_DB")?,
        landstate_dir: format!("{run_dir}/landstate"),
        queue_dir: need("SPIRA_QUEUE_DIR")?,
        ask_label: bootstrap::declared("SPIRA_ASK_LABEL"),
    };
    if dry {
        would(&format!("run: spira-lc {}", install::populate::args(&inputs).join(" ")));
        return Ok(());
    }
    let port = lc_port(bootstrap::declared("SPIRA_DOLT_DATA").as_deref());
    let counts = install::populate::run(&inputs, |args| {
        // batch-job: one-time install population of the lifecycle store; bounded at 600 s by timeout(1).
        let out = Command::new("timeout").arg("600").arg("spira-lc").args(args).env("SPIRA_LC_PORT", port.to_string()).stdin(Stdio::null()).output();
        match out {
            Ok(o) => (o.status.code().unwrap_or(1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
            Err(e) => (127, format!("cannot run spira-lc: {e}")),
        }
    })?;
    info(&format!("lifecycle population: {} bead(s) in the database, {} row(s) created, {} already present", counts.beads, counts.created, counts.present));
    if let Some(w) = install::populate::unknown_repo_line(&counts.unknown_repo) {
        eprintln!("install: WARNING: {w}");
    }
    Ok(())
}

/// Phase 4.5 (sp-xfqnr): build spira_lifecycle through `spira-lc`'s admin verbs, as the Dolt
/// admin: `SPIRA_LC_ADMIN_USER`/`SPIRA_LC_ADMIN_PASSWORD` when set, else root with the
/// password this phase provisions on first run (a fresh Dolt's root has none, and is never
/// left that way), against the SQL this release ships beside its unit templates.
fn lifecycle_store_phase(port: u16) -> Result<Vec<String>, String> {
    use install::lifecycle_store::{self, Admin};
    let host = nonempty_env("SPIRA_LC_HOST");
    if host.is_none() && !tcp_up(port) {
        return Err(format!("no Dolt server is listening on 127.0.0.1:{port} for the lifecycle store (SPIRA_LC_PORT / dolt-server.yaml)"));
    }
    let lifecycle_dir = bootstrap::templates_dir().parent().map(|r| r.join("lifecycle")).ok_or("cannot locate this release's lifecycle/ directory")?;
    let cred = same_user_credential_path();
    let rw = lifecycle_store::read_credential(Path::new(&cred))?;
    let ro = lifecycle_store::read_credential(Path::new(&format!("{cred}-ro")))?;
    let run = |args: &[String], env: &[(String, String)]| {
        // batch-job: one-time install DDL against the local Dolt server; bounded at 120 s by timeout(1).
        let mut c = Command::new("timeout");
        c.arg("120").arg("spira-lc").args(args).stdin(Stdio::null());
        for (k, v) in env {
            c.env(k, v);
        }
        match c.output() {
            Ok(o) => (o.status.code().unwrap_or(1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
            Err(e) => (127, format!("cannot run spira-lc: {e}")),
        }
    };
    let mut lines = Vec::new();
    let admin = if let Some(user) = nonempty_env("SPIRA_LC_ADMIN_USER") {
        Admin { user, password: std::env::var("SPIRA_LC_ADMIN_PASSWORD").unwrap_or_default(), host, port }
    } else {
        let env_map: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        let admin_cred = nonempty_env("SPIRA_LC_ADMIN_PASSWORD_FILE").unwrap_or_else(|| spira_config::resolve::lc_admin_credential_default(&env_map));
        let (admin, msg) = lifecycle_store::ensure_admin(Path::new(&admin_cred), &std::env::temp_dir(), host, port, run)?;
        lines.push(msg);
        // Every bd on this box connects as root: hand it the password root now has.
        let beads_cred = nonempty_env("BEADS_CREDENTIALS_FILE").unwrap_or_else(|| format!("{}/.config/beads/credentials", bootstrap::env_var("HOME")));
        let bd_host = admin.host.clone().unwrap_or_else(|| "127.0.0.1".into());
        lines.push(lifecycle_store::ensure_beads_credential(Path::new(&beads_cred), &bd_host, port, &admin.password)?);
        admin
    };
    lines.extend(lifecycle_store::apply(&lifecycle_dir, &std::env::temp_dir(), &admin, &rw, &ro, run)?);
    Ok(lines)
}

fn create_same_user_credential(path: &str) -> Result<(), String> {
    if let Some(dir) = Path::new(path).parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    write_random_credential(path, 0o600, None)
}

fn write_random_credential(path: &str, mode: u32, owner: Option<(&str, &str)>) -> Result<(), String> {
    let mut buf = [0u8; 32];
    std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf)).map_err(|e| e.to_string())?;
    let encoded = base64_no_pad(&buf);
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(path).map_err(|e| e.to_string())?;
    f.write_all(encoded.as_bytes()).map_err(|e| e.to_string())?;
    if let Some((user, group)) = owner {
        run_ok("chown", &[&format!("{user}:{group}"), path])?;
    }
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    Ok(())
}

fn base64_no_pad(bytes: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[(n >> 18 & 63) as usize] as char);
        out.push(CHARS[(n >> 12 & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(CHARS[(n >> 6 & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(CHARS[(n & 63) as usize] as char);
        }
    }
    out
}

#[cfg(test)]
mod dependency_fetch_tests {
    use super::*;

    #[test]
    fn sccache_target_maps_known_arches_to_mozillas_release_triples() {
        assert_eq!(sccache_target("x86_64").unwrap(), "x86_64-unknown-linux-musl");
        assert_eq!(sccache_target("aarch64").unwrap(), "aarch64-unknown-linux-musl");
        assert!(sccache_target("riscv64").is_err());
    }

    #[test]
    fn inotify_tools_arch_maps_known_arches_to_the_bare_label() {
        assert_eq!(inotify_tools_arch("x86_64").unwrap(), "x86_64");
        assert_eq!(inotify_tools_arch("aarch64").unwrap(), "aarch64");
        assert!(inotify_tools_arch("riscv64").is_err());
    }

    #[test]
    fn sccache_url_names_the_pinned_version_and_asset() {
        let url = sccache_url("x86_64-unknown-linux-musl");
        assert_eq!(url, "https://github.com/mozilla/sccache/releases/download/v0.18.0/sccache-v0.18.0-x86_64-unknown-linux-musl.tar.gz");
    }

    #[test]
    fn inotify_tools_url_names_the_pinned_version_and_asset() {
        let url = inotify_tools_url("aarch64");
        assert_eq!(url, "https://github.com/inotify-tools/inotify-tools/releases/download/4.26.262/inotify-tools-4.26.262-aarch64-linux.tar.gz");
    }

    #[test]
    fn sccache_tar_member_is_the_versioned_directory_the_release_asset_actually_contains() {
        // mozilla/sccache's release.yml packs the binary at <dirname>/sccache where
        // <dirname> is the same string the published .tar.gz's own filename is built
        // from — not just "sccache" at the tarball root.
        assert_eq!(sccache_tar_member("x86_64-unknown-linux-musl"), "sccache-v0.18.0-x86_64-unknown-linux-musl/sccache");
    }

    /// The exact text of sccache 0.18.0's own "Enabled features" block (captured live,
    /// built with `--features webdav`) — doctor/src/real.rs's REAL_SCCACHE_018_HELP pins
    /// this same text independently; kept in sync by inspection, not by sharing code.
    const REAL_SCCACHE_018_HELP_WEBDAV_TRUE: &str = "Enabled features:\n    S3:        false\n    Redis:     false\n    Memcached: false\n    GCS:       false\n    GHA:       false\n    Azure:     false\n    WebDAV:    true\n    OSS:       false\n    COS:       false\n";

    #[test]
    fn sccache_help_has_webdav_true_passes() {
        assert!(sccache_help_has_webdav(REAL_SCCACHE_018_HELP_WEBDAV_TRUE));
    }

    #[test]
    fn sccache_help_has_webdav_false_fails() {
        let help = REAL_SCCACHE_018_HELP_WEBDAV_TRUE.replace("WebDAV:    true", "WebDAV:    false");
        assert!(!sccache_help_has_webdav(&help));
    }

    #[test]
    fn sccache_help_has_webdav_missing_line_fails() {
        assert!(!sccache_help_has_webdav("sccache: error: no such option --help\n"));
    }

    #[test]
    fn aerc_outgoing_at_a_deleted_checkout_path_is_rewritten() {
        let conf = "[spira]\nsource = maildir:///m\noutgoing = /co/spira/mail.sh\nfrom = O <o@s>\n";
        let new = rewrite_aerc_outgoing(conf, "/h/.local/bin/spira-sendmail").unwrap();
        assert_eq!(new, "[spira]\nsource = maildir:///m\noutgoing = /h/.local/bin/spira-sendmail\nfrom = O <o@s>\n");
        assert!(rewrite_aerc_outgoing(&new, "/h/.local/bin/spira-sendmail").is_none());
    }

    #[test]
    fn sendmail_wrapper_names_release_and_toml_and_supports_check() {
        let w = render_sendmail_wrapper("/r", "/t.toml");
        assert!(w.contains("readlink -f '/r/current'") && w.contains("SPIRA_TOML='/t.toml'") && w.contains("--check"));
    }

    #[test]
    fn aerc_reports_version_matches_the_real_dash_v_output_shape() {
        assert!(aerc_reports_version("aerc 0.22.0 (go1.27.1 amd64 linux)\n", "0.22.0"));
        assert!(!aerc_reports_version("aerc 0.21.0 (go1.25.0 amd64 linux)\n", "0.22.0"));
        assert!(!aerc_reports_version("", "0.22.0"));
        assert!(!aerc_reports_version("aerc\n", "0.22.0"));
    }

    /// sp-x6v17: the coordinator asked specifically for this case — a present-at-the-
    /// pinned-version aerc must skip install_aerc's copy entirely.
    #[test]
    fn a_present_at_pinned_version_aerc_skips_the_install() {
        assert!(aerc_install_can_be_skipped(Some("aerc 0.22.0 (go1.27.1 amd64 linux)\n")));
        assert!(!aerc_install_can_be_skipped(Some("aerc 0.21.0 (go1.25.0 amd64 linux)\n")));
        assert!(!aerc_install_can_be_skipped(None));
    }

    /// A fake `timeout` on PATH that drops its own first argument (the duration) and execs
    /// the rest — enough for `install_aerc`'s `timeout 5 <path> -v` version checks, with no
    /// real `timeout`(1) dependency and, critically, no `curl`, `go` or `tar` anywhere on
    /// PATH at all. Any attempt by the code under test to spawn one of those would fail with
    /// ENOENT, so a test whose PATH holds only this proves the code path it exercises
    /// touches no network and no Go toolchain.
    fn fake_timeout_only_path() -> testkit::TempDir {
        let d = testkit::TempDir::new("install-aerc-fake-path");
        testkit::write_exe(d.join("timeout"), "#!/bin/sh\nshift\nexec \"$@\"\n");
        d
    }

    /// A fake aerc binary (any interpreter is fine — install_aerc only ever runs `-v`
    /// against it) that reports the pinned AERC_VERSION, written at `path`.
    fn fake_aerc_reporting_pinned_version(path: &Path) {
        testkit::write_exe(path, "#!/bin/sh\nprintf 'aerc 0.22.0 (go1.27.1 amd64 linux)\\n'\n");
    }

    /// sp-41so3: before this bead, aerc's only path to ~/.local/bin was `find_usable_go`
    /// then `go install` against a freshly-curled source archive — no concept of a release
    /// carrying a prebuilt copy existed at all, so a fresh host (no aerc at `bin`/aerc yet)
    /// always needed the network, no matter what sat beside it. This is exactly the shape
    /// that failed real acceptance on 2026-10-04: every `go install` module fetch hit a TLS
    /// handshake timeout under load. This test is RED against that code (no such function
    /// as `install_aerc`, and `fetch_aerc` could not have passed it: it ignores
    /// `AERC_VENDORED_REL` entirely and reaches straight for go/curl on a cache miss) and
    /// GREEN after sp-41so3: a fresh host whose release already carries the vendored binary
    /// installs with PATH holding nothing but a fake `timeout` — no curl, no go, no tar
    /// resolvable at all, so any attempt to spawn one would have errored this test out.
    #[test]
    fn install_aerc_copies_the_vendored_binary_and_spawns_no_network_command() {
        let fakebin = fake_timeout_only_path();
        let _env = testkit::env(&[("PATH", fakebin.path().to_str())]);
        let release = testkit::TempDir::new("install-aerc-release");
        let vendor_dir = release.join("vendor/bin");
        std::fs::create_dir_all(&vendor_dir).unwrap();
        fake_aerc_reporting_pinned_version(&vendor_dir.join("aerc"));
        let bin = testkit::TempDir::new("install-aerc-bin");

        let r = install_aerc(false, &bin, Some(release.path()));

        assert!(r.is_ok(), "{r:?}");
        let dest = bin.join("aerc");
        assert!(dest.is_file(), "aerc was not copied to {}", dest.display());
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "aerc is not executable (mode {mode:o})");
    }

    #[test]
    fn install_aerc_skips_the_copy_when_the_pinned_version_is_already_at_dest() {
        let fakebin = fake_timeout_only_path();
        let _env = testkit::env(&[("PATH", fakebin.path().to_str())]);
        let bin = testkit::TempDir::new("install-aerc-bin-skip");
        fake_aerc_reporting_pinned_version(&bin.join("aerc"));

        // release_root is None (no release to copy from at all): a correct skip never looks
        // at it, because there is nothing to do.
        let r = install_aerc(false, &bin, None);
        assert!(r.is_ok(), "{r:?}");
    }

    #[test]
    fn install_aerc_fails_closed_with_no_release_root() {
        let fakebin = fake_timeout_only_path();
        let _env = testkit::env(&[("PATH", fakebin.path().to_str())]);
        let bin = testkit::TempDir::new("install-aerc-bin-norel");

        let r = install_aerc(false, &bin, None);
        let e = r.expect_err("no release root must refuse, not silently skip");
        assert!(e.contains(AERC_VENDORED_REL), "{e}");
        assert!(e.contains("build-tarball.sh"), "{e}");
        assert!(!bin.join("aerc").exists());
    }

    /// SPIRA_INSTALL_AERC_CONSIDERED (sp-41so3, the same named-opt-out shape phase 4.5's
    /// SPIRA_INSTALL_LC_STORE_CONSIDERED already uses): an install-suite fixture installing
    /// from a plain tree has no release at all to resolve, let alone one carrying
    /// AERC_VENDORED_REL — without this escape hatch the fail-closed refusal above stops
    /// every such suite dead in phase -1, long before the phase it actually means to test
    /// (test-install-hooks-artifact.sh and 7 siblings, caught by the landing gate). Named
    /// and loud: Ok(()), but only after printing why aerc was not installed — never a
    /// silent skip, and no caller with a real release has reason to set it.
    #[test]
    fn install_aerc_considered_opt_out_reports_and_skips_with_no_release_root() {
        let fakebin = fake_timeout_only_path();
        let _env = testkit::env(&[("PATH", fakebin.path().to_str()), ("SPIRA_INSTALL_AERC_CONSIDERED", Some("1"))]);
        let bin = testkit::TempDir::new("install-aerc-bin-considered");

        let r = install_aerc(false, &bin, None);
        assert!(r.is_ok(), "{r:?}");
        assert!(!bin.join("aerc").exists(), "the opt-out must not fabricate an aerc binary");
    }

    #[test]
    fn install_aerc_fails_closed_when_the_release_has_no_vendored_binary() {
        let fakebin = fake_timeout_only_path();
        let _env = testkit::env(&[("PATH", fakebin.path().to_str())]);
        let bin = testkit::TempDir::new("install-aerc-bin-novendor");
        // A release with a bin/ and spira/ of its own but no vendor/bin/aerc — e.g. a
        // tarball built before sp-41so3.
        let release = testkit::TempDir::new("install-aerc-release-novendor");

        let r = install_aerc(false, &bin, Some(release.path()));
        let e = r.expect_err("a release missing the vendored binary must refuse, not silently skip");
        assert!(e.contains(AERC_VENDORED_REL), "{e}");
        assert!(e.contains("build-tarball.sh"), "{e}");
        assert!(!bin.join("aerc").exists());
    }

    #[test]
    fn install_aerc_dry_run_reports_intent_and_copies_nothing() {
        let fakebin = fake_timeout_only_path();
        let _env = testkit::env(&[("PATH", fakebin.path().to_str())]);
        let release = testkit::TempDir::new("install-aerc-release-dry");
        let vendor_dir = release.join("vendor/bin");
        std::fs::create_dir_all(&vendor_dir).unwrap();
        fake_aerc_reporting_pinned_version(&vendor_dir.join("aerc"));
        let bin = testkit::TempDir::new("install-aerc-bin-dry");

        let r = install_aerc(true, &bin, Some(release.path()));
        assert!(r.is_ok(), "{r:?}");
        assert!(!bin.join("aerc").exists(), "a dry run must not copy anything");
    }
}
