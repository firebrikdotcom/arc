import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { readFileSync, mkdtempSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { acquireRun, cleanupRun } from '../tests/e2e/ownership.mjs';
import { test } from 'node:test';
import { assertPortAvailable, waitForServer } from '../tests/e2e/startup.mjs';

const expectedInstanceId = 'e2e-test-instance';
const child = (source) => spawn(process.execPath, ['-e', source], { stdio: ['ignore', 'pipe', 'pipe'] });
async function stop(proc) {
  if (proc.exitCode !== null || proc.signalCode !== null) return;
  const exited = once(proc, 'exit');
  proc.kill('SIGKILL');
  await exited;
}

test('global setup checks the port before modifying its database and uses guarded readiness', () => {
  const setup = readFileSync(new URL('../tests/e2e/global-setup.ts', import.meta.url), 'utf8');
  assert(setup.indexOf('await assertPortAvailable(port)') < setup.indexOf('// Fresh DB per run.'));
  assert.match(setup, /await waitForServer\(proc,/);
  assert.match(setup, /ARC_INSTANCE_ID: instanceId/);
  assert(setup.indexOf('acquireRun(ROOT, instanceId)') < setup.indexOf('await assertPortAvailable(port)'));
  assert.match(setup, /return cleanup;/);
  assert.match(setup, /expectedInstanceId: instanceId/);
  assert(setup.indexOf('await waitForServer(proc,') < setup.indexOf('lease.writeState('));
});

test('occupied port is rejected even when the old server is healthy', { timeout: 3_000 }, async () => {
  const oldServer = createServer((_, response) => response.end('healthy'));
  oldServer.listen(0, '127.0.0.1');
  await once(oldServer, 'listening');
  try {
    await assert.rejects(assertPortAvailable(oldServer.address().port), /port .* unavailable/);
  } finally {
    await new Promise((resolve) => oldServer.close(resolve));
  }
});

test('early child exit rejects promptly even with a healthy old server', { timeout: 3_000 }, async () => {
  const oldServer = createServer((_, response) => setTimeout(() => response.end('healthy'), 500));
  oldServer.listen(0, '127.0.0.1');
  await once(oldServer, 'listening');
  const proc = child('process.exit(23)');
  try {
    const started = Date.now();
    await assert.rejects(waitForServer(proc, [`http://127.0.0.1:${oldServer.address().port}/health`], { expectedInstanceId, timeoutMs: 2_000 }), /exited during startup.*code=23/);
    assert(Date.now() - started < 1_500, 'must observe child exit before readiness timeout');
  } finally {
    await stop(proc);
    oldServer.closeAllConnections();
    await new Promise((resolve) => oldServer.close(resolve));
  }
});

test('both health endpoints from the live child must become ready', { timeout: 3_000 }, async () => {
  const proc = child(`const http = require('node:http'); let checks = 0; const server = http.createServer((req, res) => { if (req.url === '/__diag__/health' && ++checks < 2) res.statusCode = 503; res.setHeader('X-Arc-Instance-Id', 'e2e-test-instance'); res.end('ok'); }); server.listen(0, '127.0.0.1', () => console.log(server.address().port));`);
  try {
    const [data] = await once(proc.stdout, 'data');
    const port = Number(data.toString().trim());
    await waitForServer(proc, [`http://127.0.0.1:${port}/health`, `http://127.0.0.1:${port}/__diag__/health`], { expectedInstanceId, timeoutMs: 2_000, pollMs: 10 });
    assert.equal(proc.exitCode, null);
  } finally {
    await stop(proc);
  }
});

test('a hanging health response is bounded by the overall deadline', { timeout: 3_000 }, async () => {
  const proc = child(`require('node:http').createServer(() => {}).listen(0, '127.0.0.1', function() { console.log(this.address().port); });`);
  try {
    const [data] = await once(proc.stdout, 'data');
    await assert.rejects(waitForServer(proc, [`http://127.0.0.1:${Number(data.toString().trim())}/health`], { expectedInstanceId, timeoutMs: 100 }), /timed out after 100ms/);
  } finally {
    await stop(proc);
  }
});

test('spawn errors are reported through readiness', { timeout: 3_000 }, async () => {
  const proc = spawn('/nonexistent-arc-e2e-binary', [], { stdio: 'ignore' });
  await assert.rejects(waitForServer(proc, ['http://127.0.0.1:1/health'], { expectedInstanceId, timeoutMs: 1_000 }), /failed to start/);
});


test('healthy wrong-instance responses cannot satisfy readiness while the intended child lives', { timeout: 3_000 }, async () => {
  const proc = child(`require('node:http').createServer((req, res) => { res.setHeader('X-Arc-Instance-Id', 'old-server-instance'); res.end('healthy'); }).listen(0, '127.0.0.1', function() { console.log(this.address().port); });`);
  try {
    const [data] = await once(proc.stdout, 'data');
    await assert.rejects(waitForServer(proc, [`http://127.0.0.1:${Number(data.toString().trim())}/health`], { expectedInstanceId, timeoutMs: 150, pollMs: 10 }), /timed out after 150ms/);
  } finally {
    await stop(proc);
  }
});

test('readiness refuses an unspecified expected instance', async () => {
  await assert.rejects(waitForServer({}, [], {}), /requires a per-run instance ID/);
});


test('atomic run lock rejects contention and preserves winner state', async () => {
  const root = mkdtempSync(resolve(tmpdir(), 'arc-e2e-ownership-'));
  try {
    const winner = acquireRun(root, 'winner');
    winner.writeState({ pid: process.pid });
    assert.throws(() => acquireRun(root, 'loser'), /run lock unavailable/);
    assert.equal(JSON.parse(readFileSync(resolve(root, '.e2e-state.json'))).instanceId, 'winner');
    await cleanupRun(winner);
    assert.equal(existsSync(resolve(root, '.e2e-state.json')), false);
    const next = acquireRun(root, 'next');
    next.release();
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('stale PID state never kills an unrelated live process and foreign state is preserved', { timeout: 3_000 }, async () => {
  const root = mkdtempSync(resolve(tmpdir(), 'arc-e2e-ownership-'));
  const unrelated = child('setInterval(() => {}, 1000)');
  try {
    writeFileSync(resolve(root, '.e2e-state.json'), JSON.stringify({ pid: unrelated.pid, instanceId: 'stale' }));
    const lease = acquireRun(root, 'current');
    let dbCleaned = false;
    await cleanupRun(lease, undefined, () => { dbCleaned = true; });
    await new Promise((resolve) => setTimeout(resolve, 50));
    process.kill(unrelated.pid, 0);
    assert.equal(unrelated.exitCode, null);
    assert.equal(unrelated.signalCode, null);
    assert.equal(dbCleaned, false);
    assert.equal(JSON.parse(readFileSync(resolve(root, '.e2e-state.json'))).instanceId, 'stale');
  } finally {
    await stop(unrelated);
    rmSync(root, { recursive: true, force: true });
  }
});

test('cleanup uses owned child handle and removes only its own state', { timeout: 3_000 }, async () => {
  const root = mkdtempSync(resolve(tmpdir(), 'arc-e2e-ownership-'));
  const proc = child('setInterval(() => {}, 1000)');
  try {
    const lease = acquireRun(root, 'current');
    lease.writeState({ pid: process.pid }); // A bogus persisted PID is never used.
    let dbCleaned = false;
    await cleanupRun(lease, proc, () => { dbCleaned = true; });
    assert.equal(proc.signalCode, 'SIGTERM');
    assert.equal(dbCleaned, true);
    assert.equal(existsSync(resolve(root, '.e2e-state.json')), false);
    assert.equal(existsSync(resolve(root, '.e2e-run.lock')), false);
  } finally {
    await stop(proc);
    rmSync(root, { recursive: true, force: true });
  }
});

test('stale run lock fails closed without deleting prior ownership', () => {
  const root = mkdtempSync(resolve(tmpdir(), 'arc-e2e-ownership-'));
  try {
    acquireRun(root, 'abandoned');
    assert.throws(() => acquireRun(root, 'new-run'), /stale lock requires attention/);
    assert.equal(JSON.parse(readFileSync(resolve(root, '.e2e-run.lock/owner.json'))).instanceId, 'abandoned');
  } finally { rmSync(root, { recursive: true, force: true }); }
});
