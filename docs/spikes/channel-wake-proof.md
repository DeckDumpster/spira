# Does a channel event wake an idle session?

A live proof on the installed Claude Code: an idle `--remote-control` session is sent a channel
event after 31 idle minutes, and another after `/compact`. Channel support was only read from
the 2.1.332 binary and docs. A green `channel-spike` test proves the server's half, not this.
**Nothing that depends on channels is built until the Verdict below reads PASS.**

The parts, in [`channel-wake-proof/`](channel-wake-proof/): marketplace `spira`, plugin
`spira-sub` (its `.mcp.json` runs the `channel-spike` binary this repository builds), and
`managed-settings.json`. The names are the ones the shipped plugin uses, so the root file is
written once.

Run it in a scratch session, **never the Concierge's own**. Run every block from one shell at
the root of the checkout unless it says otherwise. The installed plugin is read from this
checkout, so leave it in place until clean-up.

## Runbook

### 1. Client version

```sh
claude --version   # must print: 2.1.332 (Claude Code)
```

**Stop** on any other version until these are re-read in that version's binary and docs: the
flags `--channels` and `--remote-control`; the settings `channelsEnabled` and
`allowedChannelPlugins`; the capability `experimental["claude/channel"]`; the method
`notifications/claude/channel` (`content` a string; `meta` string values under keys of letters,
digits and `_`, other keys dropped); and the MCP revision cap, `2025-11-25` (a server on
2026-07-28 or later is silently not registered).

Do not use `--dangerously-load-development-channels`: it shows a dialog at every launch.

### 2. Managed settings (the operator, once, as root)

The content of `/etc/claude-code/managed-settings.json`:

```json
{"channelsEnabled": true, "allowedChannelPlugins": [{"marketplace": "spira", "plugin": "spira-sub"}]}
```

`allowedChannelPlugins` **replaces** Anthropic's allowlist for every session on the host. If
the file already exists, stop and merge these two keys by hand. Otherwise the operator runs:

```sh
sudo install -D -m 0644 docs/spikes/channel-wake-proof/managed-settings.json /etc/claude-code/managed-settings.json
```

### 3. Scratch directory and build

The directory is short (a Unix socket path has a small limit) and outside any git checkout.

```sh
export REPO="$PWD"
export SPIKE="$(realpath "$(mktemp -d /tmp/cw.XXXXXX)")"
mkdir -p "$SPIKE/control" "$SPIKE/project"
CARGO_TARGET_DIR="$SPIKE/target" cargo build -p channel-spike
printf '[spira]\nsub_control_dir = "%s"\n' "$SPIKE/control" > "$SPIKE/override.toml"
export SPIKE_TOML="$SPIRA_TOML:$SPIKE/override.toml"
```

### 4. The scratch subscriber, in its own shell

The subscriber is `wake-spike`, never the Concierge's name. Its control directory must be the
scratch one. A `SPIRA_SUB_CONTROL_DIR` set in the environment does **not** override a
directory the config declares, so it is set by a second config layer. In a second shell, with
`SPIKE_TOML` copied over, leave this running:

```sh
SPIRA_TOML="$SPIKE_TOML" spira-sub run --as wake-spike
```

Back in the first shell:

```sh
test -S "$SPIKE/control/wake-spike.sock" && echo scratch socket present
```

### 5. Install the plugin for the scratch project only

`--scope local` enables it in the scratch project and nowhere else.

```sh
cd "$SPIKE/project"
claude plugin marketplace add "$REPO/docs/spikes/channel-wake-proof" --scope local
claude plugin install spira-sub@spira --scope local
claude        # once, with no channels: accept the folder trust question, then /exit
cd "$REPO"
```

The trust question belongs to any new folder; answering it here keeps it out of the launch
being judged.

### 6. Launch, in a fresh tmux pane

`--channels` takes a list, so another flag must follow it. `--debug-file` is that flag, and
its log says whether the channel was registered or why it was skipped.

```sh
tmux new-session -d -s channel-wake -x 200 -y 50 -c "$SPIKE/project" \
  env -u CLAUDECODE CHANNEL_SPIKE_BIN="$SPIKE/target/debug/channel-spike" CHANNEL_SPIKE_CONTROL_DIR="$SPIKE/control" \
  claude --remote-control --channels plugin:spira-sub@spira --debug-file "$SPIKE/claude-debug.log"
tmux set-option -t channel-wake history-limit 100000
tmux set-option -t channel-wake remain-on-exit on
```

Look at the pane **without attaching**, so that no key can reach it:

```sh
tmux capture-pane -p -t channel-wake -S - | tee "$SPIKE/pane-launch.txt"
SPIRA_TOML="$SPIKE_TOML" spira-sub list --as wake-spike   # must say: readers 1
grep -i channel "$SPIKE/claude-debug.log"
```

- Record whether the pane shows **any dialog**. If it does, the result is FAIL.
- `readers 1` means the channel server is attached; do not wait without it, and do not start
  a `spira-sub follow` on this name, which would make the count prove nothing. It does not
  mean the client registered the channel: an unregistered channel drops events silently. Keep
  the debug log's registration line for the results.

