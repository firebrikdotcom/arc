#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import YAML from "yaml";

const args = new Set(process.argv.slice(2));
const checkOnly = args.has("--check");

function readManifestFiles(dir) {
  if (!fs.existsSync(dir)) {
    return [];
  }

  return fs
    .readdirSync(dir)
    .filter((file) => file.endsWith(".yaml") || file.endsWith(".yml"))
    .sort()
    .map((file) => path.join(dir, file));
}

function parseManifest(file) {
  const parsed = YAML.parse(fs.readFileSync(file, "utf8"));
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error(`${file}: manifest must be a YAML mapping`);
  }
  return { ...parsed, __file: file };
}

function assertAllowedKeys(value, allowed, label) {
  for (const key of Object.keys(value)) {
    if (!allowed.includes(key)) {
      throw new Error(`${label}.${key} is not supported; allowed keys: ${allowed.join(", ")}`);
    }
  }
}

function requireString(value, label) {
  if (typeof value !== "string" || value.trim() === "") {
    throw new Error(`${label} must be a non-empty string`);
  }
  return value.trim();
}

function optionalStringArray(value, label) {
  if (value === undefined || value === null) {
    return [];
  }
  if (!Array.isArray(value) || value.some((item) => typeof item !== "string" || item.trim() === "")) {
    throw new Error(`${label} must be a list of non-empty strings`);
  }
  return value.map((item) => item.trim());
}

