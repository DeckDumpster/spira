-- The runtime's journal (the `runtime` crate's StoreJournal), additive: five new tables,
-- nothing existing is touched. A fresh database gets them from schema.sql.

CREATE TABLE IF NOT EXISTS runtime_event (
    seq         BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
    event_id    VARCHAR(255) NOT NULL UNIQUE,
    stream      VARCHAR(128) NOT NULL,
    kind_tag    VARCHAR(64) NOT NULL,
    kind_body   TEXT NOT NULL,
    at          BIGINT NOT NULL,
    cause       VARCHAR(255) NULL,
    correlation VARCHAR(255) NOT NULL
);

CREATE TABLE IF NOT EXISTS runtime_applied (
    handler  VARCHAR(128) NOT NULL,
    event_id VARCHAR(255) NOT NULL,
    PRIMARY KEY (handler, event_id)
);

CREATE TABLE IF NOT EXISTS runtime_cursor (
    id   TINYINT NOT NULL PRIMARY KEY,
    next BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS runtime_schedule (
    row_id      VARCHAR(255) NOT NULL PRIMARY KEY,
    at          BIGINT NOT NULL,
    stream      VARCHAR(128) NOT NULL,
    kind_tag    VARCHAR(64) NOT NULL,
    kind_body   TEXT NOT NULL,
    cause       VARCHAR(255) NULL,
    correlation VARCHAR(255) NOT NULL,
    fired       TINYINT(1) NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS runtime_poison (
    stream   VARCHAR(128) NOT NULL PRIMARY KEY,
    event_id VARCHAR(255) NOT NULL,
    handler  VARCHAR(128) NOT NULL,
    message  TEXT NOT NULL
);
