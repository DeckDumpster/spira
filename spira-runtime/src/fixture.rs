//! Settings built the way the service builds them, from test values none of which is a shipped default.

use crate::settings::Settings;

/// A complete `Settings` from test values, none of them a shipped default.
pub fn fixture() -> Settings {
    Settings::from_lookup(&lookup("7")).expect("fixture settings")
}

/// Resolves this test process's config door (`$SPIRA_TOML`, `$SPIRA_HOME`) over the complete
/// fixture, once, before anything reads it: a ported row whose `build` pins its keys through
/// `spira_config::process::cfg` (family A's `kit::pin`) then builds under `from_lookup` in a
/// test as it does in the service.
pub fn fixture_config_door() {
    static DOOR: std::sync::OnceLock<testkit::TempDir> = std::sync::OnceLock::new();
    DOOR.get_or_init(|| {
        let dir = testkit::TempDir::new("runtime-settings-door");
        let run = dir.path().join("run");
        std::fs::create_dir_all(&run).expect("the fixture run directory");
        let toml = spira_config::process::fixture_toml(dir.path(), &[("SPIRA_LC_SOCKET", "d/sock"), ("SPIRA_RUN", &run.to_string_lossy())]);
        let home = spira_config::resolve::locate_home_for_process().unwrap();
        // The door resolves once per process and keeps what it read, so the environment
        // needs to name the fixture only while it does.
        let _env = testkit::env(&[("SPIRA_TOML", Some(&toml.to_string_lossy())), ("SPIRA_HOME", Some(&home.to_string_lossy()))]);
        spira_config::process::config().expect("the complete fixture resolves");
        dir
    });
}

/// The unit-test binary opens the door before `main`: the process resolves its config once and
/// keeps a refusal, so a test that reads config ahead of the fixture would poison every later one.
#[cfg(all(test, target_os = "linux"))]
#[used]
#[link_section = ".init_array"]
static OPEN_DOOR_BEFORE_MAIN: extern "C" fn() = {
    extern "C" fn open() {
        fixture_config_door();
    }
    open
};