function optionalStringMap(value, label) {
  if (value === undefined || value === null) {
    return {};
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be a mapping of string keys to string values`);
  }

  const result = {};
  for (const [key, item] of Object.entries(value)) {
    if (typeof item !== "string" || item.trim() === "") {
      throw new Error(`${label}.${key} must be a non-empty string`);
    }
    result[key] = item.trim();
  }
  return result;
}

function requiredStringArray(value, label) {
  const values = optionalStringArray(value, label);
  if (values.length === 0) {
    throw new Error(`${label} must contain at least one value`);
  }
  return values;
}

function quoteBloblang(value) {
  return JSON.stringify(value);
}

function listCheck(field, values) {
  if (values.length === 0) {
    return null;
  }
  return `[${values.map(quoteBloblang).join(", ")}].contains(this.${field})`;
}

function handlerCheck(handler) {
  const checks = [
    "this.x_arc_dlq == null",
    listCheck("event_type", handler.subscribe.event_types),
    listCheck("aggregate_type", handler.subscribe.aggregate_types),
  ].filter(Boolean);

  if (handler.subscribe.filter) {
    checks.push(`(${handler.subscribe.filter})`);
  }

  return checks.length === 0 ? "true" : checks.join(" && ");
}

function normalizeRetry(retry = {}) {
  assertAllowedKeys(retry, ["max_attempts", "backoff", "initial_interval", "max_interval"], "retry");
  const maxAttempts = retry.max_attempts === undefined ? 4 : retry.max_attempts;
  if (!Number.isInteger(maxAttempts) || maxAttempts < 1) {
    throw new Error("retry.max_attempts must be a positive integer");
  }
  const backoff = retry.backoff === undefined ? "exponential" : requireString(retry.backoff, "retry.backoff");
  if (backoff !== "exponential") {
    throw new Error("retry.backoff currently supports only exponential");
  }

  return {
    maxAttempts,
    maxRetries: Math.max(maxAttempts - 1, 0),
    backoff,
    initialInterval: typeof retry.initial_interval === "string" ? retry.initial_interval : "2s",
    maxInterval: typeof retry.max_interval === "string" ? retry.max_interval : "1m",
  };
}

function normalizeIdempotency(idempotency = {}) {
  assertAllowedKeys(idempotency, ["key", "ordering"], "idempotency");
  const key = idempotency.key === undefined ? "event_id" : requireString(idempotency.key, "idempotency.key");
  const ordering = idempotency.ordering === undefined ? "none" : requireString(idempotency.ordering, "idempotency.ordering");

  if (key !== "event_id") {
    throw new Error("idempotency.key currently supports only event_id");
  }
  if (ordering !== "none") {
    throw new Error("idempotency.ordering currently supports only none; use sequence checks in the handler/read model");
  }

  return { key, ordering };
}

function normalizeManifest(raw) {
  const prefix = raw.__file;
  assertAllowedKeys(
    raw,
    ["name", "description", "subscribe", "delivery", "idempotency", "retry", "dead_letter", "__file"],
    prefix,
  );
  const name = requireString(raw.name, `${prefix}: name`);
  if (!/^[a-zA-Z0-9][a-zA-Z0-9_-]*$/.test(name)) {
    throw new Error(`${prefix}: name must contain only letters, numbers, underscores, and dashes`);
  }

  if (!raw.subscribe || typeof raw.subscribe !== "object" || Array.isArray(raw.subscribe)) {
    throw new Error(`${prefix}: subscribe must be a mapping`);
  }
  assertAllowedKeys(raw.subscribe, ["aggregate_types", "event_types", "filter"], `${prefix}: subscribe`);
  const subscribe = {
    aggregate_types: optionalStringArray(raw.subscribe.aggregate_types, `${prefix}: subscribe.aggregate_types`),
    event_types: requiredStringArray(raw.subscribe.event_types, `${prefix}: subscribe.event_types`),
    filter: raw.subscribe.filter === undefined ? null : requireString(raw.subscribe.filter, `${prefix}: subscribe.filter`),
  };

  if (!raw.delivery || typeof raw.delivery !== "object" || Array.isArray(raw.delivery)) {
    throw new Error(`${prefix}: delivery must be a mapping`);
  }
  assertAllowedKeys(raw.delivery, ["type", "http", "nats"], `${prefix}: delivery`);
  const deliveryType = requireString(raw.delivery.type, `${prefix}: delivery.type`);
  if (!["http", "nats"].includes(deliveryType)) {
    throw new Error(`${prefix}: delivery.type must be one of: http, nats`);
  }

  const deadLetter = raw.dead_letter ?? {};
  assertAllowedKeys(deadLetter, ["enabled"], `${prefix}: dead_letter`);
  return {
    name,
    description: typeof raw.description === "string" ? raw.description : "",
    subscribe,
    idempotency: normalizeIdempotency(raw.idempotency),
    delivery: normalizeDelivery(raw.delivery, deliveryType, prefix),
    retry: normalizeRetry(raw.retry),
    dead_letter: {
      enabled: deadLetter.enabled === undefined ? true : Boolean(deadLetter.enabled),
    },
  };
}

function normalizeDelivery(delivery, type, prefix) {
  if (!delivery[type] || typeof delivery[type] !== "object" || Array.isArray(delivery[type])) {
    throw new Error(`${prefix}: delivery.${type} must be a mapping`);
  }

  if (type === "http") {
    const http = delivery.http;
    assertAllowedKeys(http, ["url", "verb", "timeout", "headers"], `${prefix}: delivery.http`);
    return {
      type,
      http: {
        url: requireString(http.url, `${prefix}: delivery.http.url`),
        verb: typeof http.verb === "string" ? http.verb.toUpperCase() : "POST",
        timeout: typeof http.timeout === "string" ? http.timeout : "10s",
        headers: optionalStringMap(http.headers, `${prefix}: delivery.http.headers`),
      },
    };
  }

  if (type === "nats") {
    assertAllowedKeys(delivery.nats, ["subject"], `${prefix}: delivery.nats`);
    return {
      type,
      nats: {
        subject: requireString(delivery.nats.subject, `${prefix}: delivery.nats.subject`),
      },
    };
  }
}

function baseConfig(handlerCases) {
  return {
    http: {
      enabled: true,
      address: "0.0.0.0:4195",
    },
    input: {
      nats_jetstream: {
        urls: ["${NATS_URL:nats://127.0.0.1:4222}"],
        stream: "${NATS_STREAM:EVENTS}",
        subject: "events.>",
        durable: "${NATS_CONSUMER:benthos-events}",
        deliver: "all",
        ack_wait: "30s",
      },
    },
    pipeline: {
      processors: [
        {
          mapping: [
            "root = this",
            "root.envelope_version = 1",
            "root.occurred_at = this.timestamp",
            'root.subject = meta("nats_subject").or("")',
          ].join("\n"),
        },
        {
          mapping: [
            'root = if this.event_id.type() != "string" || this.event_id == "" || this.aggregate_type.type() != "string" || this.aggregate_type == "" || this.aggregate_id.type() != "string" || this.aggregate_id == "" || this.sequence.type() != "number" || this.event_type.type() != "string" || this.event_type == "" || this.timestamp.type() != "number" || this.audit.type() != "object" || this.payload.type() != "object" {',
            '  throw("invalid Arc event envelope: expected event_id, aggregate_type, aggregate_id, sequence, event_type, timestamp, audit, and payload")',
            "} else {",
            "  this",
            "}",
          ].join("\n"),
        },
        {
          catch: [
            {
              mapping: dlqMetadataMapping("envelope-validation", "envelope_validation_failed"),
            },
          ],
        },
        {
          dedupe: {
            cache: "dedupe_cache",
            key: '${! json("event_id").or(json("x_arc_dlq.fingerprint")) }',
            drop_on_err: false,
          },
        },
        {
          log: {
            level: "INFO",
            message:
              'routing event ${! json("event_type").or("invalid") } aggregate=${! json("aggregate_type").or("unknown") } id=${! json("event_id").or(json("x_arc_dlq.fingerprint")).or("unknown") } seq=${! json("sequence").or("unknown") }',
          },
        },
      ],
    },
    cache_resources: [
      {
        label: "dedupe_cache",
        memory: {
          default_ttl: "5m",
        },
      },
    ],
    output: {
      switch: {
        cases: [
          {
            check: 'this.x_arc_dlq.handler == "envelope-validation"',
            output: validationDlqOutput(),
          },
          ...handlerCases,
          {
            output: {
              stdout: {},
            },
          },
        ],
      },
    },
  };
}

function dlqMetadataMapping(handlerName, reason) {
  return [
    "root = this",
    "root.x_arc_dlq = {",
    `  "handler": ${quoteBloblang(handlerName)},`,
    `  "reason": ${quoteBloblang(reason)},`,
    '  "failed_at": now(),',
    '  "original_subject": meta("nats_subject").or(this.subject).or(""),',
    '  "fingerprint": content().hash("sha256"),',
    "}",
  ].join("\n");
}

function deliveryOutput(handler) {
  if (handler.delivery.type === "http") {
    return {
      http_client: {
        url: handler.delivery.http.url,
        verb: handler.delivery.http.verb,
        timeout: handler.delivery.http.timeout,
        headers: {
          ...handler.delivery.http.headers,
          "Content-Type": "application/json",
          "Idempotency-Key": '${! json("event_id") }',
          "X-Arc-Event-Sequence": '${! json("sequence") }',
        },
        backoff_on: [429, 500, 502, 503, 504],
      },
    };
  }

  if (handler.delivery.type === "nats") {
    return {
      nats_jetstream: {
        urls: ["${NATS_URL:nats://127.0.0.1:4222}"],
        subject: handler.delivery.nats.subject,
      },
    };
  }

  throw new Error(`unsupported delivery type: ${handler.delivery.type}`);
}

function retryOutput(handler, output) {
  return {
    retry: {
      max_retries: handler.retry.maxRetries,
      backoff: {
        initial_interval: handler.retry.initialInterval,
        max_interval: handler.retry.maxInterval,
      },
      output,
    },
  };
}

function dlqOutput(handler) {
  return {
    processors: [
      {
        mapping: dlqMetadataMapping(handler.name, "delivery_failed_after_retries"),
      },
    ],
    nats_jetstream: {
      urls: ["${NATS_URL:nats://127.0.0.1:4222}"],
      subject: `dlq.${handler.name}.\${! json("event_type").lowercase() }`,
    },
  };
}

function validationDlqOutput() {
  return {
    nats_jetstream: {
      urls: ["${NATS_URL:nats://127.0.0.1:4222}"],
      subject: 'dlq.envelope.${! json("event_type").or("invalid").lowercase() }',
    },
  };
}

function handlerCase(handler) {
  const output = retryOutput(handler, deliveryOutput(handler));
  return {
    check: handlerCheck(handler),
    output: handler.dead_letter.enabled
      ? {
          fallback: [output, dlqOutput(handler)],
        }
      : output,
  };
}

function renderConfig(config) {
  const header = [
    "# Generated by scripts/generate-benthos-config.mjs.",
    "# Do not edit by hand; edit config/handlers/*.yaml and run `make benthos-config`.",
    "# See docs/event-handlers.md for the manifest contract.",
    "",
  ].join("\n");

  return `${header}${YAML.stringify(config, { lineWidth: 0 })}`;
}

export function generateBenthosConfig(repoRoot = process.cwd()) {
  const handlersDir = path.join(repoRoot, "config", "handlers");
  const files = readManifestFiles(handlersDir);
  const handlers = files.map(parseManifest).map(normalizeManifest);
  const names = new Set();
  for (const handler of handlers) {
    if (names.has(handler.name)) {
      throw new Error(`duplicate handler name: ${handler.name}`);
    }
    names.add(handler.name);
  }
  const rendered = renderConfig(baseConfig(handlers.map(handlerCase)));
  return { rendered, handlers };
}

export function writeBenthosConfig({ repoRoot = process.cwd(), check = false } = {}) {
  const outputPath = path.join(repoRoot, "config", "benthos", "generated", "events.yaml");
  const { rendered, handlers } = generateBenthosConfig(repoRoot);
  const existing = fs.existsSync(outputPath) ? fs.readFileSync(outputPath, "utf8") : null;

  if (check) {
    if (existing !== rendered) {
      throw new Error(`${path.relative(repoRoot, outputPath)} is out of date; run make benthos-config`);
    }
    return { outputPath, handlers, changed: false };
  }

  fs.mkdirSync(path.dirname(outputPath), { recursive: true });
  fs.writeFileSync(outputPath, rendered);
  return { outputPath, handlers, changed: existing !== rendered };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const repoRoot = process.cwd();
  try {
    const result = writeBenthosConfig({ repoRoot, check: checkOnly });
    if (checkOnly) {
      console.log(`Benthos config is up to date (${result.handlers.length} handler manifest(s)).`);
    } else {
      console.log(`Wrote ${path.relative(repoRoot, result.outputPath)} from ${result.handlers.length} handler manifest(s).`);
    }
  } catch (error) {
    console.error(error.message);
    process.exit(1);
  }
}
