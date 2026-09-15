import assert from 'node:assert/strict';
import { chromium } from '@playwright/test';
const base = process.env.ARC_BOUNDED_URL || 'http://127.0.0.1:18771';
const browser = await chromium.launch({ executablePath: process.env.CHROMIUM_PATH || '/usr/bin/chromium', args: ['--no-sandbox'] });
try {
  const page = await browser.newPage();
  assert.equal((await (await page.request.get(`${base}/health`)).json()).application, 'bounded-fixture');
  for (let i = 104; i >= 0; i--) {
    const response = await page.request.post(`${base}/api/products`, { data: {id: String(i).padStart(3,'0'), name: i < 103 ? 'Same É & 100%_' : 'Other'} });
    assert.equal(response.status(), 201);
  }
  for (const [query, length, next] of [['',20,'true'],['?limit=100',100,'true'],['?limit=20&offset=100',5,'false'],['?offset=1000000',0,'false']]) {
    const response = await page.request.get(`${base}/api/products${query}`);
    assert.equal(response.status(),200); assert.equal((await response.json()).length,length); assert.equal(response.headers()['x-has-next'],next);
  }
  for (const query of ['limit=','limit=0','limit=101','limit=-1','limit=no','limit=é','offset=1000001','offset=18446744073709551615','offset=18446744073709551616']) assert.equal((await page.request.get(`${base}/api/products?${query}`)).status(),400,query);
  await page.goto(`${base}/signin`);
  await page.getByLabel('Email').fill('admin@example.com');
  await page.getByLabel('Password', {exact:true}).fill('change-me-now');
  await page.getByRole('button',{name:'Sign in'}).click();
  await page.waitForURL(`${base}/admin`);
  for (const route of ['products','users']) {
    for (const query of ['page=','filter=%00','page=-1','page=no','page=é','page=18446744073709551615','page=50002',`filter=${encodeURIComponent('é'.repeat(513))}`]) assert.equal((await page.request.get(`${base}/admin/${route}?${query}`)).status(),400,`${route}:${query}`);
    assert.equal((await page.request.get(`${base}/admin/${route}?page=0`)).status(),200);
    assert.equal((await page.request.get(`${base}/admin/${route}?filter=${encodeURIComponent('é'.repeat(512))}`)).status(),200);
  }
  assert.equal((await page.request.get(`${base}/admin/products?sort=invalid`)).status(),400);
  await page.goto(`${base}/admin/products?filter=${encodeURIComponent('É & 100%_')}&sort=name_desc`);
  assert.equal(await page.locator('tbody tr').count(),20);
  assert.equal(await page.locator('tbody tr').first().locator('td').first().textContent(),'000');
  await page.getByRole('link',{name:'Next',exact:true}).click();
  assert.equal(new URL(page.url()).searchParams.get('filter'),'É & 100%_');
  assert.equal(new URL(page.url()).searchParams.get('sort'),'name_desc');
  assert.equal(await page.locator('tbody tr').first().locator('td').first().textContent(),'020');
  await page.getByRole('link',{name:'Previous',exact:true}).click();
  assert.equal(await page.locator('tbody tr').first().locator('td').first().textContent(),'000');
  await page.goto(`${base}/admin/products?filter=${encodeURIComponent('É & 100%_')}&page=6`);
  assert.equal(await page.locator('tbody tr').count(),3);
  assert.equal(await page.getByRole('link',{name:'Next',exact:true}).count(),0);
  // Create enough identities through real CSRF-protected forms to cross a page.
  for (let i=0;i<21;i++) {
    await page.goto(`${base}/admin/users/new`);
    await page.getByLabel('Name',{exact:true}).fill(`Paging ${i}`);
    await page.getByLabel('Email',{exact:true}).fill(`paging${String(i).padStart(2,'0')}@example.test`);
    await page.getByLabel('Password',{exact:true}).fill('test-password');
    await page.getByLabel('Roles').fill('user');
    await page.getByRole('button',{name:'Save user'}).click();
    await page.waitForURL(/\/admin\/users\/[^/]+$/);
  }
  await page.goto(`${base}/admin/users?filter=Paging`);
  assert.equal(await page.locator('tbody tr').count(),20);
  await page.getByRole('link',{name:'Next',exact:true}).click();
  assert.equal(new URL(page.url()).searchParams.get('filter'),'paging');
  assert.equal(await page.locator('tbody tr').count(),1);
  assert.equal(await page.getByRole('link',{name:'Next',exact:true}).count(),0);
  await page.getByRole('link',{name:'Previous',exact:true}).click();
  assert.equal(await page.locator('tbody tr').count(),20);
  console.log('PASS: generated API bounds/adversarial inputs, resource browser paging/filter/sort, admin user paging/CSRF');
} finally { await browser.close(); }
