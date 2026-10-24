-- Preserve event-number JSON text for exact Event equality. Read-model and
-- snapshot columns intentionally remain JSONB.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM pg_attribute a
        WHERE a.attrelid = 'events'::regclass
          AND a.attname = 'payload'
          AND NOT a.attisdropped
          AND a.atttypid = 'pg_catalog.jsonb'::regtype
    ) THEN
        ALTER TABLE events ALTER COLUMN payload TYPE JSON USING payload::json;
    END IF;
END $$;
