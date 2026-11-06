CREATE TABLE event_publications (
    event_id TEXT NOT NULL PRIMARY KEY REFERENCES events(event_id)
);