### 7. Event 1: 31 idle minutes

```sh
AT1="$(date -u -d '+31 minutes' +%Y-%m-%dT%H:%M:%SZ)"; echo "event 1 at $AT1"
SPIRA_TOML="$SPIKE_TOML" spira-sub add --at "$AT1" --note "wake test 1" --as wake-spike
```

Send nothing to the session, from the pane or the phone, until after `$AT1`. A few minutes
after it:

```sh
tmux capture-pane -p -t channel-wake -S - | tee "$SPIKE/pane-event1.txt"
```

Then the operator opens the session on the phone and screenshots how the event is drawn. Save
it as `docs/spikes/channel-wake-proof/phone.png`.

**If the phone draws the event so that it looks like the operator's own typing, stop the run
here** and take the screenshot to the operator.

### 8. Event 2: after `/compact`

`/compact` is typed (it is not a delivery):

```sh
tmux send-keys -t channel-wake /compact Enter
tmux capture-pane -p -t channel-wake -S -        # repeat until compaction has finished
```

Once it has finished, schedule event 2:

```sh
AT2="$(date -u -d '+5 minutes' +%Y-%m-%dT%H:%M:%SZ)"; echo "event 2 at $AT2"
SPIRA_TOML="$SPIKE_TOML" spira-sub add --at "$AT2" --note "wake test 2" --as wake-spike
```

Send nothing more. A few minutes after `$AT2`:

```sh
tmux capture-pane -p -t channel-wake -S - | tee "$SPIKE/pane-event2.txt"
```

The only input in the pane history since launch must be `/compact`: nothing was typed to
deliver either event.

### 9. Timings and the transcript row

```sh
PROJECT_LOGS="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/projects/$(printf %s "$SPIKE/project" | sed 's/[^A-Za-z0-9]/-/g')"
grep -n 'wake test' "$PROJECT_LOGS"/*.jsonl | cut -c1-400
```

For each event, the turn started at the `timestamp` of the first `assistant` row after the
event's row. Latency is that minus the scheduled instant. A row that only queues the event,
with no `assistant` row following it, is not a turn.

The row to keep is event 1's `user` row whose `origin.kind` is `channel`. This writes it with
host paths and the session id replaced:

```sh
LOG="$(grep -l 'wake test 1' "$PROJECT_LOGS"/*.jsonl | head -1)"
jq -c --arg spike "$SPIKE" --arg home "$HOME" --arg sid "$(basename "$LOG" .jsonl)" '
  select(.type == "user" and .origin.kind? == "channel" and (tostring | contains("wake test 1")))
  | walk(if type == "string" then
      split($spike) | join("<scratch>") | split($home) | join("<home>") | split($sid) | join("<session>")
    else . end)' "$LOG" > docs/spikes/channel-wake-proof/channel-row.jsonl
test "$(wc -l < docs/spikes/channel-wake-proof/channel-row.jsonl)" = 1 && cat docs/spikes/channel-wake-proof/channel-row.jsonl
```

Read the line before committing it: no host path, user name, host name or session id may be
left. If no row matches that filter, the transcript's shape differs from what was expected:
keep whichever row carries the event, scrubbed the same way, and say so under Results. That
file is the fixture for the channel verb's transcript check.

### 10. Clean up

Stop the `spira-sub run` shell, then:

```sh
tmux kill-session -t channel-wake
cd "$SPIKE/project" && claude plugin uninstall spira-sub@spira --scope local && claude plugin marketplace remove spira; cd "$REPO"
```

Remove `$SPIKE` once the results are committed. The managed-settings file stays. The shipped
plugin takes the same names.

## The two branches

**PASS** needs all of these:

- no dialog at launch;
- event 1, 31 idle minutes out, starts a turn;
- event 2, after `/compact`, starts a turn;
- no keystroke was sent to the pane to deliver either;
- the phone screenshot and the scrubbed transcript row are saved;
- the phone's drawing can be told apart from the operator's typing.

**FAIL:** either event starts no turn, or a dialog appears. The channel verb is then
re-drafted as the background waiter (design candidate (b): an `asyncRewake` hook running
`spira-sub follow --until-one`). That first needs a replay cursor, and proof that the hook's
timeout passes 31 idle minutes. It goes back to the operator with the evidence: the pane
captures, the debug log's channel lines and the transcript.

**The phone draws the event like the operator's typing:** stop the run and take the
screenshot to the operator. This is not a PASS until they decide.

## Results

Filled in by whoever runs the proof, from the live run only.

| | |
|---|---|
| Date of the run | |
| Client version | |
| Dialog at launch (yes/no) | |
| Debug log's channel registration line | |

| Event | Scheduled instant | Turn started (transcript) | Latency |
|---|---|---|---|
| 1, after 31 idle minutes | | | |
| 2, after `/compact` | | | |

| | |
|---|---|
| Keystrokes sent to deliver either (pane history) | |
| Phone screenshot (path under `docs/spikes/channel-wake-proof/`) | |
| What the phone drew; can it be told from typing? | |
| Transcript row (`docs/spikes/channel-wake-proof/channel-row.jsonl`) | |

**Verdict:**
