CREATE TABLE IF NOT EXISTS access_log (
    access_id TEXT PRIMARY KEY NOT NULL,
    timestamp_utc_us BIGINT NOT NULL,
    entry_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS access_log_timestamp ON access_log(timestamp_utc_us);
