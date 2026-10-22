# Releasing c4gen

A release is a GitHub release with one asset: the npm package `@3lsl/c4gen`, which holds the `c4gen` binary for Linux (x64 and arm64), macOS (x64 and arm64) and Windows (x64). The package is not published on the npm registry; projects depend on the tarball's URL.

## Making a release

1. Set the new version in `Cargo.toml`, run `cargo build` so that `Cargo.lock` follows, and commit both to `main`. Versions follow [semantic versioning](https://semver.org/): a change to the command line or the library that breaks a caller needs a new major version (a new minor version while it is 0.x).
2. Tag that commit `v<version>` and push the tag:

   ```console
   $ git tag v0.1.0
   $ git push origin v0.1.0
   ```

3. The release workflow, `.github/workflows/release.yml`, builds and tests c4gen on each platform, packs the package with `npm/pack.mjs`, checks it with `npm/check.mjs` and creates the release with `3lsl-c4gen-<version>.tgz` attached. It stops if the tag is not `v` followed by the version in `Cargo.toml`.

A project that uses the release adds it to its `package.json`:

```json
"@3lsl/c4gen": "https://github.com/3LSL/c4gen/releases/download/v0.1.0/3lsl-c4gen-0.1.0.tgz"
```

If the workflow fails, fix the cause, then delete the tag (and the release, if one was made) and push the tag again:

```console
$ gh release delete v0.1.0 --yes          # only if the release was made
$ git push --delete origin v0.1.0
$ git tag -f v0.1.0
$ git push origin v0.1.0
```

The workflow also runs, without making a release, on every pull request.

## Trying an unreleased build in another project

Pack the binary built on this machine, check it, and install the tarball in the other project without saving it to that project's `package.json`:

```console
$ cargo build --release
$ node npm/pack.mjs linux-x64=target/release/c4gen      # prints the tarball's path
$ node npm/check.mjs target/npm/3lsl-c4gen-<version>.tgz
$ cd ../other-project
$ npm install --no-save ../c4gen/target/npm/3lsl-c4gen-<version>.tgz
```

Name this machine's platform in the first argument: `linux-x64`, `linux-arm64`, `darwin-x64`, `darwin-arm64` or `win32-x64` (with `target/release/c4gen.exe`). A package packed this way holds only that binary. Running `npm install` in the other project again goes back to the release its `package.json` names.
