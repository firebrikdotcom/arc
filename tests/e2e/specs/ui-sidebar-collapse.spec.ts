import { test, expect } from '../fixtures';

async function signinSeeded(page: import('@playwright/test').Page): Promise<void> {
  await page.goto('/signin');
  await page.fill('input[name="email"]', 'jekyll@example.com');
  await page.fill('input[name="password"]', 'password');
  await page.click('button[type="submit"]');
  await page.waitForURL(/\/admin/);
}

test.describe('Workbench navigation rail', () => {
  test('collapses to glyphs, expands again, and remembers the choice', async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 800 });
    await signinSeeded(page);

    const html = page.locator('html');
    const rail = page.locator('aside.rail');
    const dashboard = rail.getByRole('link', { name: 'Dashboard' });
    const label = dashboard.locator('.rail__label');

    await expect(dashboard).toHaveAttribute('aria-current', 'page');
    await expect(label).toBeVisible();
    await expect(rail).toHaveCSS('width', '240px');

    await page.getByRole('button', { name: 'Collapse navigation' }).click();
    await expect(html).toHaveClass(/rail-collapsed/);
    await expect(rail).toHaveCSS('width', '72px');
    // Label is visually hidden but keeps the link's accessible name.
    await expect(label).toHaveCSS('position', 'absolute');
    await expect(dashboard).toBeVisible();

    // Persists across a full reload and across navigation.
    await page.reload();
    await expect(html).toHaveClass(/rail-collapsed/);
    await rail.getByRole('link', { name: 'Profile' }).click();
    await page.waitForURL(/\/admin\/profile$/);
    await expect(rail).toHaveCSS('width', '72px');
    await expect(rail.getByRole('link', { name: 'Profile' })).toHaveAttribute('aria-current', 'page');

    await page.getByRole('button', { name: 'Expand navigation' }).click();
    await expect(html).not.toHaveClass(/rail-collapsed/);
    await expect(rail).toHaveCSS('width', '240px');
  });

  test('profile page keeps its forms working in the Workbench design', async ({ page }) => {
    await signinSeeded(page);
    await page.goto('/admin/profile');
    await expect(page.getByRole('heading', { name: 'Profile', level: 1 })).toBeVisible();
    await expect(page.getByLabel('Name')).toHaveValue(/.+/);
    await expect(page.getByLabel('Old password')).toBeVisible();
  });
});
