---
status: approved
---

# Mail

## Intent

Everything addressed to the operator, and everything the operator sends back, is **mail**.
Senders post to mailboxes; a delivery daemon tells a reader it has mail; the operator reads and
answers in an ordinary mail client. Whether a message is also tracked as a bead is an
implementation detail the operator never sees (`law-operator-mail-is-not-about-beads`).

This replaces, rather than adds to: `ask.sh`, the answers watcher and its cursor, the Monitor
latch a session had to keep re-arming, the per-watcher unread and health escalations, and the
attention pane.

## Out of scope

- IMAP or SMTP servers. The store is Maildir so that a server can be added later without
  changing anything downstream; adding one is a separate decision.
- A mailbox for an aeon. An aeon is gone by the time an answer arrives; a verdict that gates
  its work reaches it through the bead graph (closing the ask unblocks its dependents).
  Long-lived actors — the sentinel, Auron, the fayth — may have mailboxes; nothing here may
  assume they do not.
- A compatibility layer for `ask.sh`. It is retired, not wrapped.

## The store

One Maildir per mailbox under `SPIRA_MAIL` (default `$SPIRA_RUN/mail`):

    $SPIRA_MAIL/<mailbox>/{tmp,new,cur}

**A mailbox is any directory there; nothing enumerates them.** `operator` and `concierge` are
the first two. A message is an RFC 5322 file. Delivery is write to `tmp/`, then rename into
`new/` — atomic, so a reader never sees a partial message. **Unread is a file in `new/`**;
reading moves it to `cur/`. There is no cursor file.

Headers Spira sets:

| header | meaning |
|---|---|
| `From` | the sender in plain words: `Landing gate <gate@spira>` |
| `Subject` | what happened or what is being decided |
| `X-Spira-Kind` | the kind; selects the schema the message was validated against |
| `X-Spira-Urgent` | present when the sender claims urgency |
| `X-Spira-Bead` | the tracking bead, if any — for machinery, never shown as the subject |
| `X-Spira-Lint-Override` | present when the sender bypassed validation, with who and why |
| `In-Reply-To` | on replies, the message answered |

## Kinds are schemas, and they are data

Each kind is one file, `spira/mail/kinds/<kind>.md`:

- a header block: the headers the kind requires, and whether sending it opens a tracking bead;
- a body template whose `##` headings are the sections a message of that kind must fill.

The first kinds are `decision` (Decision / Default / What is blocked / Cost of the wrong
choice), `question`, `note` and `suit`. An urgent message of any kind must also fill
`## Why it is urgent`. Tightening a kind the operator finds unhelpful is an edit to its file,
never to code.

## `spira/mail.sh` — the one door

    mail.sh send <mailbox> --from "<sender>" --subject "<s>" --kind K [--urgent] [--bead ID] < body
    mail.sh template <kind>                 print the kind's body skeleton to fill
    mail.sh list <mailbox> [--unread]       one line per message: sender, subject, age
    mail.sh read <mailbox> [<message>]      prints, and moves new -> cur
    mail.sh count <mailbox>                 number of unread messages
    mail.sh unread-age <mailbox>            seconds since the oldest unread message; empty if none
    mail.sh sendmail                        RFC 5322 on stdin — the mail client's outgoing command

**Send refuses**, naming the rule:

- no sender, no subject, a subject that is or leads with a bead id, or a body naming a bead id
  without the words around it saying what the work is — for every kind;
- an unknown kind, a missing required header, or an empty required section of the kind's
  template.

Override: `SPIRA_MAIL_LINT_CONSIDERED=<reason>`, recorded in `X-Spira-Lint-Override` so
bypasses are visible and countable.

**A bead named by `--bead`, or found anywhere in the subject or body, is rendered.** `send`
resolves it against the store and opens the body with a block — title, status, priority,
type, repo, branch, and the close reason or last note — before the producer's own prose. An
id the store can't resolve renders `unresolved: <id>`; the send still succeeds
(`law-a-bead-reference-carries-its-details`).

