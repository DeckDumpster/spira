-- Adds the mending table for an existing spira_lifecycle database: one row per failed suite of a
-- pass. A fresh database gets it from schema.sql.

CREATE TABLE IF NOT EXISTS mending (
    batch_id        VARCHAR(40) NOT NULL,
    pass            BIGINT NOT NULL,
    suite           VARCHAR(80) NOT NULL,
    state           VARCHAR(16) NOT NULL,
    failed_at       BIGINT NOT NULL,
    picked_at       BIGINT NULL,
    deadline        BIGINT NULL,
    triage_ended_at BIGINT NULL,
    diagnosis       TEXT NULL,
    version         BIGINT NOT NULL,
    PRIMARY KEY (batch_id, pass, suite)
);
