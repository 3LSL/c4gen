#!/usr/bin/env node
// The c4gen command: runs the binary for this platform with the arguments given.

import { spawnSync } from 'node:child_process';
import { binaryPath } from './index.js';

let binary;
try {
  binary = binaryPath();
} catch (error) {
  console.error(`c4gen: ${error.message}`);
  process.exit(2);
}
const result = spawnSync(binary, process.argv.slice(2), { stdio: 'inherit' });
if (result.error) {
  console.error(`c4gen: ${result.error.message}`);
  process.exit(2);
}
if (result.signal) {
  process.kill(process.pid, result.signal);
}
process.exit(result.status ?? 2);
