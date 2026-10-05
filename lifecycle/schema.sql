-- spira_lifecycle — the one database backing all three machines (bead, delivery, batch),
-- so a cascade across them is one transaction (design §3.3). Applied with:
--
--   dolt --host H --port P -u root -p PASS sql < schema.sql
--
-- Grants that make "only spira_lc writes" enforced, not intended, are in grants.sql — kept
-- separate because applying them requires a superuser connection this file does not need,
-- and because rotating the spira_lc credential must never mean re-running DDL.

CREATE DATABASE IF NOT EXISTS spira_lifecycle;
USE spira_lifecycle;

-- The bead machine's row. `holds` is a JSON array of hold kinds (poison/ask/wait/operator);
-- holds suspend a state without losing it, so they are not folded into `state` itself.
CREATE TABLE IF NOT EXISTS bead (
    bead_id     VARCHAR(64) NOT NULL PRIMARY KEY,
    state       VARCHAR(16) NOT NULL,
    tip         VARCHAR(64) NULL,
    gate_key    VARCHAR(128) NULL,
    holder      VARCHAR(128) NULL,
    lease_until BIGINT NULL,
    holds       JSON NOT NULL,
    reason      TEXT NULL,
    version     BIGINT NOT NULL,
    -- Stacked dependents (design stacked-dependents-2026-09-28 §1): the certified tip of
    -- each prerequisite this bead's current work was built on, and the depth that stack
    -- reaches. Empty/zero for an unstacked bead. An existing database gets these columns
    -- from migrations/0001-stack.sql instead of re-running this CREATE TABLE.
    stack       JSON NOT NULL DEFAULT (JSON_OBJECT()),
    stack_depth BIGINT NOT NULL DEFAULT 0,
    -- Epoch seconds the row last entered LANDED or CERTIFIED; migrations/0002-since.sql for an existing database.
    since       BIGINT NULL,
    updated_at  BIGINT NOT NULL
);

-- One row per bead's current (or most recent) delivery attempt. A new `deliver` event
-- after an exit starts a fresh row's worth of state, not a new table row — `bead_id` is
-- the primary key, so the exited delivery's history lives only in `event`.
CREATE TABLE IF NOT EXISTS delivery (
    bead_id   VARCHAR(64) NOT NULL PRIMARY KEY,
    mode      VARCHAR(8) NOT NULL,
    state     VARCHAR(16) NOT NULL,
    batch_id  VARCHAR(64) NULL,
    pr        BIGINT NULL,
    merge_sha VARCHAR(64) NULL,
    version   BIGINT NOT NULL,
    CONSTRAINT fk_delivery_bead FOREIGN KEY (bead_id) REFERENCES bead (bead_id)
);

CREATE TABLE IF NOT EXISTS batch (
    batch_id  VARCHAR(64) NOT NULL PRIMARY KEY,
    repo      VARCHAR(128) NOT NULL,
    state     VARCHAR(16) NOT NULL,
    parent    VARCHAR(64) NULL,
    head      VARCHAR(64) NULL,
    base      VARCHAR(64) NULL,
    pr        BIGINT NULL,
    run       VARCHAR(64) NULL,
    reason    TEXT NULL,
    version   BIGINT NOT NULL,
    opened_at BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS batch_member (
    batch_id VARCHAR(64) NOT NULL,
    bead_id  VARCHAR(64) NOT NULL,
    tip      VARCHAR(64) NOT NULL,
    outcome  VARCHAR(16) NULL,
    PRIMARY KEY (batch_id, bead_id),
    CONSTRAINT fk_member_batch FOREIGN KEY (batch_id) REFERENCES batch (batch_id),
    CONSTRAINT fk_member_bead FOREIGN KEY (bead_id) REFERENCES bead (bead_id)
);

-- The truth. Append-only: grants.sql gives spira_lc INSERT and SELECT only, so not even
-- the machine's own user can UPDATE or DELETE a row here (design §3.3).
CREATE TABLE IF NOT EXISTS event (
    seq        BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
    machine    VARCHAR(8) NOT NULL,
    lc_key     VARCHAR(64) NOT NULL,
    event      VARCHAR(32) NOT NULL,
    expect     VARCHAR(16) NOT NULL,
    from_state VARCHAR(16) NOT NULL,
    to_state   VARCHAR(16) NOT NULL,
    applied    TINYINT(1) NOT NULL,
    refusal    VARCHAR(32) NULL,
    evidence   JSON NOT NULL,
    actor      VARCHAR(128) NOT NULL,
    at         BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS event_lc_key_idx ON event (machine, lc_key);
