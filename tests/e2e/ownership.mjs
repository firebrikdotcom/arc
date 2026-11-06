import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { once } from 'node:events';

function readJson(path) {
  try { return JSON.parse(readFileSync(path, 'utf8')); } catch { return undefined; }
}

export function acquireRun(root, instanceId) {
  const lock = resolve(root, '.e2e-run.lock');
  const stateFile = resolve(root, '.e2e-state.json');
  try {
    mkdirSync(lock);
  } catch (error) {
    throw new Error(`E2E run lock unavailable at ${lock}; another run or stale lock requires attention: ${error.message}`);
  }
  const ownerFile = resolve(lock, 'owner.json');
  writeFileSync(ownerFile, JSON.stringify({ instanceId }));
  const ownsLock = () => readJson(ownerFile)?.instanceId === instanceId;
  const ownsState = () => ownsLock() && readJson(stateFile)?.instanceId === instanceId;
  return {
    ownsState,
    writeState(state) {
      if (!ownsLock()) throw new Error('E2E run lost its lock');
      writeFileSync(stateFile, JSON.stringify({ ...state, instanceId }, null, 2));
    },
    release() {
      if (!ownsLock()) return;
      if (ownsState()) rmSync(stateFile, { force: true });
      rmSync(lock, { recursive: true });
    },
  };
}

// Retain the ChildProcess object in Playwright's global-setup cleanup closure.
// Serialized PIDs are diagnostic data only, never authority to signal a process.
export async function cleanupRun(lease, proc, cleanupDatabase = () => {}) {
  if (proc?.pid && proc.exitCode === null && proc.signalCode === null) {
    const exited = once(proc, 'exit');
    proc.kill('SIGTERM');
    let timer;
    try {
      await Promise.race([exited, new Promise((resolve) => { timer = setTimeout(resolve, 2_500); })]);
    } finally {
      clearTimeout(timer);
    }
    if (proc.exitCode === null && proc.signalCode === null) {
      proc.kill('SIGKILL');
      let killTimer;
      try {
        await Promise.race([exited, new Promise((_, reject) => {
          killTimer = setTimeout(() => reject(new Error('E2E owned server did not exit; preserving run lock')), 2_500);
        })]);
      } finally {
        clearTimeout(killTimer);
      }
    }
  }
  if (lease.ownsState()) cleanupDatabase();
  lease.release();
}
