# Mail

## Intent

Everything addressed to the operator, and everything the operator sends back, is **mail**.
Senders post to mailboxes; a delivery daemon tells a reader it has mail; the operator reads and
answers in an ordinary mail client. Whether a message is also tracked as a bead is an
implementation detail the operator never sees (`law-operator-mail-is-not-about-beads`).

This replaces, rather than adds to: the answers watcher and its cursor, the Monitor latch a
session had to keep re-arming, the per-watcher unread and health escalations, and the
attention pane.

## Out of scope

- IMAP or SMTP servers. The store is Maildir so that a server can be added later without
  changing anything downstream; adding one is a separate decision.
- Mail between aeons. An aeon is gone by the time an answer arrives; a verdict that gates work
  reaches that work through the bead graph (closing the ask unblocks its dependents).

## The store

One Maildir per mailbox under `SPIRA_MAIL` (default `$SPIRA_RUN/mail`):

    $SPIRA_MAIL/<mailbox>/{tmp,new,cur}

Two mailboxes: `operator` and `concierge`. A message is an RFC 5322 file. Delivery is write to
`tmp/`, then rename into `new/` — atomic, so a reader never sees a partial message. **Unread is
a file in `new/`**; reading moves it to `cur/`. There is no cursor file.

Headers Spira sets:

| header | meaning |
|---|---|
| `From` | the sender in plain words: `Landing gate <gate@spira>` |
| `Subject` | what happened or what is being decided |
| `X-Spira-Kind` | `decision`, `question`, `note`, `suit` |
| `X-Spira-Default` | the sender's recommendation, for `decision` and `question` |
| `X-Spira-Bead` | the tracking bead, if any — for machinery, never shown as the subject |
| `In-Reply-To` | on replies, the message answered |

## `spira/mail.sh`

    mail.sh send <mailbox> --from "<sender>" --subject "<s>" [--kind K] [--default D] [--bead ID] < body
    mail.sh list <mailbox> [--unread]
    mail.sh read <mailbox> [<message>]      prints, and moves new -> cur
    mail.sh unread-age <mailbox>            seconds since the oldest unread message; empty if none
    mail.sh sendmail                        RFC 5322 on stdin — the mail client's outgoing command

**Send refuses** a message with no sender, no subject, a subject that is or leads with a bead
id, a body that names a bead id without the words around it saying what the work is, or a
`decision`/`question` with no default. Each refusal names the rule. Override:
`SPIRA_MAIL_LINT_CONSIDERED=1`.

## Delivery

`spira-mail-deliver.service` watches `concierge/new` with `inotifywait`. On an arrival it
waits `SPIRA_MAIL_SETTLE` (default 2s) for a burst to finish, then runs
`$SPIRA_WAKE "You have mail — run mail.sh read concierge."`. If the session is not running
the wake fails quietly; the mail stays unread.

Fresh context: the session hook prints the concierge's unread mail (bounded by
`SPIRA_HOOK_LINES`) and marks nothing read. It prints no Monitor instructions.

## Health

One check, on the existing notify timer: `mail.sh unread-age concierge` above
`SPIRA_MAIL_UNREAD_AGE` (default 1800) sends the operator one message — "The concierge is not
reading its mail" — once per distinct backlog.

## The operator's side

- **Sending to the operator.** `ask.sh add|decide|insight|suit` post to `operator` with a
  plain-language sender, and still open the tracking bead where one is needed, linked by
  `X-Spira-Bead`. Machinery that escalates (incident, watchd, suites) names itself as a sender.
- **Reading.** aerc on the `operator` Maildir. The harness ships an example
  `aerc/accounts.conf` and `aerc/binds.conf`; `outgoing` is `mail.sh sendmail`.
- **Answering.** A reply is delivered by `mail.sh sendmail`: it closes the tracking bead with
  the reply's first paragraph as the verdict (or executes `uphold` / `retire` / `amend: …` for
  a suit), then delivers the reply to `concierge`. A key binding sends "accept default".

## Retirement

Only after delivery, the hook and replies have each run supervised
(`law-arm-before-you-retire`): remove the answers watcher from the manifest, the attention
pane from the cockpit layout, and the Monitor guidance from the concierge brief.

## Dependencies

`inotify-tools` and `aerc` are packages; `doctor.sh` reports them missing and never installs.
