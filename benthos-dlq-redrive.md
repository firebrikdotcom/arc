# Benthos DLQ and Redrive

This guide is for operators running Arc in the distributed event lane:

`Arc writer -> NATS JetStream events.> -> Benthos -> HTTP/NATS handlers`

Benthos owns handler retries and dead-letter routing. It must never write directly to Arc
databases. Projection repair and projection writes still go through Arc-owned code paths.

## DLQ Subjects

Generated handler pipelines route exhausted failures to:

```text
dlq.<handler-name>.<event_type>
```

Envelope validation failures route to:

```text
dlq.envelope.<event_type>
dlq.envelope.invalid
```

The message body is the handler envelope plus `x_arc_dlq` metadata:

```json
{
  "event_id": "550e8400-e29b-41d4-a716-446655440000",
  "aggregate_type": "User",
  "aggregate_id": "9b2f...",
  "sequence": 3,
  "event_type": "UserRegistered",
  "payload": {},
  "x_arc_dlq": {
    "handler": "welcome-email",
    "reason": "delivery_failed_after_retries",
    "failed_at": "2026-06-18T19:46:55.827647355Z",
    "original_subject": "events.user.user_registered",
    "fingerprint": "..."
  }
}
```

`event_id` is the idempotency key. `x_arc_dlq.fingerprint` is for operator correlation only; do not
use it as a domain identifier.

## DLQ Stream Persistence

Benthos publishes DLQ messages with `nats_jetstream`, so NATS must have a stream whose subjects
match `dlq.>`. The main Arc event stream is `EVENTS` and consumes `events.>`; keep the DLQ stream
separate so redrive and retention can be managed independently.

When Arc starts with `EVENT_BUS=nats`, `arc-es-nats` idempotently ensures both streams exist:

```text
EVENTS   -> events.>
ARC_DLQ  -> dlq.>
```

That means local Docker Compose and normal app startup provision DLQ persistence automatically. The
NATS CLI equivalent is useful for operator verification or manual repair:

```bash
nats --server "$NATS_URL" stream add ARC_DLQ \
  --subjects "dlq.>" \
  --storage file \
  --retention limits \
  --discard old \
  --max-age 336h \
  --defaults
```

If the DLQ stream is missing, Benthos cannot persist a failed message to the DLQ and the original
message can continue retrying instead of being isolated.

## Inspect Failures

Start with counts and subjects:

```bash
nats --server "$NATS_URL" stream info ARC_DLQ
nats --server "$NATS_URL" stream subjects ARC_DLQ
```

Create or reuse an operator pull consumer:

```bash
nats --server "$NATS_URL" consumer add ARC_DLQ ops-inspect \
  --pull \
  --filter "dlq.>" \
  --ack explicit \
  --deliver all \
  --replay instant \
  --defaults
```

Inspect a small batch without changing Arc state:

```bash
nats --server "$NATS_URL" consumer next ARC_DLQ ops-inspect --count 5
```

When reviewing a DLQ message, capture:

- `x_arc_dlq.handler`
- `x_arc_dlq.reason`
- `x_arc_dlq.original_subject`
- `event_id`
- `aggregate_type`
- `aggregate_id`
- `sequence`
- handler response logs around `x_arc_dlq.failed_at`

Do not paste raw `payload` into tickets or chat if it can contain PHI or secrets.

## Fix Before Redrive

Redrive is only safe after the cause is fixed. Common causes:

- Handler is down or unreachable.
- Handler auth token is missing or wrong.
- Handler returns a permanent validation error.
- Handler is not idempotent and fails on duplicate `event_id`.
- Projection endpoint rejects the event because the Arc-owned projection route is not configured.
- Envelope validation fails because a publisher emitted a malformed event.

For projection handlers, the fix belongs in Arc-owned projection code or configuration. Do not add
SQL/database outputs to Benthos as a shortcut.

## Redrive A Handler DLQ Message

For normal handler failures, publish the original envelope back to its original event subject after
the handler is fixed. Remove `x_arc_dlq` before publishing so the normal routing checks accept it.

Use this shape for a one-message redrive:

```bash
nats --server "$NATS_URL" consumer next ARC_DLQ ops-inspect --raw > /tmp/arc-dlq-message.json
jq 'del(.x_arc_dlq)' /tmp/arc-dlq-message.json > /tmp/arc-redrive-message.json
nats --server "$NATS_URL" pub "events.user.user_registered" "$(cat /tmp/arc-redrive-message.json)"
```

Use the subject from `x_arc_dlq.original_subject`. If it is empty, reconstruct the subject from the
Arc convention:

```text
events.<aggregate_type snake_case>.<event_type snake_case>
```

The handler must be idempotent on `event_id`, so redriving the same message more than once should
not duplicate side effects.

## Redrive Envelope Validation Failures

Do not republish validation failures blindly. These messages failed the Arc envelope contract.

Use this process instead:

1. Inspect the missing or invalid fields.
2. Fix the publisher, migration, or manual test input that produced the malformed event.
3. If the event represents a real business fact, reconstruct a valid Arc event through an Arc-owned
   repair path or command.
4. Publish only a valid event envelope back to `events.>`.

If a validation failure cannot be reconstructed safely, keep it in the DLQ for audit and mark it
resolved in the incident record.

## Acknowledge Or Keep

After a successful redrive, acknowledge the DLQ message in the operator consumer so it is not
presented again:

```bash
nats --server "$NATS_URL" consumer next ARC_DLQ ops-inspect --ack
```

If the redrive is not complete, do not acknowledge the message. Leave it pending or create a
separate incident note with the `event_id` and `x_arc_dlq.fingerprint`.

## Operator Checklist

- DLQ stream `ARC_DLQ` exists and matches `dlq.>`.
- Benthos metrics and logs are monitored for delivery failures.
- Handler owners know their DLQ subject names.
- Redrive removes `x_arc_dlq` and republishes to `x_arc_dlq.original_subject`.
- Redrive is tested against an idempotent handler before production use.
- Benthos has no SQL/database outputs and no Arc database credentials.