pub fn lookup(secs: &'static str) -> impl Fn(&str) -> Result<String, String> {
    fixture_config_door();
    move |k| {
        if k == crate::modes::KEY {
            return Ok(fixture_modes());
        }
        Ok(match k {
            "SPIRA_RUNTIME_HEARTBEAT_URL" => "http://127.0.0.1:9/ping",
            "SPIRA_RUNTIME_HEARTBEAT_SECS" => secs,
            "SPIRA_RUNTIME_STALL_LIMIT" => "3",
            "SPIRA_RUNTIME_SWEEP_SECS" => "7",
            "SPIRA_METRICS_ENDPOINT" => "127.0.0.1:9",
            "SPIRA_RUN" => "r",
            "SPIRA_LC_LIFECYCLE_ON_STORE" => "1",
            "SPIRA_LC_SOCKET" => "d/sock",
            "SPIRA_GH_API" => "http://forge",
            "SPIRA_BROKER_GH_TOKEN" | "SPIRA_BROKER_GH_CONFIG_DIR" | "SPIRA_GH" => "",
            "SPIRA_RUNTIME_INBOX_KEEPER_SECS" | "SPIRA_RUNTIME_PR_NOTIFY_SECS" | "SPIRA_RUNTIME_CR_CADENCE_SECS" | "SPIRA_RUNTIME_ASKS_SECS" | "SPIRA_RUNTIME_SUPERSEDE_DUTY_SECS" | "SPIRA_RUNTIME_CLAIM_COUNT_SECS" | "SPIRA_CLAIM_PAGE_EVERY" | "SPIRA_RUNTIME_DESIGN_ASKS_SECS" | "SPIRA_RUNTIME_BRAIN_DIRTY_SECS" | "SPIRA_RUNTIME_WRITER_GRANTS_SECS" | "SPIRA_BRAIN_DIRTY_AGE" | "SPIRA_DESIGN_ASKS_REPEAT" | "SPIRA_RUNTIME_PUBLISH_BACKLOG_SECS" | "SPIRA_RUNTIME_PUBLISH_BACKLOG_RECHECK_SECS" | "SPIRA_RUNTIME_OVERRIDES_SECS" | "SPIRA_RUNTIME_READINESS_DEPENDENTS_SECS" | "SPIRA_RUNTIME_VIEW_SECS" | "SPIRA_LOCAL_BACKLOG_COUNT" | "SPIRA_LOCAL_BACKLOG_AGE" | "SPIRA_RUNTIME_BEAD_SWEEP_SECS" | "SPIRA_BEAD_SWEEP_HOLD_MIN" | "SPIRA_BEAD_SWEEP_REWORK_MIN" | "SPIRA_BEAD_SWEEP_CERT_MIN" | "SPIRA_BEAD_SWEEP_WAIT_MIN" | "SPIRA_BEAD_SWEEP_BATCH_MIN" | "SPIRA_SCOPED_STOP_REVIEW_SECS" | "SPIRA_PR_DEPLOY_GIVEUP" | "SPIRA_CONCIERGE_INBOX_STALL" | "SPIRA_CONCIERGE_INBOX_BACKOFF" | "SPIRA_CONCIERGE_WAKE_TRIES" | "SPIRA_RUNTIME_ROUND_DUTY_SECS" | "SPIRA_ROUND_MIN" | "SPIRA_ROUND_MAX_OPEN" | "SPIRA_ROUND_DUE_REMIND" | "SPIRA_ROUND_IDLE_CUT" => "5",
            "SPIRA_WIKI" | "SPIRA_MAIL" | "SPIRA_ASK_LABEL" | "SPIRA_QUEUE_DIR" | "SPIRA_REPO_MAP" | "SPIRA_CONCIERGE_INBOX" => "q",
            "SPIRA_HOME_REPO" => "spira",
            "SPIRA_CONCIERGE_PUSH_WAKE" => "bin/wake",
            "SPIRA_CONCIERGE_PUSH_PATTERN" => r"\bP0\b",
            "SPIRA_RUNTIME_RECONCILER_FLOW_SECS" | "SPIRA_FLOW_GRACE_SECS" | "SPIRA_FLOW_UNOBSERVABLE_GRACE_SECS" | "SPIRA_FLOW_SENTINEL_PERIOD_SECS" => "5",
            "SPIRA_FLOW_WINDOW_HOURS" | "SPIRA_FLOW_BASELINE_HOURS" | "SPIRA_FLOW_REWORK_WINDOW_HOURS" => "0.5",
            "SPIRA_SCOPE_LABEL" => "q",
            "SPIRA_VIEW" => "bin/view",
            "SPIRA_VIEW_SESSION" => "pit",
            "SPIRA_RUNTIME_SOCKET" => "d/control.sock",
            "SPIRA_VICTORIALOGS_URL" => "http://127.0.0.1:19428",
            "SPIRA_POOL_CAP_STORE" => "41",
            "SPIRA_POOL_CAP_UNIT" => "42",
            "SPIRA_POOL_CAP_GIT" => "43",
            "SPIRA_POOL_CAP_FORGE" => "44",
            "SPIRA_POOL_CAP_HOST" => "45",
            "SPIRA_POOL_CAP_CONTAINER" => "2",
            "SPIRA_RUNTIME_BROKER_CAP" => "46",
            k if reconciler_fixture(k).is_some() => reconciler_fixture(k).unwrap_or_default(),
            "BD_TIMEOUT" => "30",
            "SPIRA_COCKPIT_WINDOW_HOURS" => "24",
            "SPIRA_LC_BIN" => "spira-lc",
            k if row_declares(k) => return complete_fixture(k).ok_or_else(|| format!("{k} has no value in spira/testlib/complete.toml")),
            _ => return Err(format!("{k} unknown")),
        }
        .to_string())
    }
}

/// Whether a row in `tasks::all()` or `tasks::pending()` declares `k`: the keys whose value is
/// the complete fixture's, so moving a row from `PENDING` to `TASKS` keeps its keys resolved.
fn row_declares(k: &str) -> bool {
    crate::sources::KEYS.contains(&k) || crate::tasks::all().chain(crate::tasks::pending()).any(|t| t.keys.iter().any(|ks| ks.contains(&k)))
}

/// The fixture's task modes: `asks` and `view` observe, and every other task, ported or
/// pending, acts, so a row moved from `PENDING` to `TASKS` is already named here.
fn fixture_modes() -> String {
    let mode = |n: &str| if n == "asks" || n == "view" { "observe" } else { "act" };
    crate::tasks::all().chain(crate::tasks::pending()).map(|t| format!("{}={}", t.name, mode(t.name))).collect::<Vec<_>>().join(" ")
}

