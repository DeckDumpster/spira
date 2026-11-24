//! `spira/deploy.sh` is the subject, so the script itself runs under `bash` with stub `gh`,
//! `systemctl`, `release` and the rest of what it calls, each scenario in its own scratch box.
//! Booting the script is most of a run's cost, so a test starts its scenarios together. No shell
//! suite may grow.

#[macro_use]
extern crate spira_test_id;
use std::path::{Path, PathBuf};
use std::process::Command;

const NEW: &str = "spira-20260917T053803Z";
const NEW_TAG: &str = "spira-release-spira-20260917T053803Z";
const PRIOR: &str = "spira-20260901T000000Z";
const DRAFT_TAG: &str = "spira-release-spira-20260918T000000Z";
const OLD_PROD: &str = "old-checkout/spira";
const STUB_NAMES: &[&str] = &["gh", "world.sh", "release", "install.sh", "layout.sh", "doctor.sh", "skew.sh", "slay.sh", "spira-lc", "systemctl", "ctrl"];

const STUBS: &str = r#"#!/bin/bash
n="$(basename "$0")"
printf '%s %s\n' "$n" "$*" >> "$CALL_LOG"
case "$n" in
gh)
    args=(); while [ $# -gt 0 ]; do case "$1" in --repo) shift 2 ;; *) args+=("$1"); shift ;; esac; done
    set -- "${args[@]}"
    case "$1 $2" in
    "release list") printf '%s\n' "${GH_RELEASE_LIST:-[]}" ;;
    "release view")
        case "$*" in
        *assets*) printf '{"assets":[{"name":"%s"}]}\n' "$GH_ASSET" ;;
        *) [ "${GH_DRAFT:-0}" = 1 ] && echo '{"isDraft":true}' || echo '{"isDraft":false}' ;;
        esac ;;
    "release download") shift 2; while [ $# -gt 0 ]; do case "$1" in --dir) d="$2"; shift 2 ;; --pattern) p="$2"; shift 2 ;; *) shift ;; esac; done; mkdir -p "$d"; : > "$d/$p" ;;
    esac ;;
world.sh) [ "$1" != drain ] || exit "${WORLD_DRAIN_EXIT:-0}" ;;
release)
    printf 'toml-at-activate %s\n' "$(grep '^prod' "$SPIRA_TOML")" >> "$CALL_LOG"
    name="$(basename "${*: -1}" .tar.gz)"
    mkdir -p "$SPIRA_RELEASES/$name"
    ln -s "$name" "$SPIRA_RELEASES/.cur.$$" && mv -T "$SPIRA_RELEASES/.cur.$$" "$SPIRA_RELEASES/current" ;;
install.sh)
    printf 'install SPIRA_PROD=%s SPIRA_HOME=%s SPIRA_REPO=%s\n' "${SPIRA_PROD:-UNSET}" "${SPIRA_HOME:-UNSET}" "${SPIRA_REPO:-UNSET}" >> "$CALL_LOG"
    printf 'install-resolved %s\n' "$(readlink -f "$SPIRA_HOME")" >> "$CALL_LOG" ;;
doctor.sh)
    c=$(( $(cat "$CALL_LOG.doctor" 2>/dev/null || echo 0) + 1 )); echo "$c" > "$CALL_LOG.doctor"
    if [ "${DOCTOR_EXIT:-0}" != 0 ] || { [ -n "${DOCTOR_FAIL_ON_CALL:-}" ] && [ "$c" -ge "$DOCTOR_FAIL_ON_CALL" ]; }; then
        printf '  FAIL  %s\n' "${DOCTOR_FAIL_MSG:-injected failure}"; exit 1
    fi ;;
skew.sh) [ -z "${SKEW_OUT:-}" ] || { printf '%s\n' "$SKEW_OUT"; exit "${SKEW_EXIT:-1}"; } ;;
spira-lc) [ -z "${MIGRATE_FAIL:-}" ] || { echo "database is at v61, binary knows up to v53"; exit 1; } ;;
systemctl)
    case "$*" in
    *list-unit-files*--no-legend*) [ -z "${SC_UNIT_FILES:-}" ] || printf '%s\n' "$SC_UNIT_FILES" ;;
    *is-enabled*) echo "${SC_IS_ENABLED:-enabled}" ;;
    *list-units*--state=failed*) ;;
    *list-units*) echo 'spira-sentinel-prod.service loaded active running' ;;
    esac ;;
ctrl) [ "$1" = check ] && [ "$2" = "${CTRL_SUSPENDED:-@none@}" ]; exit $? ;;
esac
exit 0
"#;

