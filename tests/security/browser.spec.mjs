import { test, expect } from '@playwright/test';
const baseURL = process.env.ARC_SECURITY_URL || 'http://127.0.0.1:18764';
const desired = process.env.ARC_SECURITY_CONTRACTS === '1';
async function reset(request) {
  for (const name of ['alice','bob']) expect((await request.post(`/fixture/change/${name}/reset`)).status()).toBe(204);
}
test.beforeEach(async ({request}) => { await reset(request); });
for (const change of ['remove-role', 'disable']) {
  test(`browser session revoked after ${change} and remains revoked after restoration`, async ({ page, context, request }) => {
    const login = await (await page.goto('/fixture/login/alice')).json();
    const saved = await context.cookies();
    expect(saved.some(c => c.httpOnly)).toBe(true);
    expect((await page.goto('/browser/admin')).url()).toBe(`${baseURL}/browser/admin`);
    expect((await request.post(`/fixture/change/alice/${change}`)).status()).toBe(204);
    expect((await request.get('/api/admin', {headers:{Authorization:`Bearer ${login.token}`}})).status()).toBe(403);
    await page.goto('/browser/admin');
    await expect(page).toHaveURL(/\/signin$/);
    await request.post('/fixture/change/alice/reset');
    await context.addCookies(saved);
    await page.goto('/browser/admin');
    await expect(page).toHaveURL(/\/signin$/);
    await context.addCookies(saved);
    await page.goto('/resources');
    await expect(page).toHaveURL(/\/signin$/);
    await page.goto('/fixture/login/alice');
    expect((await page.goto('/browser/admin')).url()).toBe(`${baseURL}/browser/admin`);
  });
}
test('logout revokes saved cookies and preserves another valid session', async ({page, context, browser}) => {
  const other = await browser.newContext({baseURL});
  try {
    const second = await other.newPage();
    await second.goto('/fixture/login/alice');
    await page.goto('/fixture/login/alice');
    const saved = await context.cookies();
    expect(saved.some(c => c.httpOnly)).toBe(true);
    expect((await page.request.post('/fixture/logout')).status()).toBe(204);
    await context.addCookies(saved);
    await page.goto('/browser/admin');
    await expect(page).toHaveURL(/\/signin$/);
    expect((await second.goto('/browser/admin')).url()).toBe(`${baseURL}/browser/admin`);
  } finally { await other.close(); }
});
test('idle expiration durably revokes even an earlier non-expired cookie', async ({page, context}) => {
  await page.goto('/fixture/login/alice');
  const saved = await context.cookies();
  await page.request.post('/fixture/expire');
  await page.goto('/browser/admin');
  await expect(page).toHaveURL(/\/signin\?reason=idle$/);
  await context.addCookies(saved);
  await page.goto('/browser/admin');
  await expect(page).toHaveURL(/\/signin$/);
  await page.goto('/fixture/login/alice');
  expect((await page.goto('/resources')).url()).toBe(`${baseURL}/resources`);
});
test('store outage denies browser access and logout, then recovers', async ({page, request}) => {
  await page.goto('/fixture/login/alice');
  await request.post('/fixture/outage/on');
  try {
    expect((await page.goto('/browser/admin')).status()).toBe(503);
    await expect(page.locator('body')).toContainText('Authentication is temporarily unavailable.');
    expect((await page.goto('/resources')).status()).toBe(503);
    expect((await page.request.post('/fixture/logout')).status()).toBe(503);
  } finally { await request.post('/fixture/outage/off'); }
  expect((await page.goto('/browser/admin')).url()).toBe(`${baseURL}/browser/admin`);
});
test('real HTTP rejects replay and does not let a cookie override a JWT actor', async ({request, playwright}) => {
  const client = await playwright.request.newContext({baseURL});
  try {
    const alice = await (await client.get('/fixture/login/alice')).json();
    const state = await client.storageState();
    expect(state.cookies.length).toBeGreaterThan(0);
    const cookie = state.cookies.map(c => `${c.name}=${c.value}`).join('; ');
    expect((await client.get('/resources', {maxRedirects:0})).status()).toBe(200);
    const bob = await (await request.get('/fixture/login/bob')).json();
    await request.post('/fixture/change/bob/remove-role');
    expect((await client.get('/api/admin', {headers:{Authorization:`Bearer ${bob.token}`}})).status()).toBe(403);
    expect((await client.get('/api/admin', {headers:{Authorization:`Bearer ${alice.token}`}})).status()).toBe(200);
    await client.post('/fixture/logout');
    const replay = await request.get('/resources', {headers:{Cookie:cookie}, maxRedirects:0});
    expect(replay.status()).toBe(302);
    expect(replay.headers().location).toBe('/signin');
  } finally { await client.dispose(); }
});
test('reauthentication rotates the old browser handle', async ({page, context}) => {
  await page.goto('/fixture/login/alice');
  const saved = await context.cookies();
  expect(saved.length).toBeGreaterThan(0);
  await page.goto('/fixture/login/alice');
  expect((await page.goto('/resources')).url()).toBe(`${baseURL}/resources`);
  await context.addCookies(saved);
  await page.goto('/resources');
  await expect(page).toHaveURL(/\/signin$/);
});
test('production signout requires CSRF and revokes the saved cookie', async ({page, context}) => {
  await page.goto('/fixture/login/alice');
  const token = await (await page.request.get('/fixture/csrf')).text();
  const saved = await context.cookies();
  expect((await page.request.post('/signout', {form:{csrf_token:'wrong'}, maxRedirects:0})).status()).toBe(403);
  expect((await page.goto('/resources')).url()).toBe(`${baseURL}/resources`);
  expect((await page.request.post('/signout', {form:{csrf_token:token}, maxRedirects:0})).status()).toBe(303);
  await context.addCookies(saved);
  await page.goto('/admin/users');
  await expect(page).toHaveURL(/\/signin$/);
});
test('idle revocation fails closed during outage and completes after recovery', async ({page, context, request}) => {
  await page.goto('/fixture/login/alice');
  const saved = await context.cookies();
  expect(saved.length).toBeGreaterThan(0);
  await page.request.post('/fixture/expire');
  await request.post('/fixture/outage/on');
  try { expect((await page.goto('/resources')).status()).toBe(503); }
  finally { await request.post('/fixture/outage/off'); }
  await page.goto('/resources');
  await expect(page).toHaveURL(/\/signin\?reason=idle$/);
  await context.addCookies(saved);
  await page.goto('/resources');
  await expect(page).toHaveURL(/\/signin$/);
});
async function openSocket(page) {
  await page.evaluate(() => {
    window.messages = [];
    window.socket = new WebSocket(`ws://${location.host}/ws`);
    socket.onmessage = e => messages.push(e.data);
    window.opened = new Promise(resolve => { socket.onopen = () => resolve(true); socket.onerror = () => resolve(false); });
  });
  return page.evaluate(() => window.opened);
}
async function subscribe(page, room) {
  await page.evaluate(room => { socket.send(JSON.stringify({type:'subscribe',room})); socket.send(JSON.stringify({type:'ping'})); },room);
  await expect.poll(() => page.evaluate(() => messages.includes('{"type":"pong"}'))).toBe(true);
}
test('GAP anonymous socket can subscribe to private room', async ({page,request}) => {
  await page.goto('/');
  const opened = await openSocket(page);
  if (desired) { expect(opened, 'desired authenticated handshake').toBe(false); return; }
  expect(opened).toBe(true);
  await subscribe(page, 'private');
  await request.post('/fixture/room/private');
  await expect.poll(() => page.evaluate(() => messages)).toContain('private-room-message');
  await page.evaluate(() => socket.close());
});
test('user broadcasts isolate users; GAP arbitrary room permits cross-user disclosure', async ({browser, request}) => {
  const alice = await browser.newContext(), bob = await browser.newContext();
  try {
    const a = await alice.newPage(), b = await bob.newPage();
    const user = await (await a.goto(`${baseURL}/fixture/login/alice`)).json();
    await b.goto(`${baseURL}/fixture/login/bob`);
    expect(await openSocket(a)).toBe(true); expect(await openSocket(b)).toBe(true);
    await subscribe(a, `user:${user.id}`); await subscribe(b, `user:${user.id}`);
    await request.post(`/fixture/user/${user.id}`);
    await expect.poll(() => a.evaluate(() => messages)).toContain('private-user-message');
    // Ordered round-trip barrier then check Bob, instead of a negative sleep assertion.
    const pongCount = await b.evaluate(() => messages.filter(m => m === '{"type":"pong"}').length);
    await b.evaluate(() => socket.send(JSON.stringify({type:'ping'})));
    await expect.poll(() => b.evaluate(() => messages.filter(m => m === '{"type":"pong"}').length)).toBe(pongCount + 1);
    expect(await b.evaluate(() => messages)).not.toContain('private-user-message');
    await request.post(`/fixture/room/${encodeURIComponent(`user:${user.id}`)}`);
    await expect.poll(() => b.evaluate(() => messages)).toContain('private-room-message');
    if (desired) expect(await b.evaluate(() => messages), 'desired room authorization').not.toContain('private-room-message');
  } finally { await alice.close(); await bob.close(); }
});
