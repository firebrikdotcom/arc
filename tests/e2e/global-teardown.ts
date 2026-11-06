// Playwright invokes the cleanup returned by globalSetup with its retained
// ChildProcess handle. Never signal a PID from a previous run's state file.
export default async function globalTeardown(): Promise<void> {}
