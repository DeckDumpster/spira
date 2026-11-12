//! The trigger half: filing the scheduled persona beads the scheduler's trigger tasks decide on
//! (`trigger.groom`, `trigger.warden`, `trigger.mimir`). The bead and its lifecycle row are
//! written together, so no trigger is left filed but unclaimable.

use std::io::Write;
use std::process::Stdio;

use spira_store::lc_call::LcClient;

/// One trigger bead: the fields `bead.sh file` / `bdq create` write today.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filing<'a> {
    pub title: &'a str,
    pub kind: &'a str,
    pub priority: u8,
    pub labels: &'a [String],
    pub body: &'a str,
    /// A persona bead is filed through `bead file --for <persona>`, which writes the rune's
    /// labels and the lifecycle row itself.
    pub persona: Option<&'a str>,
    pub repo: Option<&'a str>,
}

/// The argv the filing runs, after the program: `bead file …` for a persona bead, else `bdq
/// create …`; the body always goes on stdin.
pub fn argv(f: &Filing) -> Vec<String> {
    let mut a: Vec<String> = match f.persona {
        Some(p) => vec!["file".into(), f.title.into(), "--for".into(), p.into()],
        None => vec!["create".into(), f.title.into(), "--type".into(), f.kind.into(), "--label".into(), f.labels.join(","), "--silent".into()],
    };
    if let (Some(_), Some(r)) = (f.persona, f.repo) {
        a.extend(["--repo".into(), r.into()]);
    }
    a.extend(["--priority".into(), f.priority.to_string(), "--body-file".into(), "-".into()]);
    a
}

/// The id a filing printed: its last non-empty line, trimmed.
pub fn printed_id(out: &str) -> Option<String> {
    out.lines().map(str::trim).rfind(|l| !l.is_empty()).map(str::to_string)
}

/// Files `f` and returns its id. A persona bead then gets the labels `bead file` did not write
/// over the content door; any other bead gets its lifecycle row over the store's door.
pub fn file(lc: &LcClient, f: &Filing) -> Result<String, String> {
    let (tool, stdout) = match f.persona {
        Some(_) => ("bead", file_persona(f)?),
        None => {
            // The store's content door: every protection bdq applies, the body as the request body.
            let a: Vec<String> = std::iter::once("content".to_string()).chain(argv(f)).collect();
            let r = lc.call_stdin(&a, f.body).map_err(|e| format!("content create: {e}"))?;
            if r.code != 0 {
                return Err(format!("content create exited {}: {}", r.code, r.why()));
            }
            ("content create", r.out)
        }
    };
    let id = printed_id(&stdout).ok_or_else(|| format!("{tool} printed no id"))?;
    if f.persona.is_some() {
        let mut a: Vec<String> = vec!["content".into(), "update".into(), id.clone()];
        for l in f.labels {
            a.extend(["--add-label".into(), l.clone()]);
        }
        if f.labels.is_empty() {
            return Ok(id);
        }
        let r = lc.call(&a).map_err(|e| format!("{id} filed but its labels were not added: {e}"))?;
        if r.code != 0 {
            return Err(format!("{id} filed but its labels were not added: {}", r.why()));
        }
        return Ok(id);
    }
    let mut row = vec!["create-bead".to_string(), id.clone(), "--ready".into()];
    if let Some(r) = f.repo {
        row.extend(["--repo".into(), r.into()]);
    }
    let r = lc.call(&row).map_err(|e| format!("{id} filed but its lifecycle row was not created: {e}; repair: spira-lc create-bead {id} --ready"))?;
    if r.code != 0 {
        return Err(format!("{id} filed but its lifecycle row was not created: {}; repair: spira-lc create-bead {id} --ready", r.why()));
    }
    Ok(id)
}

/// A persona bead through `bead file --for <persona>`: the filing tool's own contract (the
/// rune's labels, lane admission), which the content door does not apply. Its stdout.
fn file_persona(f: &Filing) -> Result<String, String> {
    let program = spira_config::release_env::tool("bead")?;
    // FORK-EDGE: `bead file` is the operator filing tool — its contract (rune labels, lane admission) is not the content door's
    let mut child = spira_config::bounded::bounded(&program).args(argv(f)).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("cannot run bead: {e}"))?;
    child.stdin.take().ok_or("no stdin")?.write_all(f.body.as_bytes()).map_err(|e| format!("bead: writing the body: {e}"))?;
    let out = child.wait_with_output().map_err(|e| format!("bead: {e}"))?;
    if !out.status.success() {
        return Err(format!("bead exited {}: {}", out.status, String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("")));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The store as a scripted transport: records each request and its body.
    struct Script(std::sync::Mutex<Vec<(Vec<String>, Option<String>)>>);

    impl spira_store::lc_call::Transport for Script {
        fn exchange(&self, argv: &[String], stdin: Option<&str>, _: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
            self.0.lock().unwrap().push((argv.to_vec(), stdin.map(str::to_string)));
            let out = if argv.first().map(String::as_str) == Some("content") { "sp-new\n".to_string() } else { String::new() };
            Ok(spira_store::lc_call::Reply { code: 0, out, err: String::new() })
        }
    }

    #[test]
    fn a_plain_trigger_is_filed_through_the_content_door_and_forks_nothing() {
        let l = labels();
        let f = Filing { title: "T", kind: "task", priority: 3, labels: &l, body: "the body", persona: None, repo: Some("spira") };
        let script = std::sync::Arc::new(Script(Default::default()));
        let lc = LcClient::over(script.clone(), true);
        assert_eq!(file(&lc, &f), Ok("sp-new".to_string()));
        let calls = script.0.lock().unwrap();
        assert_eq!(calls[0].0, ["content", "create", "T", "--type", "task", "--label", "spira,warden-sweep", "--silent", "--priority", "3", "--body-file", "-"]);
        assert_eq!(calls[0].1.as_deref(), Some("the body"), "the body is the request body");
        assert_eq!(calls[1].0, ["create-bead", "sp-new", "--ready", "--repo", "spira"], "then its row, with no planning field");
    }

    fn labels() -> Vec<String> {
        vec!["spira".into(), "warden-sweep".into()]
    }

    #[test]
    fn a_plain_trigger_is_a_bdq_create_with_its_labels_and_the_body_on_stdin() {
        let l = labels();
        let f = Filing { title: "T", kind: "task", priority: 3, labels: &l, body: "b", persona: None, repo: None };
        assert_eq!(argv(&f), ["create", "T", "--type", "task", "--label", "spira,warden-sweep", "--silent", "--priority", "3", "--body-file", "-"]);
    }

    #[test]
    fn a_persona_trigger_is_a_bead_file_for_the_persona_and_repo() {
        let l = labels();
        let f = Filing { title: "T", kind: "task", priority: 3, labels: &l, body: "b", persona: Some("groomer"), repo: Some("spira") };
        assert_eq!(argv(&f), ["file", "T", "--for", "groomer", "--repo", "spira", "--priority", "3", "--body-file", "-"]);
    }

    #[test]
    fn the_id_is_the_last_line_printed() {
        assert_eq!(printed_id("note\nsp-ab12\n\n"), Some("sp-ab12".into()));
        assert_eq!(printed_id("  \n"), None);
    }
}
