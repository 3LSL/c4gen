// Checks a packed @3lsl/c4gen tarball: its name, version and files, and that, once installed
// with npm in an empty project, binaryPath() and the c4gen command run this platform's binary.
//
//   node npm/check.mjs <tarball> [--all-platforms]
//
// --all-platforms also requires a binary for every platform, as a release has.

import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const PLATFORMS = ['linux-x64', 'linux-arm64', 'darwin-x64', 'darwin-arm64', 'win32-x64'];

const args = process.argv.slice(2);
const tarball = args.find((argument) => !argument.startsWith('--'));
const allPlatforms = args.includes('--all-platforms');
if (!tarball) {
  console.error('usage: node npm/check.mjs <tarball> [--all-platforms]');
  process.exit(2);
}

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const version = /^version\s*=\s*"([^"]+)"/m.exec(readFileSync(join(root, 'Cargo.toml'), 'utf8'))?.[1];
const problems = [];
const run = (command, commandArgs, options = {}) =>
  execFileSync(command, commandArgs, { encoding: 'utf8', shell: process.platform === 'win32', ...options }).trim();

// What the tarball holds, with each file's mode.
const listing = run('tar', ['-tvzf', resolve(tarball)])
  .split('\n')
  .map((line) => ({ mode: line.split(/\s+/)[0], path: line.split(/\s+/).pop() }));
const has = (path) => listing.find((entry) => entry.path === `package/${path}`);
for (const file of ['package.json', 'README.md', 'cli.js', 'index.d.ts', 'index.js']) {
  if (!has(file)) {
    problems.push(`${file} is missing`);
  }
}
for (const platform of PLATFORMS) {
  const binary = `native/${platform}/${platform.startsWith('win32') ? 'c4gen.exe' : 'c4gen'}`;
  const entry = has(binary);
  if (!entry) {
    if (allPlatforms) {
      problems.push(`${binary} is missing`);
    }
  } else if (!platform.startsWith('win32') && entry.mode !== '-rwxr-xr-x') {
    problems.push(`${binary} has mode ${entry.mode}, not -rwxr-xr-x`);
  }
}

// Installed as another project would install it.
const project = mkdtempSync(join(tmpdir(), 'c4gen-check-'));
try {
  run('npm', ['init', '-y'], { cwd: project });
  run('npm', ['install', '--no-audit', '--no-fund', resolve(tarball)], { cwd: project });
  const manifest = JSON.parse(readFileSync(join(project, 'node_modules', '@3lsl', 'c4gen', 'package.json'), 'utf8'));
  if (manifest.name !== '@3lsl/c4gen' || manifest.version !== version) {
    problems.push(`the package is ${manifest.name}@${manifest.version}, not @3lsl/c4gen@${version}`);
  }
  const binary = run(process.execPath, ['--input-type=module', '-e', "import { binaryPath } from '@3lsl/c4gen'; console.log(binaryPath());"], { cwd: project, shell: false });
  const fromModule = run(binary, ['--version'], { shell: false });
  const fromCommand = run('npm', ['exec', '--no', '--', 'c4gen', '--version'], { cwd: project });
  for (const [how, answer] of [['binaryPath()', fromModule], ['the c4gen command', fromCommand]]) {
    if (answer !== `c4gen ${version}`) {
      problems.push(`${how} runs a binary that says "${answer}", not "c4gen ${version}"`);
    }
  }
} catch (error) {
  problems.push(`installing and running the package failed: ${error.message}`);
} finally {
  rmSync(project, { recursive: true, force: true });
}

if (problems.length > 0) {
  for (const problem of problems) {
    console.error(`check: ${problem}`);
  }
  process.exit(1);
}
console.log(`check: ${tarball} is @3lsl/c4gen@${version} and runs on ${process.platform}-${process.arch}`);
