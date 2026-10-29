use spira_runtime::Settings;

pub fn settings(url: &str, run: &std::path::Path) -> Settings {
    let get = |k: &str| -> std::result::Result<String, String> {
        Ok(match k {
            "SPIRA_RUNTIME_HEARTBEAT_URL" => url.to_string(),
            "SPIRA_RUNTIME_HEARTBEAT_SECS" => "1".into(),
            "SPIRA_RUNTIME_STALL_LIMIT" => "2".into(),
            "SPIRA_METRICS_ENDPOINT" => String::new(),
            "SPIRA_RUN" => run.display().to_string(),
            "SPIRA_LC_SOCKET" => "/unused".into(),
            "SPIRA_RUNTIME_INBOX_KEEPER_SECS" | "SPIRA_CONCIERGE_INBOX_STALL" | "SPIRA_CONCIERGE_INBOX_BACKOFF" | "SPIRA_RUNTIME_ROUND_DUTY_SECS" | "SPIRA_ROUND_MIN" | "SPIRA_ROUND_MAX_OPEN" | "SPIRA_ROUND_DUE_REMIND" | "SPIRA_ROUND_IDLE_CUT" => "5".into(),
            "SPIRA_QUEUE_DIR" | "SPIRA_REPO_MAP" | "SPIRA_CONCIERGE_INBOX" => "/unused".into(),
            "SPIRA_HOME_REPO" => "spira".into(),
            _ => return Err(k.to_string()),
        })
    };
    Settings::from_lookup(&get).unwrap()
}