/// `key`'s value under `[spira]` in the complete fixture (`spira/testlib/complete.toml`), its
/// quotes stripped: the fixture values of the keys pending rows declare live there, once.
pub fn complete_fixture(key: &str) -> Option<String> {
    static MEMO: std::sync::Mutex<Option<(String, std::sync::Arc<toml::Table>)>> = std::sync::Mutex::new(None);
    let text = spira_config::process::complete_fixture_text();
    let doc = {
        let mut memo = MEMO.lock().unwrap_or_else(|e| e.into_inner());
        parse_memo(&mut memo, text, |t| t.parse().expect("complete.toml parses"))
    };
    value_of(&doc, key)
}

fn parse_memo(memo: &mut Option<(String, std::sync::Arc<toml::Table>)>, text: String, parse: impl FnOnce(&str) -> toml::Table) -> std::sync::Arc<toml::Table> {
    match memo {
        Some((seen, doc)) if *seen == text => doc.clone(),
        _ => {
            let doc = std::sync::Arc::new(parse(&text));
            *memo = Some((text, doc.clone()));
            doc
        }
    }
}

fn value_of(doc: &toml::Table, key: &str) -> Option<String> {
    let field = spira_config::registry::field_of(key);
    match doc.get("spira")?.get(field)? {
        toml::Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// Values for the keys the reconciler reads; none is the shipped default.
pub fn reconciler_fixture(k: &str) -> Option<&'static str> {
    Some(match k {
        "SPIRA_RUNTIME_RECONCILER_SECS" => "7",
        "COCKPIT_SESSIONS" => "one two",
        "COCKPIT_MAIL" | "SPIRA_RECONCILER_SHADOW_KINDS" | "SPIRA_DOLT_DATA" | "SPIRA_MAX_LIVE_DWARVES" | "SPIRA_MAX_DWARVES" | "SPIRA_LIMIT_CPU" | "SPIRA_LIMIT_MEMORY" | "SPIRA_LIMIT_NETWORK" => "",
        "SPIRA_RELEASES" | "SPIRA_CTRL" | "SPIRA_DESIRED_DIR" | "SPIRA_RUNES" | "SPIRA_LC_PASSWORD_FILE" => "q",
        "SPIRA_INSTANCE" => "prod",
        "SPIRA_RECONCILER_EFFECT_PASSES" | "SPIRA_NO_PROGRESS_BREAKER_CLAIMS" | "SPIRA_NO_PROGRESS_BREAKER_WINDOW_MINUTES" => "3",
        "SPIRA_DISK_FLOOR_PCT" => "11",
        "SPIRA_UNTAGGED_IMAGES_MAX" => "19",
        "SPIRA_RECONCILER_GRACE_SECS" => "13",
        "SPIRA_MAIN_HEALTH_ACTIVATION_GRACE_SECS" | "SPIRA_MAIN_HEALTH_FENCE_DEADLINE_SECS" | "SPIRA_PREFLIGHT_WALL_SECS" | "SPIRA_STRANDED_RUNNER_MIN_AGE" => "17",
        _ => return None,
    })
}

#[cfg(test)]
mod door_tests {
    #[test_id(thoeu42)]
    #[test]
    fn the_config_door_is_open_for_a_test_that_never_asks_the_fixture() {
        spira_config::process::config().expect("the door resolved before this test ran");
    }
}

#[cfg(test)]
mod memo_tests {
    use super::*;

    #[test_id(tmemo01)]
    #[test]
    fn the_complete_fixture_parses_once_per_text_however_many_keys_ask() {
        let parses = std::cell::Cell::new(0);
        let parse = |t: &str| {
            parses.set(parses.get() + 1);
            t.parse::<toml::Table>().unwrap()
        };
        let mut memo = None;
        let a = parse_memo(&mut memo, "[spira]\nx = \"1\"\ny = \"2\"\n".into(), parse);
        let b = parse_memo(&mut memo, "[spira]\nx = \"1\"\ny = \"2\"\n".into(), parse);
        assert_eq!((value_of(&a, "SPIRA_X"), value_of(&b, "SPIRA_Y")), (Some("1".into()), Some("2".into())));
        assert_eq!(parses.get(), 1);
        parse_memo(&mut memo, "[spira]\nx = \"3\"\n".into(), parse);
        assert_eq!(parses.get(), 2);
    }
}
