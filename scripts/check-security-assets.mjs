import assert from 'node:assert/strict';
import { chromium } from '@playwright/test';

const baseUrl = process.argv[2];
assert(baseUrl, 'usage: node scripts/check-security-assets.mjs <demo-url>');
const browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH || undefined });
try {
  const page = await browser.newPage();
  const violations = [];
  const errors = [];
  page.on('console', message => {
    if (/Content Security Policy|content-security-policy/i.test(message.text())) violations.push(message.text());
  });
  page.on('pageerror', error => errors.push(error.message));
  const response = await page.goto(`${baseUrl}/signin`);
  assert.equal(response.status(), 200);
  assert.match(response.headers()['content-type'] || '', /^text\/html\b/, 'HTML must declare its MIME type under nosniff');
  await page.waitForFunction(() => typeof window.Toastify === 'function').catch(async error => {
    throw new Error(`${error.message}; content-type=${response.headers()['content-type']}; scripts=${await page.locator('script').count()}; errors=${JSON.stringify(errors)}; violations=${JSON.stringify(violations)}`);
  });
  assert(await page.locator('.focused-shell').isVisible(), 'Workbench sign-in layout renders under CSP');
  const assets = await page.locator('script[src^="/public/"],link[rel="stylesheet"][href^="/public/"]').evaluateAll(elements => elements.map(el => el.src || el.href));
  assert(assets.some(url => url.endsWith('.js')));
  assert(assets.some(url => url.endsWith('.css')));
  for (const url of assets) {
    const asset = await page.request.get(url);
    assert.equal(asset.status(), 200, url);
    if (new URL(url).pathname.endsWith('/workbench.js')) {
      assert.match(asset.headers()['cache-control'], /max-age=3600/, url);
      assert.match(asset.headers()['cache-control'], /must-revalidate/, url);
      assert.doesNotMatch(asset.headers()['cache-control'], /immutable/, url);
    } else {
      assert.match(new URL(url).pathname, /-[\w-]+\.(js|css)$/, 'Vite assets carry content hashes');
      assert.match(asset.headers()['cache-control'], /immutable/, url);
    }
    const cached = await page.request.get(url, { headers: { 'if-none-match': asset.headers().etag } });
    assert.equal(cached.status(), 304, url);
    assert(cached.headers()['content-security-policy']);
  }
  await page.evaluate(() => {
    window.securityNavigationMarker = 'preserved';
    const link = document.createElement('a');
    link.id = 'security-navigation';
    link.href = '/signin?security-check=1';
    link.textContent = 'Security navigation';
    document.body.append(link);
  });
  await page.locator('#security-navigation').click();
  await page.waitForURL(`${baseUrl}/signin?security-check=1`);
  assert.equal(await page.evaluate(() => window.securityNavigationMarker), 'preserved', 'Turbo navigation preserves the JS context');
  await page.evaluate(() => window.Toastify({ text: 'Security asset check', duration: 5000 }).showToast());
  await page.locator('.toastify').getByText('Security asset check').waitFor({ state: 'visible' });
  assert.deepEqual(errors, []);
  assert.deepEqual(violations, []);
  console.log('Security assets: Workbench layout, Vite JS/CSS, immutable caching/304, Turbo navigation, Toastify passed.');
} finally {
  await browser.close();
}
