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

-- The bead machine's row. `holds` is a JSON array of hold kinds (poison/ask/wait/manual);
-- holds suspend a state without losing it, so they are not folded into `state` itself.
CREATE TABLE IF NOT EXISTS bead (
    bead_id     VARCHAR(64) NOT NULL PRIMARY KEY,
    state       VARCHAR(16) NOT NULL,
    tip         VARCHAR(64) NULL,
    gate_key    VARCHAR(128) NULL,
    holder      VARCHAR(128) NULL,
    -- The claiming persona (fayth); migrations/0005-persona.sql for an existing database.
    persona     VARCHAR(64) NULL,
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
    -- Mirrored from bd by bead.sh file/amend so the ops views never join to bd; migrations/0007.
    title       VARCHAR(512) NULL,
    priority    TINYINT NULL,
    -- Ahead of the line (Express/Unexpress events); migrations/0010-express.sql for an existing database.
    express     BOOLEAN NOT NULL DEFAULT FALSE,
    -- The holder's phase and how its session was cut short, set only while WORKING; migrations/0016-aeon-phase.sql for an existing database.
    aeon_phase       VARCHAR(16) NULL,
    disposition      VARCHAR(16) NULL,
    disposition_note TEXT NULL,
    -- The tip a round last ejected as red; submit refuses it. migrations/0017-ejected-red-tip.sql for an existing database.
    ejected_red_tip VARCHAR(64) NULL,
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
    -- The round's pass number and, while CI_RUNNING, its phase (build|suites); migrations/0014-batch-pass.sql for an existing database.
    pass      BIGINT NOT NULL DEFAULT 0,
    phase     VARCHAR(8) NULL,
    version   BIGINT NOT NULL,
    opened_at BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_batch_opened ON batch (opened_at, batch_id);

CREATE TABLE IF NOT EXISTS batch_member (
    batch_id VARCHAR(64) NOT NULL,
    bead_id  VARCHAR(64) NOT NULL,
    tip      VARCHAR(64) NOT NULL,
    outcome  VARCHAR(16) NULL,
    PRIMARY KEY (batch_id, bead_id),
    CONSTRAINT fk_member_batch FOREIGN KEY (batch_id) REFERENCES batch (batch_id),
    CONSTRAINT fk_member_bead FOREIGN KEY (bead_id) REFERENCES bead (bead_id)
);

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
CREATE INDEX IF NOT EXISTS ask_state_idx ON ask (state, opened_at);

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
CREATE INDEX IF NOT EXISTS event_since_idx ON event (machine, applied, lc_key, to_state, at);
CREATE INDEX IF NOT EXISTS event_history_idx ON event (lc_key, machine);
CREATE INDEX IF NOT EXISTS bead_state_since_idx ON bead (state, since);
CREATE INDEX IF NOT EXISTS batch_state_idx ON batch (state);
CREATE INDEX IF NOT EXISTS event_at_idx ON event (machine, applied, at, from_state, to_state, event, refusal);

-- Dependency edges mirrored from bd (migrations/0011-bead-dep.sql for an existing database).
CREATE TABLE IF NOT EXISTS bead_dep (
    bead_id    VARCHAR(64) NOT NULL,
    depends_on VARCHAR(64) NOT NULL,
    dep_type   VARCHAR(32) NOT NULL,
    PRIMARY KEY (bead_id, depends_on)
);
CREATE INDEX IF NOT EXISTS bead_dep_target_idx ON bead_dep (depends_on);

-- The ops read model; migrations/0007-ops-read-model.sql makes the same views on an existing
-- database (ops_live as replaced by 0009), and the two are asserted identical by test-ops-read-model.sh.
-- Each view takes its keys from the covering (state, since) index, then reads the rows by
-- primary key: with the hint the plan is the same whether or not the optimizer has table
-- statistics, which on a mostly-terminal table would otherwise choose a scan.
CREATE OR REPLACE VIEW ops_live AS
SELECT /*+ JOIN_ORDER(r,t,x) LOOKUP_JOIN(r,t) */
       t.bead_id, t.state, t.holds, t.holder, t.persona, (t.state = 'REWORK') AS rework, t.lease_until, t.since, t.updated_at, t.priority, t.title,
       (t.state = 'READY' AND JSON_LENGTH(t.holds) = 0 AND x.blocker IS NULL) AS claimable,
       x.blocker AS blocker
  FROM (SELECT bead_id FROM bead
         WHERE state IN ('OPEN', 'READY', 'WORKING', 'SUBMITTED', 'CERTIFIED', 'IN_DELIVERY', 'REWORK')) r
  JOIN bead t ON t.bead_id = r.bead_id
  LEFT JOIN (SELECT /*+ JOIN_ORDER(b,d) LOOKUP_JOIN(b,d) */ d.bead_id, MIN(d.depends_on) AS blocker
               FROM bead b JOIN bead_dep d ON d.depends_on = b.bead_id
              WHERE b.state IN ('OPEN', 'READY', 'WORKING', 'SUBMITTED', 'CERTIFIED', 'IN_DELIVERY', 'REWORK')
                AND d.dep_type = 'blocks'
              GROUP BY d.bead_id) x ON x.bead_id = t.bead_id;

CREATE OR REPLACE VIEW ops_round AS
SELECT /*+ JOIN_ORDER(b,m,t) LOOKUP_JOIN(b,m) LOOKUP_JOIN(m,t) */
       b.batch_id, b.repo, b.state AS batch_state, b.head, b.base, b.opened_at,
       m.bead_id, m.tip, m.outcome, t.title, t.priority
  FROM batch b
  JOIN batch_member m ON m.batch_id = b.batch_id
  LEFT JOIN bead t ON t.bead_id = m.bead_id
 WHERE b.state IN ('OPEN', 'CI_RUNNING', 'GREEN', 'ATTRIBUTING', 'REBUILDING');

CREATE OR REPLACE VIEW ops_recent AS
SELECT /*+ JOIN_ORDER(r,t) LOOKUP_JOIN(r,t) */
       t.bead_id, t.state, t.since, t.updated_at, t.priority, t.title
  FROM (SELECT bead_id FROM bead WHERE state = 'LANDED' AND since >= UNIX_TIMESTAMP() - 86400) r
  JOIN bead t ON t.bead_id = r.bead_id;

-- The where-stuck read model; migrations/0009-where-stuck.sql and 0013-ops-edges-event.sql make the
-- same views on an existing database, and the two are asserted identical by test-ops-read-model.sh.
CREATE OR REPLACE VIEW ops_edges AS
SELECT 'applied' AS kind, from_state, to_state, event, NULL AS refusal,
       SUM(at >= UNIX_TIMESTAMP() - 3600) AS n_1h, COUNT(*) AS n_24h, MAX(at) AS last_at
  FROM event
 WHERE machine = 'bead' AND applied = 1 AND at >= UNIX_TIMESTAMP() - 86400 AND from_state <> to_state
 GROUP BY from_state, to_state, event
UNION ALL
SELECT 'refused', from_state, NULL, event, refusal,
       SUM(at >= UNIX_TIMESTAMP() - 3600), COUNT(*), MAX(at)
  FROM event
 WHERE machine = 'bead' AND applied = 0 AND at >= UNIX_TIMESTAMP() - 86400
 GROUP BY from_state, event, refusal;

CREATE OR REPLACE VIEW ops_dwell_p95 AS
SELECT state, MIN(dwell) AS p95_s, MAX(n) AS samples
  FROM (SELECT state, dwell,
               ROW_NUMBER() OVER (PARTITION BY state ORDER BY dwell) AS rn,
               COUNT(*) OVER (PARTITION BY state) AS n
          FROM (SELECT from_state AS state,
                       at - LAG(at) OVER (PARTITION BY lc_key ORDER BY seq) AS dwell
                  FROM event
                 WHERE machine = 'bead' AND applied = 1 AND at >= UNIX_TIMESTAMP() - 1209600
                   AND from_state <> to_state) d
         WHERE dwell IS NOT NULL) r
 WHERE rn >= CEIL(0.95 * n)
 GROUP BY state;

CREATE OR REPLACE VIEW ops_dwell AS
SELECT x.bead_id, x.state, x.holds, x.priority, x.title, x.entered_at, p.p95_s, p.samples
  FROM (SELECT /*+ JOIN_ORDER(r,t,e) LOOKUP_JOIN(r,t) LOOKUP_JOIN(t,e) */
               t.bead_id, t.state, t.holds, t.priority, t.title,
               COALESCE(MAX(e.at), t.updated_at) AS entered_at
          FROM (SELECT bead_id FROM bead
                 WHERE state IN ('OPEN', 'READY', 'WORKING', 'SUBMITTED', 'CERTIFIED', 'IN_DELIVERY', 'REWORK')) r
          JOIN bead t ON t.bead_id = r.bead_id
          LEFT JOIN event e ON e.machine = 'bead' AND e.applied = 1 AND e.lc_key = t.bead_id
                           AND e.to_state = t.state AND e.from_state <> e.to_state
         GROUP BY t.bead_id, t.state, t.holds, t.priority, t.title, t.updated_at) x
  LEFT JOIN ops_dwell_p95 p ON p.state = x.state;
