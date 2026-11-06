import { test, expect, aggregateIdFromRegister } from '../fixtures';

test('profile validation retries and sequential saves preserve both forms on the original page', async ({
  page,
  request,
  uniqueEmail,
}) => {
  // This account belongs to this test; never change the seeded login's password.
  const email = uniqueEmail();
  const updatedEmail = uniqueEmail();
  const password = 'profile-original-password';
  const newPassword = 'profile-changed-password';
  await aggregateIdFromRegister(request, email, password, 'Profile Browser');
  const login = await request.post('/api/v1/login', { data: { email, password } });
  expect(login.status()).toBe(200);
  const { token } = await login.json() as { token: string };

  const unexpectedSignouts: string[] = [];
  page.on('request', (request) => {
    if (new URL(request.url()).pathname === '/signout') unexpectedSignouts.push(request.method());
  });
  try {
    await page.goto('/signin');
    await page.getByLabel('Email').fill(email);
    await page.getByLabel('Password', { exact: true }).fill(password);
    await page.getByRole('button', { name: /sign in/i }).click();
    await page.waitForURL(/\/admin/);
    await page.goto('/admin/profile');

    const profileForm = page.locator('#profile-form');
    const passwordForm = page.locator('#password-form');
    const originalToken = await profileForm.locator('[name="csrf_token"]').inputValue();
    expect(originalToken).not.toBe('');
    await expect(passwordForm.locator('[name="csrf_token"]')).toHaveValue(originalToken);

    const signout = page.getByRole('link', { name: 'Sign out', exact: true });
    await expect(signout).toHaveAttribute('data-turbo-prefetch', 'false');
    await expect(signout).toHaveAttribute('data-turbo', 'false');
    await signout.hover();
    // Turbo's hover prefetch is delayed 100ms; observe beyond that window.
    await page.waitForTimeout(350);
    expect(unexpectedSignouts, 'hover must never request the state-changing signout URL').toEqual([]);

    const feedback = async (message: string) => {
      const toast = page.locator('.toastify').filter({ hasText: message });
      await expect(toast).toBeVisible();
      await toast.locator('.toast-close').click();
      await expect(toast).toBeHidden();
      expect(unexpectedSignouts, 'closing feedback must not prefetch signout underneath it').toEqual([]);
    };
    const saveProfile = async (name: string, nextEmail: string, status: number) => {
      await profileForm.getByLabel('Name', { exact: true }).fill(name);
      await profileForm.getByLabel('Email', { exact: true }).fill(nextEmail);
      const response = page.waitForResponse((r) => r.url().endsWith('/admin/profile') && r.request().method() === 'POST');
      await profileForm.getByRole('button', { name: 'Save profile' }).click();
      const saved = await response;
      expect(saved.status()).toBe(status);
      await feedback(status === 200 ? 'Profile updated successfully' : 'Failed to update profile');
      await expect(profileForm.getByRole('button', { name: 'Save profile' })).toBeEnabled();
      await expect(profileForm.locator('[name="csrf_token"]')).toHaveValue(originalToken);
      await expect(passwordForm.locator('[name="csrf_token"]')).toHaveValue(originalToken);
    };

    // Exercise real fetch submissions and session-cookie updates without reloads.
    await saveProfile('Profile First Save', updatedEmail, 200);
    await saveProfile('Profile Second Save', updatedEmail, 200);

    // Bypass native required/minlength validation only for this submission so
    // the backend's validation error and the Stimulus retry path both execute.
    await profileForm.evaluate((form: HTMLFormElement) => { form.noValidate = true; });
    await saveProfile('', updatedEmail, 400);
    await saveProfile('Profile After Retry', updatedEmail, 200);

    await passwordForm.getByLabel('Old password', { exact: true }).fill(password);
    await passwordForm.getByLabel('New password', { exact: true }).fill(newPassword);
    const changed = page.waitForResponse((r) => r.url().endsWith('/admin/profile-password') && r.request().method() === 'POST');
    await passwordForm.getByRole('button', { name: 'Change password' }).click();
    expect((await changed).status()).toBe(200);
    await feedback('Password updated successfully');
    await expect(passwordForm.getByLabel('Old password', { exact: true })).toHaveValue('');
    await expect(passwordForm.getByLabel('New password', { exact: true })).toHaveValue('');
    await expect(passwordForm.getByRole('button', { name: 'Change password' })).toBeEnabled();
    await expect(passwordForm.locator('[name="csrf_token"]')).toHaveValue(originalToken);
    await expect(page).toHaveURL(/\/admin\/profile$/);

    const checkPassword = await request.post('/api/v1/login', {
      data: { email: updatedEmail, password: newPassword },
    });
    expect(checkPassword.status()).toBe(200);
  } finally {
    const deleted = await request.delete('/api/v1/protected/profile', {
      headers: { Authorization: `Bearer ${token}` },
    });
    expect(deleted.status(), 'clean up the test-owned account').toBe(204);
  }
});
