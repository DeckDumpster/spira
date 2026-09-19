use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use serde_json::json;
use crate::policy::Verb;

pub struct Args {
    pub verb: Verb,
    pub repo: String,
    pub number: String,
    pub reason: String,
    pub bead: String,
    pub class: Option<String>,
}

pub fn run(args: Args) -> Result<(), String> {
    let run_dir = std::env::var("SPIRA_RUN")
        .map_err(|_| "broker submit: SPIRA_RUN is not set".to_string())?;
    let fayth = std::env::var("FAYTH_NAME").unwrap_or_default();
    let aeon  = std::env::var("SPIRA_AEON").unwrap_or_default();

    let inbox = Path::new(&run_dir).join("broker").join("inbox");
    std::fs::create_dir_all(&inbox)
        .map_err(|e| format!("broker submit: cannot create inbox: {e}"))?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let id = format!("{}-{}", now, std::process::id());

    let intent = json!({
        "id":           id,
        "verb":         args.verb.as_str(),
        "repo":         args.repo,
        "number":       args.number,
        "reason":       args.reason,
        "bead":         args.bead,
        "fayth":        fayth,
        "aeon":         aeon,
        "class":        args.class,
        "submitted_at": now,
    });

    let path = inbox.join(format!("{}.json", id));
    std::fs::write(&path, intent.to_string())
        .map_err(|e| format!("broker submit: cannot write intent: {e}"))?;

    println!("{}", id);
    Ok(())
}
