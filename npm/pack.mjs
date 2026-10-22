// Packs the npm package @3lsl/c4gen from built binaries, at the version in Cargo.toml, and
// prints the path of the tarball. Each argument names a platform and its binary:
//
//   node npm/pack.mjs linux-x64=target/release/c4gen
//
// The release workflow passes every platform; a local build usually has just its own.
// See RELEASING.md.

import { execFileSync } from 'node:child_process';
import { chmodSync, copyFileSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const PLATFORMS = ['linux-x64', 'linux-arm64', 'darwin-x64', 'darwin-arm64', 'win32-x64'];

const fail = (message) => {
  console.error(`pack: ${message}`);
  process.exit(1);
};

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const version = /^version\s*=\s*"([^"]+)"/m.exec(readFileSync(join(root, 'Cargo.toml'), 'utf8'))?.[1];
if (!version) {
  fail('no version found in Cargo.toml');
}

const binaries = process.argv.slice(2).map((argument) => {
  const at = argument.indexOf('=');
  const platform = argument.slice(0, at);
  if (at < 1 || !PLATFORMS.includes(platform)) {
    fail(`expected <platform>=<binary>, with a platform of ${PLATFORMS.join(', ')}; got ${argument}`);
  }
  return { platform, file: resolve(argument.slice(at + 1)) };
});
if (binaries.length === 0) {
  fail('give at least one <platform>=<binary>');
}

const out = join(root, 'target', 'npm');
const stage = join(out, 'package');
rmSync(stage, { recursive: true, force: true });
mkdirSync(stage, { recursive: true });
const manifest = JSON.parse(readFileSync(join(root, 'npm', 'package.json'), 'utf8'));
writeFileSync(join(stage, 'package.json'), `${JSON.stringify({ ...manifest, version }, null, 2)}\n`);
for (const file of ['cli.js', 'index.d.ts', 'index.js']) {
  copyFileSync(join(root, 'npm', file), join(stage, file));
}
copyFileSync(join(root, 'README.md'), join(stage, 'README.md'));
for (const { platform, file } of binaries) {
  const target = join(stage, 'native', platform, platform.startsWith('win32') ? 'c4gen.exe' : 'c4gen');
  mkdirSync(dirname(target), { recursive: true });
  copyFileSync(file, target);
  // Artifacts and copies can lose the executable bit; npm keeps the one set here.
  chmodSync(target, 0o755);
}

const name = execFileSync('npm', ['pack', '--silent', '--pack-destination', out], {
  cwd: stage,
  encoding: 'utf8',
  shell: process.platform === 'win32',
}).trim();
console.log(join(out, name));
