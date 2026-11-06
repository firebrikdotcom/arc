import { spawn, ChildProcess, execSync, execFileSync } from 'node:child_process';
import { existsSync, rmSync, mkdirSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { assertPortAvailable, waitForServer } from './startup.mjs';
import { acquireRun, cleanupRun } from './ownership.mjs';

const ROOT = resolve(__dirname, '../..');
/**
 * Read .env.e2e once and merge into the current process so spawned children
 * inherit deterministic secrets and ports without polluting the user's shell.
 */
function loadEnvE2e(): Record<string, string> {
  const path = resolve(ROOT, '.env.e2e');
  const text = readFileSync(path, 'utf8');
  const out: Record<string, string> = {};
  for (const line of text.split(/\r?\n/)) {
    const t = line.trim();
    if (!t || t.startsWith('#')) continue;
    const eq = t.indexOf('=');
    if (eq < 0) continue;
    const k = t.slice(0, eq).trim();
    let v = t.slice(eq + 1).trim();
    if (v.startsWith('"') && v.endsWith('"')) v = v.slice(1, -1);
    out[k] = v;
  }
  return out;
}

export default async function globalSetup(): Promise<() => Promise<void>> {
  const env = loadEnvE2e();
  const port = Number(process.env.APP_PORT ?? env.APP_PORT ?? 18080);
  const dbUrl = env.DATABASE_URL ?? 'database/database-e2e.sqlite';
  const dbPath = resolve(ROOT, dbUrl);

  const instanceId = randomUUID();
  const lease = acquireRun(ROOT, instanceId);
  let proc: ChildProcess | undefined;
  const cleanup = () => cleanupRun(lease, proc, () => {
    for (const suffix of ['', '-shm', '-wal']) rmSync(`${dbPath}${suffix}`, { force: true });
  });
  try {
    await assertPortAvailable(port);

    // Fresh DB per run.
    for (const suffix of ['', '-shm', '-wal']) {
      const p = `${dbPath}${suffix}`;
      if (existsSync(p)) rmSync(p);
    }
    mkdirSync(resolve(ROOT, 'database'), { recursive: true });

    const childEnv = { ...process.env, ...env, APP_PORT: String(port), ARC_INSTANCE_ID: instanceId };

    // Build the binary once. Skip when already built (developers iterating).
    if (!process.env.E2E_SKIP_BUILD) {
      process.stdout.write('[e2e] cargo build...\n');
      execSync('cargo build -p arc --bin arc', { cwd: ROOT, stdio: 'inherit', env: childEnv });
    }

    // Build frontend assets so /signin renders fully.
    if (!process.env.E2E_SKIP_FRONTEND) {
      process.stdout.write('[e2e] vite build...\n');
      execSync('npm run build', { cwd: ROOT, stdio: 'inherit', env: childEnv });
    }

    const binary = resolve(
      ROOT, process.env.CARGO_TARGET_DIR ?? 'target',
      'debug', process.platform === 'win32' ? 'arc.exe' : 'arc',
    );

    // Migrations + seed (legacy users for /signin form flow).
    process.stdout.write('[e2e] migrate + seed...\n');
    execFileSync(binary, ['migrate'], { cwd: ROOT, stdio: 'inherit', env: childEnv });
    execFileSync(binary, ['seed'], { cwd: ROOT, stdio: 'inherit', env: childEnv });

    // Spawn the server.
    process.stdout.write(`[e2e] spawning server on :${port}...\n`);
    proc = spawn(binary, ['serve'], {
      cwd: ROOT,
      env: childEnv,
      stdio: ['ignore', 'inherit', 'inherit'],
      detached: false,
    });

    await waitForServer(proc, [
      `http://127.0.0.1:${port}/health`,
      `http://127.0.0.1:${port}/__diag__/health`,
    ], { expectedInstanceId: instanceId });
    // Teardown may only own a process that passed this run's startup checks.
    lease.writeState({ pid: proc.pid!, port, dbUrl });
    process.stdout.write('[e2e] server ready\n');
    return cleanup;
  } catch (error) {
    await cleanup();
    throw error;
  }
}
