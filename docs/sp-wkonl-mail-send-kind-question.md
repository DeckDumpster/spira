# sp-wkonl: mail send --kind question — correct invocation

`mail send <mailbox> --kind question` has no body flag. `--body`,
`--message`, `--question`, a bare positional, and a `-`-prefixed heredoc are
all rejected with `mail send: unknown option: <flag>`.

The body must be piped on stdin as markdown headers, not `Label:` lines:

```
printf '## Question\n<question text>\n\n## Default\n<default text>\n' \
  | mail send <mailbox> --from "Ops <ops@spira>" \
      --subject "<human topic — do NOT lead with a bead id>" \
      --kind question --default "<default text>" --bead <id>
```

Two more lint rules found the same way:

- `--default` on the CLI is still required even though the piped body also
  carries a `## Default` section — checked separately from the body.
- Subject must not lead with a bead id (`mail: lint: Subject leads with a
  bead id`) — put the id in `--bead` instead. `--bead` wires a `relates_to`
  edge; a `bug`-typed bead refuses a *blocking* edge unless
  `SPIRA_MAIL_ALLOW_BLOCKING=1`, but `relates_to` goes through with exit 0.

Verified twice end-to-end on 2026-09-27: both messages landed in
`run/mail/operator/cur` with both sections populated, no lint error.

This is a distinct failure mode from `sop-mail-send-loom-splice-hang`: that
one hangs in a coprocess splice with a filled body already piped; this one
lints instantly on a body of the wrong shape. Written up as
`sop-mail-send-kind-question-body-format` on the SOP shelf.

Used this invocation to deliver the blocked sp-jppn1 escalation (rebuild
stale batch container now vs wait) to the operator during this session.
