//! The mail a watchd row sent with `mail send <mailbox> --from … --subject … --kind …` and a
//! `## <Kind>\n<line>` body, as one effect input and one in-process runner on `mail::cmds::send`.
//! `operator.note` and `concierge_note` fix their kind and From, so neither can carry these.

use runtime::event::EffectSpec;
use serde::{Deserialize, Serialize};

use crate::Effects;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alert {
    pub mailbox: String,
    pub from: String,
    pub subject: String,
    pub kind: String,
    pub body: String,
}

impl Alert {
    /// `<prefix>: <line>` under `## <Heading>`, the shape every such script wrote.
    pub fn of(mailbox: &str, from: &str, prefix: &str, kind: &str, line: &str) -> Alert {
        let mut heading = kind.to_string();
        if let Some(c) = heading.get_mut(0..1) {
            c.make_ascii_uppercase();
        }
        Alert { mailbox: mailbox.into(), from: from.into(), subject: format!("{prefix}: {line}"), kind: kind.into(), body: format!("## {heading}\n{line}\n") }
    }

    pub fn effect(&self, name: &str, key: String, deadline_ms: u64) -> EffectSpec {
        EffectSpec::keyed(name, key, serde_json::to_string(self).unwrap_or_default(), deadline_ms)
    }
}

/// Sends `input` (an [`Alert`]) under the service's mail environment, without a tracking bead.
pub fn send(e: &Effects, input: &str) -> Result<String, String> {
    let a: Alert = serde_json::from_str(input).map_err(|e| format!("alert: bad input: {e}"))?;
    if a.subject.trim().is_empty() || a.mailbox.is_empty() {
        return Err("alert: an empty subject or mailbox is refused".into());
    }
    let env = e.concierge_mail.as_deref().ok_or("no mail environment configured")?;
    let args = mail::cmds::SendArgs { mailbox: &a.mailbox, from: Some(&a.from), subject: &a.subject, kind: &a.kind, default: "", class: "", bead: "", urgent: false, digest: false, dry_run: false, requested_by: "" };
    let bd = mail::bead::BdCli { bdq: String::new(), conn_retries: 1 };
    mail::cmds::send(&bd, env, &args, a.body.clone()).map(|o| o.delivered_path.display().to_string())
}
