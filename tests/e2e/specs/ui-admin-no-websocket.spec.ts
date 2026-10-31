import { test, expect } from '../fixtures';

// The reference app does not mount `/ws`: no page subscribes to a room, and its
// browser login has no server-side session handle a WebSocket principal
// provider could verify. The fail-closed handshake itself is covered by
// tests/security/browser.spec.mjs against a fixture wired per
// docs/websocket-security.md.
test.describe('Admin page without a WebSocket', () => {
  test('admin requires sign-in, then renders without opening a socket', async ({ page }) => {
    const sockets: string[] = [];
    page.on('websocket', (ws) => sockets.push(ws.url()));

    await page.goto('/admin');
    await expect(page).toHaveURL(/\/signin$/);

    await page.fill('input[name="email"]', 'jekyll@example.com');
    await page.fill('input[name="password"]', 'password');
    await page.click('button[type="submit"]');
    await page.waitForURL(/\/admin$/);
    await expect(page.getByRole('main')).toContainText('Dashboard');

    await page.waitForLoadState('networkidle');
    expect(sockets).toEqual([]);
  });

  test('/ws is not routed', async ({ request, baseURL }) => {
    const response = await request.get('/ws', { headers: { Origin: new URL(baseURL!).origin } });
    expect(response.status()).toBe(404);
  });
});
