-- The planning watermark while the control plane is this store: one monotonic sequence that
-- `spira-lc plan stamp` advances after every planning write, so a planning reader polls one
-- cheap row instead of re-reading bd. A fresh database gets the table from schema.sql.

CREATE TABLE IF NOT EXISTS planning_watermark (
    id  TINYINT NOT NULL PRIMARY KEY,
    seq BIGINT NOT NULL
);
