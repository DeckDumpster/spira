-- Serves the batch list read: ORDER BY opened_at DESC, batch_id DESC with no filesort.

CREATE INDEX idx_batch_opened ON batch (opened_at, batch_id);
