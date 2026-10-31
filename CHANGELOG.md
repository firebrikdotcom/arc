# Changelog

## Unreleased — 0.8.12 candidate

- Add headless `arc-stack` for shared event-sourced stack assembly and retain
  the existing `arc-web` stack/config public paths as re-exports.
- Add `arc-ffi` native adapter infrastructure with an owned async runtime,
  shared core registry, and UniFFI support; Android uses FFI only while web UIs
  are for web deployments.
- SQLite storage is being updated to wait up to 5 seconds for write contention
  and acquire write transactions immediately. Journal mode remains unchanged;
  see [Upgrading](docs/upgrading.md). This behavior change is not yet released.
- This candidate has not been published; release 0.8.12 needs authorization.
