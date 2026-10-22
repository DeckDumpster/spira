// covers: systemd/*.service systemd/*.timer install/src/values.rs
use install::values::{render_file, HostValues};
use std::path::{Path, PathBuf};

const IO_FENCED: [&str; 5] = ["spira-tsd-prune.service", "spira-landing-pass.service", "spira-sop-lint.service", "lc-backup.service", "lc-backup-drill.service"];
const QUOTA_FENCED: [&str; 17] = ["spira-tsd-prune.service", "spira-dolt-boot-check.service", "spira-mail-reconcile.service", "spira-concierge-guard.service", "spira-nix-gc.service", "spira-verify-asks.service", "spira-perf-watch.service", "spira-perf-happy-path.service", "spira-refusal-watch.service", "spira-kit-bump.service", "spira-runtime-watch.service", "spira-sift.service", "spira-reclaim.service", "spira-unit-run-dbus.service", "spira-metrics-vm.service", "spira-metrics-collector.service", "spira-metrics-grafana.service"];
const FENCE_EXEMPT_OTHER: [&str; 1] = ["dolt-tmp-prune.service"];
const STORE_LATE: u32 = 60;

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../systemd")
}

fn host() -> HostValues {
    HostValues {
        toml: "/cfg/base.toml:/cfg/over.toml".into(),
        home: "/rel/spira".into(),
        repo: "/rel/repo".into(),
        run: "/rel/run".into(),
        db: "/rel/db".into(),
        cockpit: "/rel/cockpit".into(),
        dolt_data: "/rel/dolt".into(),
        dolt: "/rel/bin/dolt".into(),
        prod: "".into(),
        instance: "prod".into(),
        snap_stale_s: "600".into(),
        watchtower_start_timeout_s: "360".into(),
        path_tail: "".into(), env_bin: "/rel/env/bin".into(),
        sccache_dav_addr: "127.0.0.1:1".into(),
        repo_map: "/rel/repomap".into(),
        lc_password_file: "/rel/run/lc.credential".into(),
    }
}

fn templates() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".service") || n.ends_with(".timer") || n.ends_with(".socket"))
        .collect();
    v.sort();
    v
}

fn rendered() -> Vec<(String, String)> {
    templates()
        .into_iter()
        .map(|n| {
            let w = n.starts_with("spira-watch@").then_some("alpha");
            let text = render_file(&dir().join(&n), &host(), w).unwrap_or_else(|e| panic!("{n}: {e}"));
            (n, text)
        })
        .collect()
}

fn raw(n: &str) -> String {
    std::fs::read_to_string(dir().join(n)).unwrap()
}

fn fence_lines(text: &str) -> usize {
    text.lines().filter(|l| ["CPUQuota=", "Nice=", "IOSchedulingClass="].iter().any(|p| l.starts_with(p))).count()
}

fn strays(n: &str, text: &str) -> Vec<String> {
    let sys = ["/usr/", "/bin/", "/etc/", "/dev/", "/proc/", "/sys/", "/var/", "/run/", "/lib/", "/opt/", "/sbin/"];
    let mut out = Vec::new();
    for l in text.lines().filter(|l| ["Exec", "Environment", "ReadWritePaths", "WorkingDirectory"].iter().any(|p| l.starts_with(p))) {
        for tok in l.split(|c: char| c == ' ' || c == '=' || c == ':' || c == '"').filter(|t| t.starts_with('/') && t.len() > 1) {
            let ok = ["/rel/", "/cfg/"].iter().any(|r| tok.starts_with(r)) || sys.iter().any(|s| tok.starts_with(s)) || ["/tmp", "/rel", "/bin"].contains(&tok);
            if !ok {
                out.push(format!("{n}: {tok}"));
            }
        }
    }
    out
}

#[test]
fn every_unit_renders_with_no_placeholder_and_every_path_from_configuration() {
    assert_eq!(strays("p", "[Service]\nExecStart=/srv/literal/tool\n").len(), 1, "positive control");
    let r = rendered();
    assert!(r.len() >= 20, "positive control: render holds many units, got {}", r.len());
    for (n, text) in &r {
        assert!(release::units::placeholders(text).is_empty(), "{n}: placeholder survived");
        assert!(!n.starts_with("spira-watch@") || !text.contains("%i"), "{n}: template specifier survived");
        let s = strays(n, text);
        assert!(s.is_empty(), "path not from configuration: {s:?}");
    }
}

#[test]
fn no_unit_outside_the_fenced_set_carries_cpuquota_nice_or_ioschedulingclass() {
    assert_eq!(fence_lines("[Service]\nCPUQuota=35%\nNice=10\nType=oneshot\n"), 2, "positive control");
    let mut outside = Vec::new();
    for (n, text) in rendered() {
        let exempt = IO_FENCED.contains(&n.as_str()) || QUOTA_FENCED.contains(&n.as_str()) || FENCE_EXEMPT_OTHER.contains(&n.as_str());
        if !exempt && fence_lines(&text) > 0 {
            outside.push(n);
        }
    }
    assert!(outside.is_empty(), "fenced outside the sanctioned set: {outside:?}");
    for (n, text) in rendered() {
        if IO_FENCED.contains(&n.as_str()) {
            assert!(text.contains("IOSchedulingClass=idle"), "{n}: IO-fenced idle");
        }
        if QUOTA_FENCED.contains(&n.as_str()) {
            assert!(text.contains("CPUQuota="), "{n}: CPU-capped");
        }
    }
}

