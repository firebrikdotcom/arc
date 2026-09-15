import { defineConfig } from '@playwright/test';
export default defineConfig({
  testDir: '.', testMatch: 'browser.spec.mjs', workers: 1, retries: 0,
  timeout: 15000, outputDir: '../../test-results/durable-audit', reporter: 'list',
  use: { baseURL: 'http://127.0.0.1:18784', headless: true,
    launchOptions: { executablePath: process.env.CHROMIUM_PATH || '/usr/bin/chromium', args: ['--no-sandbox'] } }
});
