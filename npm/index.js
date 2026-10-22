// The c4gen binary for the platform Node runs on. The package holds one binary per
// platform under native/<platform>-<arch>/; npm/pack.mjs puts them there.

import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const BINARIES = {
  'linux-x64': 'linux-x64/c4gen',
  'linux-arm64': 'linux-arm64/c4gen',
  'darwin-x64': 'darwin-x64/c4gen',
  'darwin-arm64': 'darwin-arm64/c4gen',
  'win32-x64': 'win32-x64/c4gen.exe',
};

/**
 * The absolute path of the c4gen binary for this platform. Throws an Error that names the
 * platform when the package has no binary for it.
 */
export const binaryPath = () => {
  const platform = `${process.platform}-${process.arch}`;
  const binary = Object.hasOwn(BINARIES, platform) ? BINARIES[platform] : undefined;
  const path = binary ? fileURLToPath(new URL(`./native/${binary}`, import.meta.url)) : undefined;
  if (!path || !existsSync(path)) {
    throw new Error(`This c4gen package has no binary for ${process.platform} on ${process.arch}`);
  }
  return path;
};