#[derive(Default)]
struct Spec {
    args: Vec<&'static str>,
    prior: bool,
    already_current: bool,
    db: bool,
    live_dwarf: bool,
    env: Vec<(&'static str, String)>,
}

fn spec(args: &[&'static str]) -> Spec {
    Spec { args: args.to_vec(), ..Spec::default() }
}

impl Spec {
    fn prior(mut self) -> Self {
        self.prior = true;
        self
    }
    fn env(mut self, k: &'static str, v: &str) -> Self {
        self.env.push((k, v.to_string()));
        self
    }
}

struct Run {
    code: i32,
    out: String,
    log: String,
    releases: PathBuf,
    toml: PathBuf,
    _dir: testkit::TempDir,
}

impl Run {
    fn current(&self) -> Option<String> {
        std::fs::read_link(self.releases.join("current")).ok().map(|p| p.display().to_string())
    }
    fn config_text(&self) -> String {
        std::fs::read_to_string(&self.toml).unwrap()
    }
}

fn run(s: Spec) -> Run {
    let dir = testkit::TempDir::new("deploy-script");
    let d = dir.path();
    let bin = d.join("bin");
    let releases = d.join("releases");
    let run_dir = d.join("run");
    let release = d.join("release");
    for p in [&bin, &releases, &run_dir, &release.join("bin")] {
        std::fs::create_dir_all(p).unwrap();
    }
    std::os::unix::fs::symlink(testkit::bin_exe("spira-config"), release.join("bin/spira-config")).unwrap();
    for n in STUB_NAMES {
        testkit::write_exe(bin.join(n), STUBS);
    }
    let repo = d.join("repo");
    std::fs::create_dir(&repo).unwrap();
    if s.prior || s.already_current {
        let cur = if s.already_current { NEW } else { PRIOR };
        std::fs::create_dir(releases.join(cur)).unwrap();
        std::os::unix::fs::symlink(cur, releases.join("current")).unwrap();
    }
    let db = d.join("db");
    if s.db {
        std::fs::create_dir_all(db.join(".beads")).unwrap();
    }
    if s.live_dwarf {
        std::fs::write(run_dir.join("dwarf-alviss-sp-frce1.pid"), format!("{}\n", std::process::id())).unwrap();
    }
    let path = |p: &Path| p.display().to_string();
    let toml = spira_config::process::fixture_toml(
        d,
        &[
            ("SPIRA_PATH", &path(&bin)),
            ("SPIRA_DB", &path(&db)),
            ("SPIRA_RUN", &path(&run_dir)),
            ("SPIRA_RELEASES", &path(&releases)),
            ("SPIRA_FORGE_REPO", "testowner/testrepo"),
            ("SPIRA_INSTANCE", "prod"),
            ("SPIRA_PROD", OLD_PROD),
        ],
    );
    let log = d.join("calls.log");
    std::fs::write(&log, "").unwrap();
    let script = testkit::manifest_dir().parent().unwrap().join("spira/deploy.sh");
    let mut c = Command::new("bash");
    c.env_clear().arg(&script).args(&s.args);
    let stub = |n: &str| path(&bin.join(n));
    for (k, v) in [
        ("PATH", format!("{}:{}:/usr/bin:/bin", bin.display(), release.join("bin").display())),
        ("HOME", path(d)),
        ("SPIRA_RELEASE", path(&release)),
        ("SPIRA_HOME", path(script.parent().unwrap())),
        ("SPIRA_CONF", path(&d.join("no-conf"))),
        ("SPIRA_TOML", path(&toml)),
        ("SPIRA_REPO", path(&repo)),
        ("SPIRA_DOCTOR", "1".into()),
        ("SPIRA_SYSTEMCTL", stub("systemctl")),
        ("SPIRA_WORLD_SH", stub("world.sh")),
        ("SPIRA_ACTIVATE_SH", stub("release")),
        ("SPIRA_INSTALL_SH", stub("install.sh")),
        ("SPIRA_COCKPIT_LAYOUT_SH", stub("layout.sh")),
        ("SPIRA_DOCTOR_SH", stub("doctor.sh")),
        ("SPIRA_SKEW_SH", stub("skew.sh")),
        ("SPIRA_SLAY_SH", stub("slay.sh")),
        ("SPIRA_CTRL_SH", stub("ctrl")),
        ("CALL_LOG", path(&log)),
        ("GH_ASSET", format!("{NEW}.tar.gz")),
        ("GIT_CONFIG_NOSYSTEM", "1".into()),
    ] {
        c.env(k, v);
    }
    for (k, v) in &s.env {
        c.env(k, v);
    }
    let o = c.output().unwrap();
    Run {
        code: o.status.code().unwrap(),
        out: format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)),
        log: std::fs::read_to_string(&log).unwrap(),
        releases,
        toml,
        _dir: dir,
    }
}

