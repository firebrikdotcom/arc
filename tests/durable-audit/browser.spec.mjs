import { test, expect } from '@playwright/test';
import fs from 'node:fs';
const receiptPath = process.env.ARC_AUDIT_RECEIPT || '/tmp/arc-nineties-durable-audit-receipt.json';
test('before restart: real browser disclosure commits metadata; locked journal withholds PHI', async ({page, request}) => {
  const boot = await (await request.get('/health')).text();
  expect(boot).toMatch(/^durable-audit-fixture:/);
  expect((await page.goto('/read/phi')).status()).toBe(200);
  await expect(page.locator('body')).toContainText('SYNTHETIC-PRIVATE-RESPONSE');
  const entries = await (await request.get('/fixture/entries')).json();
  expect(entries.length).toBeGreaterThan(0);
  const entry = entries.at(-1);
  expect(entry.actor.actor_id).toBe('synthetic-browser-user');
  expect(entry.actor.user_agent).toBeNull();
  expect(entry.actor.source_ip).toBeNull();
  expect(entry.actor.session_id).toBeNull();
  expect(entry.resource.fields).toEqual(['display']);
  expect(JSON.stringify(entries)).not.toContain('SYNTHETIC-PRIVATE-RESPONSE');
  fs.writeFileSync(receiptPath, JSON.stringify({boot, entry}));
  expect((await request.post('/fixture/lock')).status()).toBe(204);
  try {
    expect((await page.goto('/read/phi')).status()).toBe(503);
    await expect(page.locator('body')).not.toContainText('SYNTHETIC-PRIVATE-RESPONSE');
    expect((await page.goto('/read/pii')).status()).toBe(200);
  } finally { expect((await request.post('/fixture/unlock')).status()).toBe(204); }
  expect((await page.goto('/read/phi')).status()).toBe(200);
});
test('after restart: committed receipt survives a different server process', async ({request}) => {
  const previous = JSON.parse(fs.readFileSync(receiptPath, 'utf8'));
  const boot = await (await request.get('/health')).text();
  expect(boot).toMatch(/^durable-audit-fixture:/);
  expect(boot).not.toBe(previous.boot);
  const entries = await (await request.get('/fixture/entries')).json();
  expect(entries).toContainEqual(previous.entry);
});
