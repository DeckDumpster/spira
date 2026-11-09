-- The runtime's journal leaves dolt: events go to VictoriaLogs, and dolt keeps only what must
-- outlive a restart, each row upserted in place, so nothing here grows with history and the
-- runtime's user needs no DELETE.
--   runtime_task_state — one row per (task, key): a task's carry, the scheduler's halt.
--   runtime_effect     — one row per (task, idempotency key): the effect a crash must not lose;
--                        a request upserts it open, its outcome sets resolved = 1.
-- runtime_event, runtime_applied and runtime_cursor are no longer written. The 0038 effect
-- table is renamed aside (DDL, no DELETE); a fresh database gets both from schema.sql.
--
-- `value` and `spec` are LONGTEXT: until the per-family carry split a task's whole carry rides
-- one key, and a carry already outgrew TEXT in production.

RENAME TABLE runtime_effect TO runtime_effect_0038;

CREATE TABLE IF NOT EXISTS runtime_effect (
    task         VARCHAR(128) NOT NULL,
    ekey         VARCHAR(255) NOT NULL,
    request_id   VARCHAR(255) NOT NULL,
    stream       VARCHAR(128) NOT NULL,
    spec         LONGTEXT NOT NULL,
    requested_at BIGINT NOT NULL,
    deadline_at  BIGINT NOT NULL,
    resolved     TINYINT(1) NOT NULL DEFAULT 0,
    PRIMARY KEY (task, ekey)
);
CREATE INDEX runtime_effect_request_idx ON runtime_effect (request_id);
CREATE INDEX runtime_effect_unresolved_idx ON runtime_effect (resolved);

CREATE TABLE IF NOT EXISTS runtime_task_state (
    task  VARCHAR(128) NOT NULL,
    skey  VARCHAR(255) NOT NULL,
    value LONGTEXT NOT NULL,
    at    BIGINT NOT NULL,
    PRIMARY KEY (task, skey)
);
