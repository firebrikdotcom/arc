import { createServer } from 'node:net';
import { setTimeout as delay } from 'node:timers/promises';

// Check before builds, database resets, or spawning: a healthy old process is
// never a valid substitute for the server this run owns.
export async function assertPortAvailable(port, host = '127.0.0.1') {
  const probe = createServer();
  await new Promise((resolve, reject) => {
    probe.once('error', (error) => reject(new Error(`E2E port ${host}:${port} is unavailable: ${error.message}`)));
    probe.listen({ port, host, exclusive: true }, resolve);
  });
  await new Promise((resolve, reject) => probe.close((error) => error ? reject(error) : resolve()));
}

export async function waitForServer(proc, urls, { timeoutMs = 15_000, pollMs = 200, expectedInstanceId } = {}) {
  if (!expectedInstanceId) throw new Error('E2E readiness requires a per-run instance ID');
  const controller = new AbortController();
  let onExit;
  let onError;
  let timer;
  const failed = new Promise((_, reject) => {
    onExit = (code, signal) => reject(new Error(`E2E server exited during startup (code=${code}, signal=${signal})`));
    onError = (error) => reject(new Error(`E2E server failed to start: ${error.message}`));
    proc.once('exit', onExit);
    proc.once('error', onError);
    timer = setTimeout(() => reject(new Error(`E2E server readiness timed out after ${timeoutMs}ms`)), timeoutMs);
    if (proc.exitCode !== null || proc.signalCode !== null) onExit(proc.exitCode, proc.signalCode);
  });
  const ready = async () => {
    while (!controller.signal.aborted) {
      let healthy = true;
      for (const url of urls) {
        try {
          const response = await fetch(url, { signal: controller.signal });
          await response.body?.cancel();
          if (!response.ok || response.headers.get('X-Arc-Instance-Id') !== expectedInstanceId) healthy = false;
        } catch {
          healthy = false;
        }
      }
      if (healthy && proc.exitCode === null && proc.signalCode === null && !controller.signal.aborted) return;
      await delay(pollMs, undefined, { signal: controller.signal });
    }
  };
  try {
    await Promise.race([failed, ready()]);
  } finally {
    controller.abort();
    clearTimeout(timer);
    proc.off('exit', onExit);
    proc.off('error', onError);
  }
}
