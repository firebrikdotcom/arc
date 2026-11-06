import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const workflow = readFileSync(new URL('../.github/workflows/ci.yml', import.meta.url), 'utf8');
const summary = workflow.slice(workflow.indexOf('\n  ci-success:'));
const required = summary.match(/needs: \[([^\]]+)\]/)[1].split(',').map((name) => name.trim());
assert(required.includes('coverage'), 'coverage must gate CI Success');
assert.match(summary, /if: always\(\)/);
assert.match(summary, /NEEDS_JSON: \$\{\{ toJSON\(needs\) \}\}/);
assert.match(summary, /run: bash scripts\/ci-success\.sh/);
const check = (needs) => spawnSync('bash', ['scripts/ci-success.sh'], {
  cwd: root, env: { ...process.env, NEEDS_JSON: JSON.stringify(needs) }, encoding: 'utf8', timeout: 5_000,
});
const success = Object.fromEntries(required.map((name) => [name, { result: 'success' }]));
assert.equal(check(success).status, 0);
for (const job of required) {
  for (const result of ['failure', 'cancelled', 'skipped', 'unknown', undefined]) {
    assert.equal(check({ ...success, [job]: { result } }).status, 1, `${job}: ${result} must fail`);
  }
}
assert.equal(check({}).status, 1, 'missing dependencies must fail');
console.log(`CI summary: ${required.length} required jobs reject every non-success result (including coverage).`);
