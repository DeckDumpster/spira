//! `mail send <mailbox> --from … --subject … --kind … --default …` in-process, over the mail
//! library: what the pass's asks, CHECK 1's notices and judgement's escalation send.

/// One message.
pub struct Mail<'a> {
    pub mailbox: &'a str,
    pub from: &'a str,
    pub subject: &'a str,
    /// `question` or `note`.
    pub kind: &'a str,
    pub default: &'a str,
    pub body: &'a str,
    pub bead: &'a str,
    /// The sender dedupes itself (`repeat_considered`), so mail's own repeat check stands aside.
    pub own_dedup: bool,
}

/// Sends `m`, recording the attempt; `Err` names why it was not accepted.
pub fn send(m: &Mail) -> Result<(), String> {
    let mut env = mail::env::Env::load()?;
    if m.own_dedup {
        env.repeat_considered = Some("sentinel-own-dedup".into());
    }
    mail::maildir::mailbox_valid(m.mailbox).map_err(|e| format!("mailbox {}: {e}", m.mailbox))?;
    let bdq = spira_config::release_env::tool("bdq")?;
    let bd = mail::bead::BdCli { bdq, conn_retries: env.bd_conn_retries };
    let args = mail::cmds::SendArgs { mailbox: m.mailbox, from: Some(m.from), subject: m.subject, kind: m.kind, default: m.default, class: "", bead: m.bead, urgent: false, digest: false, dry_run: false, requested_by: "" };
    let body = m.body.to_string();
    let result = mail::cmds::send(&bd, &env, &args, body.clone());
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    if let Some(a) = mail::cmds::attempt_of_send(&args, &body, &result, now) {
        let _ = a.write(&env.run_dir, &tsd::default_host());
    }
    result.map(|_| ()).map_err(|e| e.to_string())
}
