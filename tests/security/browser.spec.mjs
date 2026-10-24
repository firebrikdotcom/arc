import { test, expect } from '@playwright/test';
import net from 'node:net';
const baseURL = process.env.ARC_SECURITY_URL || 'http://127.0.0.1:18764';
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
    expect((await page.request.get('/ws', {headers:{Origin:'http://127.0.0.1:18764'}})).status()).toBe(401);
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
    expect((await page.request.get('/ws', {headers:{Origin:'http://127.0.0.1:18764'}})).status()).toBe(503);
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
async function rawResponseStatus(headerLines) {
  const target = new URL(baseURL);
  return new Promise((resolve, reject) => {
    const socket = net.createConnection({host: target.hostname, port: Number(target.port)}, () => {
      socket.write([
        'GET /ws HTTP/1.1',
        `Host: ${target.host}`,
        ...headerLines,
        'Connection: close',
        '',
        '',
      ].join('\r\n'));
    });
    let response = '';
    socket.setTimeout(3000, () => socket.destroy(new Error('raw HTTP request timed out')));
    socket.on('data', chunk => {
      response += chunk.toString('latin1');
      const status = response.match(/^HTTP\/1\.1 (\d{3})/);
      if (status) {
        socket.destroy();
        resolve(Number(status[1]));
      }
    });
    socket.on('error', reject);
  });
}
test('WebSocket handshake denies missing, null, malformed, and unapproved origins and unauthenticated identity', async ({request}) => {
  const allowedOrigin = 'http://127.0.0.1:18764';
  expect((await request.get('/ws')).status()).toBe(403);
  expect((await request.get('/ws', {headers:{Origin:'null'}})).status()).toBe(403);
  expect((await request.get('/ws', {headers:{Origin:'not an origin'}})).status()).toBe(403);
  expect((await request.get('/ws', {headers:{Origin:'http://127.0.0.1:18764/a/..'}})).status()).toBe(403);
  expect(await rawResponseStatus([
    'Origin: http://127.0.0.1:18764',
    'Origin: http://127.0.0.1:18764',
  ])).toBe(403);
  expect((await request.get('/ws', {headers:{Origin:'http://localhost:18764'}})).status()).toBe(403);
  expect((await request.get('/ws', {headers:{Origin:allowedOrigin}})).status()).toBe(401);
  await request.get('/fixture/cache-only');
  expect((await request.get('/ws', {headers:{Origin:allowedOrigin}})).status()).toBe(401);
});
test('verified user may join only their own room and user broadcasts stay isolated', async ({browser, request}) => {
  const alice = await browser.newContext(), bob = await browser.newContext();
  try {
    const a = await alice.newPage(), b = await bob.newPage();
    const user = await (await a.goto(`${baseURL}/fixture/login/alice`)).json();
    await b.goto(`${baseURL}/fixture/login/bob`);
    expect(await openSocket(a)).toBe(true); expect(await openSocket(b)).toBe(true);
    await subscribe(a, `user:${user.id}`);
    await subscribe(b, `user:${user.id}`);
    await subscribe(b, 'unknown');
    await request.post(`/fixture/user/${user.id}`);
    await expect.poll(() => a.evaluate(() => messages)).toContain('private-user-message');
    const pongCount = await b.evaluate(() => messages.filter(m => m === '{"type":"pong"}').length);
    await b.evaluate(() => socket.send(JSON.stringify({type:'ping'})));
    await expect.poll(() => b.evaluate(() => messages.filter(m => m === '{"type":"pong"}').length)).toBe(pongCount + 1);
    expect(await b.evaluate(() => messages)).not.toContain('private-user-message');
    await request.post(`/fixture/room/${encodeURIComponent(`user:${user.id}`)}`);
    await expect.poll(() => a.evaluate(() => messages)).toContain('private-room-message');
    const roomPongs = await b.evaluate(() => messages.filter(m => m === '{"type":"pong"}').length);
    await b.evaluate(() => socket.send(JSON.stringify({type:'ping'})));
    await expect.poll(() => b.evaluate(() => messages.filter(m => m === '{"type":"pong"}').length)).toBe(roomPongs + 1);
    expect(await b.evaluate(() => messages)).not.toContain('private-room-message');
  } finally { await alice.close(); await bob.close(); }
});
