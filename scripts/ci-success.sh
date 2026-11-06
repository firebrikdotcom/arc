#!/usr/bin/env bash
set -euo pipefail

# GitHub supplies every declared dependency through toJSON(needs).
node <<'NODE'
const needs = JSON.parse(process.env.NEEDS_JSON ?? '{}');
const jobs = Object.entries(needs);
if (!jobs.length || jobs.some(([, job]) => job?.result !== 'success')) {
  console.error('One or more required CI jobs did not succeed:', needs);
  process.exit(1);
}
console.log('All required CI jobs passed');
NODE
