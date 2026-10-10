# Integrity Chain (HIPAA-5)

Each event in the store can carry an HMAC-SHA256 signature over its content
plus the previous event's signature. Tampering with any historical row
invalidates every downstream signature — the chain is tamper-evident.

§164.312(c)(1) Integrity Controls.

## Algorithm

```
sig(0)   = ""                                                  -- genesis
sig(n)   = hex( HMAC-SHA256(key, sig(n-1) || canonical(event_n)) )
```

`canonical(event)` is JSON of the tuple
`(event_id, aggregate_type, aggregate_id, sequence, event_type, payload, timestamp)`
— the immutable parts of the event. `audit` fields are deliberately
**excluded** from the signature so audit metadata can be projected to
analytic stores in a different shape without invalidating the chain.

## API

```rust
trait IntegrityChain {
    fn sign_event(&self, prev: &EventSignature, event: &Event)
        -> Result<EventSignature, IntegrityError>;

    fn verify_chain(&self, events: &[(Event, EventSignature)])
        -> IntegrityResult;
}

let chain = HmacSha256Chain::new(thirty_two_byte_key)?;
let sig1 = chain.sign_event(&EventSignature::genesis(), &event1)?;
let sig2 = chain.sign_event(&sig1, &event2)?;

assert_eq!(
    chain.verify_chain(&[(event1, sig1), (event2, sig2)]),
    IntegrityResult::Valid
);
```

`verify_chain` reports the first failure as `IntegrityResult::Broken`:

- `BrokenAt { sequence, aggregate_id }` — signature mismatch (tamper)
- `OutOfOrder { expected, sequence, aggregate_id }` — gap or reordering

## Why HMAC, not a public-key signature

Per-event ECDSA/Ed25519 is overkill for a single-tenant audit chain. The
threat model is *internal* tampering by someone with DB write access, not
*external* impersonation of the framework. A symmetric HMAC keyed by a
secret only the application owns is enough evidence of tampering — and
~50× faster on the write path.

Step 5 may add a public-verifiable mode for cross-organization audit
hand-off; the trait surface accommodates it without breaking changes.

## Runtime enforcement

Event integrity is opt-in. When `EVENT_INTEGRITY_KEY` is configured,
`SqliteEventStore` and `PostgresEventStore` sign newly appended events and
store both `integrity_signature` and `integrity_key_id` with the row. `load`,
`load_from`, and `stream_all` recompute the chain and return
`EventStoreError::Integrity` when a signature is missing or does not match.

Without `EVENT_INTEGRITY_KEY`, the stores keep the compatibility behavior:
events append and load without signature enforcement.

Existing databases need a backfill before enabling enforcement. The signature
columns are nullable so the migration can land without rewriting historical
events, but an enabled store intentionally rejects unsigned historical rows.

Postgres uses the same row fields and verification behavior as SQLite.

## Key management

Out of scope here. Real deployments:

- Load `EVENT_INTEGRITY_KEY` from a KMS / Vault / sealed-secret at startup.
- Keep `EVENT_INTEGRITY_KEY_ID` stable for a key and change it only as part of
  a documented rotation/backfill procedure.
- Rotate by versioning: store `key_id` alongside the signature; verification
  can pick the right key per event once multi-key lookup is added.
- Never log the key. The `Debug` impl on `EventSignature` already truncates
  output to avoid leaking full hashes into telemetry.

## Pinned test vector

```
key:    "thirty-two-byte-known-test-key!!"
prev:   "" (genesis)
event:  Event {
    event_id: 00000000-0000-0000-0000-000000000000,
    aggregate_type: "Vector",
    aggregate_id: "vec-1",
    sequence: 1,
    event_type: "VectorEvent",
    payload: { "n": 1 },
    timestamp: 1700000000000,
}
sig:    7f519ff1222f551b490282cd220dda12f707a3979300b05d6f89f7a564749a9f
```

`canonical_bytes` changes will break this test vector — that is intentional.
Updating the canonical layout is a deliberate breaking change to the chain
format.