**No other writer.** Every sender — incident, watchd, suites, sessions — calls `mail.sh send`.
A `PreToolUse` fence refuses an agent writing into `SPIRA_MAIL` by any other path, alongside
the existing fence on a hand-rolled `bd create`.

## Delivery

A reader registers for a mailbox in `SPIRA_MAIL_READERS` (`<mailbox>=<wake command>`, one
per line; the concierge's wake command is `concierge.sh wake`).
`spira-mail-deliver.service` watches `new/` of every registered mailbox with `inotifywait`.
On an arrival it waits `SPIRA_MAIL_SETTLE` (default 2s) for a burst to finish, then runs that
mailbox's wake command with one line: **"You have N unread messages — mail.sh list <mailbox>
--unread"**. If the reader is not running the wake fails quietly; the mail stays unread.

`SPIRA_WAKE` types the text into the concierge's tmux pane. If the operator is locally
attached and typing at that moment, the injected text can mix into their half-written line.
Typing from the phone is not affected. The daemon inherits this from the shared wake path and
does not attempt to solve it.

Fresh context: the session hook prints the concierge's unread mail (bounded by
`SPIRA_HOOK_LINES`) and marks nothing read. It prints no Monitor instructions.

## Health

One check, on the existing notify timer, over every registered mailbox: `mail.sh unread-age`
above `SPIRA_MAIL_UNREAD_AGE` (default 1800) mails the operator once per distinct backlog —
"<reader> is not reading its mail". Cleared when the mailbox is empty.

## The operator's side

- **Reading.** aerc on the `operator` Maildir. The harness ships an example
  `aerc/accounts.conf` and `aerc/binds.conf`; `outgoing` is `mail.sh sendmail`.
- **Answering.** A reply goes through `mail.sh sendmail`: it closes the tracking bead with the
  reply's first paragraph as the verdict (or executes `uphold` / `retire` / `amend: …` for a
  suit), then delivers the reply to the original sender's mailbox, or to `concierge` when the
  sender has none. A key binding sends "accept default".

## Retirement

Only after delivery, the hook and replies have each run supervised
(`law-arm-before-you-retire`): delete `ask.sh`, remove the answers watcher from the manifest,
the attention pane from the cockpit layout, and the Monitor guidance from the concierge brief.

## Work breakdown

1. `mail.sh`: store, send with the fixed lint, list, read, count, unread-age.
2. Kinds as data: the kinds directory, `template`, and per-kind validation in `send`.
3. The delivery daemon over registered mailboxes, with the count-only wake.
4. The session hook prints the unread count.
5. The unread-age health check over registered mailboxes.
6. Senders move to `mail.sh send`: every `ask.sh` caller migrated, `ask.sh` deleted.
7. Replies: `mail.sh sendmail`, verdict execution, aerc example config.
8. The fence: writes into `SPIRA_MAIL` only through `mail.sh`.
9. Retirement of the answers watcher, the attention pane and Monitor guidance.

## Test strategy

Every suite runs through `testenv-batch.sh` and plants its offender before trusting silence.

- `mail.sh`: delivery is atomic (no partial file is ever listed); read moves new to cur;
  count and unread-age are right with zero, one and many messages; every lint rule refuses a
  planted offender and names itself, and the override is recorded in the header.
- Kinds: an unknown kind, a missing header and an empty section are each refused; editing a
  kind's file changes what is accepted with no code change.
- Delivery: a stub wake receives exactly one call per burst, carries the count, and is not
  called for an unregistered mailbox; a failed wake leaves the mail unread.
- Health: a backlog older than the threshold mails the operator once, not per pass.
- Replies: a fixture reply closes its bead with the first paragraph, executes each suit
  verdict, and lands in the sender's mailbox or the concierge's.
- Migration: no caller of `ask.sh` remains (a grep that is first shown to find a planted one).
- The fence refuses a direct write into a mailbox and allows `mail.sh`.

## Dependencies

`inotify-tools` and `aerc` are declared in the dependency list; `doctor.sh` reports them
missing and never installs.
