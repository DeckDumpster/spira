-- Adds the mend ledger for an existing spira_lifecycle database: one row per member tip a
-- mender produced for a failure. A fresh database gets it from schema.sql.

CREATE TABLE IF NOT EXISTS mend (
    seq        BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
    batch_id   VARCHAR(40) NOT NULL,
    pass       BIGINT NOT NULL,
    suite      VARCHAR(80) NOT NULL,
    bead_id    VARCHAR(40) NOT NULL,
    from_tip   VARCHAR(64) NOT NULL,
    tip        VARCHAR(64) NOT NULL,
    ask_id     VARCHAR(64) NULL,
    checked    TEXT NULL,
    diagnosis  TEXT NULL,
    at         BIGINT NOT NULL,
    UNIQUE KEY mend_failure_bead (batch_id, pass, suite, bead_id),
    KEY mend_bead_idx (bead_id)
);
