CREATE TABLE events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    aggregate_type TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    sequence BIGINT NOT NULL,
    event_type TEXT NOT NULL,
    payload TEXT NOT NULL,
    metadata TEXT DEFAULT '{}',
    timestamp BIGINT NOT NULL,
    actor_id TEXT NOT NULL,
    actor_session_id TEXT,
    source_ip TEXT,
    user_agent TEXT,
    timestamp_utc_us BIGINT NOT NULL,
    causation_id TEXT,
    correlation_id TEXT NOT NULL,
    integrity_signature TEXT,
    integrity_key_id TEXT,
    UNIQUE(aggregate_type, aggregate_id, sequence)
);

CREATE INDEX idx_events_aggregate ON events(aggregate_type, aggregate_id, sequence);
CREATE INDEX idx_events_type ON events(event_type);
CREATE INDEX idx_events_timestamp ON events(timestamp);
CREATE INDEX idx_events_integrity_key_id ON events(integrity_key_id);

CREATE TABLE snapshots (
    aggregate_type TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    version BIGINT NOT NULL,
    state TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    PRIMARY KEY (aggregate_type, aggregate_id)
);

CREATE TABLE jwt_sessions (
    jti TEXT NOT NULL PRIMARY KEY,
    actor_id TEXT NOT NULL,
    created_at_us BIGINT NOT NULL,
    expires_at_us BIGINT NOT NULL,
    revoked_at_us BIGINT
);

CREATE INDEX idx_jwt_sessions_actor_id ON jwt_sessions(actor_id);
CREATE INDEX idx_jwt_sessions_expires_at ON jwt_sessions(expires_at_us);
