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

// Pure so the field set can be asserted without env vars or a filesystem — the shape IS
// the contract execute.rs reads back (as untyped serde_json::Value, so a typo in a field
// name here fails silently there rather than as a compile error).
fn build_intent(args: &Args, fayth: &str, aeon: &str, now: u64, id: &str) -> serde_json::Value {
    json!({
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
    })
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

    let intent = build_intent(&args, &fayth, &aeon, now, &id);

    let path = inbox.join(format!("{}.json", id));
    std::fs::write(&path, intent.to_string())
        .map_err(|e| format!("broker submit: cannot write intent: {e}"))?;

    println!("{}", id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_args() -> Args {
        Args {
            verb: Verb::RunRerun,
            repo: "test-repo".to_string(),
            number: "12345".to_string(),
            reason: "test reason".to_string(),
            bead: "sp-test".to_string(),
            class: Some("deadlock".to_string()),
        }
    }

    #[test]
    fn intent_carries_every_field_the_executor_reads() {
        let intent = build_intent(&sample_args(), "czar", "test-aeon", 1_700_000_000, "abc-1");
        assert_eq!(intent["id"], "abc-1");
        assert_eq!(intent["verb"], "run-rerun");
        assert_eq!(intent["repo"], "test-repo");
        assert_eq!(intent["number"], "12345");
        assert_eq!(intent["reason"], "test reason");
        assert_eq!(intent["bead"], "sp-test");
        assert_eq!(intent["fayth"], "czar");
        assert_eq!(intent["aeon"], "test-aeon");
        assert_eq!(intent["class"], "deadlock");
        assert_eq!(intent["submitted_at"], 1_700_000_000);
    }

    #[test]
    fn a_class_less_verb_serializes_class_as_null_not_absent() {
        let mut args = sample_args();
        args.class = None;
        let intent = build_intent(&args, "builder", "a", 1, "id-2");
        // execute.rs reads intent["class"].as_str() and treats a missing key the same as
        // null, but the field must still be PRESENT — an absent key vs. an explicit null
        // is exactly the ambiguity a stray-brace parser bug hid elsewhere in this area.
        assert!(intent.get("class").is_some());
        assert!(intent["class"].is_null());
    }
}
