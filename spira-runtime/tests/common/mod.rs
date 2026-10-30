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
            "SPIRA_GH_API" => "http://127.0.0.1:9".into(),
            "SPIRA_BROKER_GH_TOKEN" => "".into(),
            "SPIRA_RUNTIME_INBOX_KEEPER_SECS" | "SPIRA_RUNTIME_PR_NOTIFY_SECS" | "SPIRA_RUNTIME_ASKS_SECS" | "SPIRA_RUNTIME_SUPERSEDE_DUTY_SECS" | "SPIRA_RUNTIME_DESIGN_ASKS_SECS" | "SPIRA_RUNTIME_BRAIN_DIRTY_SECS" | "SPIRA_BRAIN_DIRTY_AGE" | "SPIRA_DESIGN_ASKS_REPEAT" | "SPIRA_RUNTIME_PUBLISH_BACKLOG_SECS" | "SPIRA_LOCAL_BACKLOG_COUNT" | "SPIRA_LOCAL_BACKLOG_AGE" | "SPIRA_RUNTIME_BEAD_SWEEP_SECS" | "SPIRA_BEAD_SWEEP_HOLD_MIN" | "SPIRA_BEAD_SWEEP_REWORK_MIN" | "SPIRA_BEAD_SWEEP_CERT_MIN" | "SPIRA_BEAD_SWEEP_WAIT_MIN" | "SPIRA_BEAD_SWEEP_BATCH_MIN" | "SPIRA_SCOPED_STOP_REVIEW_SECS" | "SPIRA_PR_DEPLOY_GIVEUP" | "SPIRA_CONCIERGE_INBOX_STALL" | "SPIRA_CONCIERGE_INBOX_BACKOFF" | "SPIRA_RUNTIME_ROUND_DUTY_SECS" | "SPIRA_ROUND_MIN" | "SPIRA_ROUND_MAX_OPEN" | "SPIRA_ROUND_DUE_REMIND" | "SPIRA_ROUND_IDLE_CUT" => "5".into(),
            "SPIRA_WIKI" | "SPIRA_MAIL" | "SPIRA_ASK_LABEL" | "SPIRA_QUEUE_DIR" | "SPIRA_REPO_MAP" | "SPIRA_CONCIERGE_INBOX" => "/unused".into(),
            "SPIRA_HOME_REPO" => "spira".into(),
            _ => return Err(k.to_string()),
        })
    };
    Settings::from_lookup(&get).unwrap()
}