/// The scenarios run at once; their results come back in the order given.
fn together(specs: Vec<Spec>) -> Vec<Run> {
    std::thread::scope(|s| {
        let all: Vec<_> = specs.into_iter().map(|sp| s.spawn(|| run(sp))).collect();
        all.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

// covers: UC-instance-lifecycle-12
#[test_id(tkahfe0)]
#[test]
fn deploy_resolves_latest_and_refuses_a_draft_the_current_release_a_migration_mismatch_and_a_failed_doctor_before_any_disruptive_step() {
    let mixed = format!("[{{\"tagName\":\"{NEW_TAG}\",\"isDraft\":false}},{{\"tagName\":\"{DRAFT_TAG}\",\"isDraft\":true}}]");
    let mut already = spec(&[NEW_TAG]);
    already.already_current = true;
    let mut migrating = spec(&[NEW_TAG]).prior().env("MIGRATE_FAIL", "1");
    migrating.db = true;
    let r = together(vec![
        spec(&["latest"]).prior().env("GH_RELEASE_LIST", &mixed),
        spec(&[NEW_TAG]).prior().env("GH_DRAFT", "1"),
        already,
        migrating,
        spec(&[NEW_TAG]).prior().env("DOCTOR_EXIT", "1").env("DOCTOR_FAIL_MSG", "hooks-path-missing"),
    ]);
    let [latest, draft, current, migration, doctor] = &r[..] else { unreachable!() };
    let before_any_disruption = |x: &Run| !x.log.contains("world.sh drain") && !x.log.contains("release install-tarball");

    assert_eq!(latest.code, 0, "{}", latest.out);
    assert_eq!(latest.current().as_deref(), Some(NEW), "latest skips the newer draft: {}", latest.out);
    assert!(latest.log.contains("release download"), "{}", latest.log);

    assert_ne!(draft.code, 0, "{}", draft.out);
    assert!(draft.out.contains("draft release"), "{}", draft.out);
    assert!(before_any_disruption(draft) && !draft.log.contains("release download"), "{}", draft.log);
    assert_eq!(draft.current().as_deref(), Some(PRIOR));

    assert_eq!(current.code, 1, "{}", current.out);
    assert!(current.out.contains("already current"), "{}", current.out);
    assert!(!current.log.contains("release download"), "{}", current.log);

    assert_ne!(migration.code, 0, "{}", migration.out);
    assert!(migration.out.contains("migration mismatch"), "{}", migration.out);
    assert!(before_any_disruption(migration), "{}", migration.log);

    assert_ne!(doctor.code, 0, "{}", doctor.out);
    assert!(doctor.out.contains("hooks-path-missing"), "{}", doctor.out);
    assert!(before_any_disruption(doctor), "{}", doctor.log);
    assert_eq!(doctor.current().as_deref(), Some(PRIOR));
}

// covers: UC-instance-lifecycle-13
#[test_id(t5tv1hk)]
#[test]
fn deploy_drains_first_a_refusal_blocks_force_slays_and_the_prod_path_is_written_before_activation_then_rendered() {
    let mut force = spec(&["--force", NEW_TAG]).prior();
    force.live_dwarf = true;
    let r = together(vec![spec(&[NEW_TAG]).prior().env("WORLD_DRAIN_EXIT", "1"), force, spec(&[NEW_TAG]).prior()]);
    let [refused, forced, healthy] = &r[..] else { unreachable!() };

    assert_ne!(refused.code, 0, "{}", refused.out);
    assert!(refused.out.contains("drain refused"), "{}", refused.out);
    assert!(!refused.log.contains("slay.sh") && !refused.log.contains("release install-tarball"), "{}", refused.log);
    assert_eq!(refused.current().as_deref(), Some(PRIOR));

    assert_eq!(forced.code, 0, "{}", forced.out);
    assert!(forced.log.contains("world.sh drain --timeout 0"), "{}", forced.log);
    assert!(forced.log.contains(&format!("slay.sh --bead sp-frce1 --keep-work --reopen --why deploy {NEW_TAG}")), "{}", forced.log);

    assert_eq!(healthy.code, 0, "{}", healthy.out);
    let prod = format!("{}/current/spira", healthy.releases.display());
    assert!(healthy.log.contains(&format!("toml-at-activate prod = \"{prod}\"")), "SPIRA_PROD is in the config when activation restarts services: {}", healthy.log);
    let release = format!("{}/current", healthy.releases.display());
    assert!(healthy.log.contains(&format!("install SPIRA_PROD={prod} SPIRA_HOME={prod} SPIRA_REPO={release}")), "{}", healthy.log);
    assert!(!healthy.log.contains("old-checkout"), "the old checkout is gone from every call: {}", healthy.log);
}

// covers: UC-instance-lifecycle-14
#[test_id(trhen7g)]
#[test]
fn deploy_rolls_a_failed_health_check_back_restoring_the_config_and_never_re_enables_a_suspended_unit() {
    let suspended_unit = "spira-ops-prod.timer enabled enabled";
    let with_unit = |s: Spec| s.env("SC_UNIT_FILES", suspended_unit).env("SC_IS_ENABLED", "disabled").env("DOCTOR_FAIL_ON_CALL", "2");
    let r = together(vec![
        spec(&[NEW_TAG]).prior().env("DOCTOR_FAIL_ON_CALL", "2"),
        spec(&[NEW_TAG]).env("DOCTOR_FAIL_ON_CALL", "2"),
        with_unit(spec(&[NEW_TAG]).prior()),
        with_unit(spec(&[NEW_TAG]).prior()).env("CTRL_SUSPENDED", "spira-ops"),
    ]);
    let [rolled, first, control, suspended] = &r[..] else { unreachable!() };
    let newer = "spira-9.9.9";
    let not_latest = format!("NOT-LATEST x is not the latest published release {newer}");
    let sk = together(vec![
        spec(&[NEW_TAG]).prior().env("SKEW_OUT", "MANIFEST-MISMATCH sidecar differs"),
        spec(&[NEW_TAG]).prior().env("SKEW_OUT", &not_latest),
        spec(&["latest"]).prior().env("SKEW_OUT", &not_latest),
        spec(&[NEW_TAG]).prior().env("SKEW_OUT", &format!("{not_latest}\nMANIFEST-MISMATCH sidecar differs")),
    ]);
    let [skew_rolled, named_older, latest_refused, mixed] = &sk[..] else { unreachable!() };
    assert_ne!(skew_rolled.code, 0, "{}", skew_rolled.out);
    assert!(skew_rolled.out.contains("ROLLBACK") && skew_rolled.out.contains("deploy: skew: MANIFEST-MISMATCH"), "{}", skew_rolled.out);
    assert_eq!(skew_rolled.current().as_deref(), Some(PRIOR));
    assert_eq!(named_older.code, 0, "a NOT-LATEST-only finding about a newer release is accepted for a named tag: {}", named_older.out);
    assert_ne!(latest_refused.code, 0, "{}", latest_refused.out);
    assert_ne!(mixed.code, 0, "a NOT-LATEST finding beside another one still rolls back: {}", mixed.out);
    let enable = "systemctl --user enable --now spira-ops-prod.timer";

    assert_ne!(rolled.code, 0, "{}", rolled.out);
    assert!(rolled.out.contains("ROLLBACK") && rolled.out.contains("deploy: doctor:   FAIL  injected failure"), "{}", rolled.out);
    assert_eq!(rolled.current().as_deref(), Some(PRIOR));
    let prod_line = rolled.config_text().lines().find(|l| l.starts_with("prod = ")).unwrap().to_string();
    assert_eq!(prod_line, format!("prod = \"{OLD_PROD}\""), "the config is restored");
    assert!(rolled.log.contains("systemctl --user restart spira-sentinel-prod.service"), "{}", rolled.log);
    let last_render = rolled.log.lines().rfind(|l| l.starts_with("install SPIRA_PROD=")).unwrap();
    assert!(last_render.ends_with(&format!("SPIRA_REPO={}/current", rolled.releases.display())), "the re-render runs on the restored release: {last_render}");

    assert_ne!(first.code, 0, "{}", first.out);
    assert!(first.out.contains("no prior release"), "{}", first.out);
    assert_eq!(first.current(), None, "a first deploy's rollback removes current");
    assert!(first.log.contains(&format!("install SPIRA_PROD={OLD_PROD} ")) && first.log.contains("world.sh resume"), "{}", first.log);

    assert!(control.log.contains(enable), "control: a unit enabled before the deploy is re-enabled: {}", control.log);
    assert!(!suspended.log.contains(enable), "a ctrl-suspended unit stays off: {}", suspended.log);
    assert!(suspended.out.contains("is suspended"), "{}", suspended.out);
}
