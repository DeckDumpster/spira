-- The ask machine's row (migrations/0017-ask.sql for an existing database): an escalation to
-- the operator is its own lifecycle, never a bead-machine row, so no claim or list over
-- `bead` can ever return one. `work_bead` is the bead whose `ask` hold this ask lifts.
CREATE TABLE IF NOT EXISTS ask (
    ask_id    VARCHAR(64) NOT NULL PRIMARY KEY,
    state     VARCHAR(16) NOT NULL,
    work_bead VARCHAR(64) NULL,
    closed_by VARCHAR(128) NULL,
    quote     TEXT NULL,
    channel   VARCHAR(32) NULL,
    version   BIGINT NOT NULL,
    opened_at BIGINT NOT NULL,
    closed_at BIGINT NULL
);
CREATE INDEX ask_state_idx ON ask (state, opened_at);