#[test]
fn the_notify_refresh_and_mail_tidy_pairs_fire_periodically() {
    for t in ["spira-notify.timer", "spira-refresh.timer", "spira-mail-tidy.timer", "spira-mail-reconcile.timer"] {
        let text = render_file(&dir().join(t), &host(), None).unwrap();
        assert!(text.contains("OnUnitActiveSec="), "{t}: fires periodically");
    }
}

#[test]
fn cert_sweep_passes_the_git_checkout_not_the_release_dir() {
    for u in ["full", "sample"] {
        let t = raw(&format!("spira-cert-sweep-{u}.service"));
        assert!(t.lines().any(|l| l.starts_with("ExecStart=") && l.contains("cert-sweep pass") && l.contains("--repo @SPIRA_REPO@")), "cert-sweep-{u}");
    }
}

#[test]
fn a_clean_stop_of_the_metrics_store_is_not_a_failure() {
    let ok = |t: &str| t.lines().any(|l| l.trim() == "SuccessExitStatus=2");
    assert!(!ok("[Service]\nRestart=always\n"), "positive control");
    assert!(ok(&raw("spira-metrics-vm.service")));
}

fn lone_pct(text: &str) -> usize {
    text.lines().filter(|l| l.starts_with("Exec")).filter(|l| l.replace("%%", "").contains('%') && l.replace("%%", "").split('%').skip(1).any(|r| r.starts_with(|c: char| c.is_ascii_alphabetic()))).count()
}

#[test]
fn exec_lines_carry_no_unescaped_percent_specifier() {
    assert!(lone_pct("ExecCondition=/bin/bash -c 'printf \"%s\"'") > 0, "positive control");
    assert_eq!(lone_pct(&raw("spira-ops.service")), 0);
}

#[test]
fn every_exec_start_begins_with_a_placeholder() {
    let bare = |t: &str| t.lines().any(|l| l.starts_with("ExecStart=/") || l.starts_with("ExecStart=-/"));
    assert!(bare("ExecStart=/bin/bash -c 'exec x'\n"), "positive control");
    let offenders: Vec<_> = templates().into_iter().filter(|n| n.ends_with(".service") && bare(&raw(n))).collect();
    assert!(offenders.is_empty(), "{offenders:?}");
}

#[test]
fn every_service_template_sets_spira_toml() {
    let has = |t: &str| t.lines().any(|l| l == "Environment=SPIRA_TOML=@SPIRA_TOML@");
    assert!(!has("[Service]\nExecStart=@SPIRA_PROD_ROOT@/bin/x\n"), "positive control");
    let offenders: Vec<_> = templates().into_iter().filter(|n| n.ends_with(".service") && !has(&raw(n))).collect();
    assert!(offenders.is_empty(), "{offenders:?}");
}

fn store_exempt(n: &str) -> bool {
    n.starts_with("dolt-") || ["lc-serve.service", "spira-lc.service", "sccache-dav.service"].contains(&n)
}

fn num(text: &str, key: &str) -> Option<u32> {
    text.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok())
}

fn store_unordered(n: &str, t: &str) -> bool {
    t.contains("\nEnvironment=SPIRA_LC_PASSWORD_FILE=") && !store_exempt(n)
        && !(t.lines().any(|l| l.starts_with("After=") && l.contains("lc-serve.service")) && t.lines().any(|l| l.starts_with("Wants=") && l.contains("lc-serve.service")))
}

fn store_short_retry(n: &str, t: &str) -> bool {
    t.contains("\nEnvironment=SPIRA_LC_PASSWORD_FILE=")
        && t.lines().any(|l| l == "Restart=always" || l.starts_with("Restart=on-failure"))
        && !store_exempt(n)
        && num(t, "RestartSec=").unwrap_or(0) * (num(t, "StartLimitBurst=").unwrap_or(5) - 1) < STORE_LATE
}

#[test]
fn every_store_credentialed_unit_waits_for_and_outlasts_the_store() {
    let planted = "[Unit]\n[Service]\nEnvironment=SPIRA_LC_PASSWORD_FILE=x\nRestart=always\nRestartSec=5\n";
    assert!(store_unordered("p.service", planted) && store_short_retry("p.service", planted), "positive control");
    let svc: Vec<_> = templates().into_iter().filter(|n| n.ends_with(".service")).collect();
    let unord: Vec<_> = svc.iter().filter(|n| store_unordered(n, &raw(n))).collect();
    let short: Vec<_> = svc.iter().filter(|n| store_short_retry(n, &raw(n))).collect();
    assert!(unord.is_empty(), "not After=/Wants= lc-serve: {unord:?}");
    assert!(short.is_empty(), "retry shorter than a {STORE_LATE}s-late store: {short:?}");
}

fn assignments_before_first_section(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .take_while(|l| !l.starts_with('['))
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with(';'))
        .map(String::from)
        .collect()
}

#[test]
fn no_unit_assigns_anything_above_its_first_section_header() {
    let planted = "After=x.service\n[Unit]\nDescription=d\n";
    assert_eq!(assignments_before_first_section(planted), ["After=x.service"]);
    let bad: Vec<String> = templates().into_iter().flat_map(|n| assignments_before_first_section(&raw(&n)).into_iter().map(move |l| format!("{n}: {l}"))).collect();
    assert!(bad.is_empty(), "assignment outside a section: {bad:?}");
}
