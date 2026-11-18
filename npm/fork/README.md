# Fork npm packages (`@effect-x/tsc`)

Assembles `@effect-x/tsc` (and local drop-in `typescript` test packages) from `tsrs`'s in-tree
TypeScript 7 SDK (`npm/tsrs`) and bundled library definitions (`crates/tsrs_vfs/libs`), while
keeping upstream `npm/build.mjs` (`tsrs` + `@tsrs/*`) untouched.

## Package layout

- **`@effect-x/tsc`**:
  - `bin/tsc` and `bin/tsc-ext` launchers (rewritten on POSIX by postinstall `lib/install.js` as a
    `sh` + ESM polyglot so `tsc` executes the native binary directly without Node startup cost).
  - `tsdk/tsgo` wrapper for VS Code native SDK discovery (`js/ts.tsdk.path`).
  - `dist/`: TypeScript 7 JavaScript API (`@effect-x/tsc/sync`, `/async`, `/ast`, `/vscode`, ...).
  - `lib/ext/`: Sweetener + TypeGPU Node API (`@effect-x/tsc/ext`, `/unstable/mapped`, `/unplugin`).
  - `typescript.contentMapper` manifest (`exec: ["./bin/tsc-ext"]`), automatically resolved by `tsc`
    to the native `lib/tsc-ext` binary inside `@effect-x/tsc-<platform>-<arch>`.
- **`@effect-x/tsc-<platform>-<arch>`**:
  - `lib/tsc` (`lib/tsc.exe` on Windows): `noembed` build of `tsrs_cli`.
  - `lib/lib.*.d.ts`: TypeScript default library definitions read adjacent to `lib/tsc`.
  - `lib/tsc-ext` (`lib/tsc-ext.exe` on Windows): Sweetener + TypeGPU content-mapper CLI.
  - `lib/tsc-ext.node`: N-API native addon for `@effect-x/tsc/ext` and `/unplugin`.

## Local build, pack, and test

```sh
# 1. Build a `noembed` tsrs binary + tsc-ext CLI & N-API cdylib
cargo build --release -p tsrs_cli --bin tsrs --features noembed
cargo build --release -p ts_ext_syntax --bin tsc-ext --lib

# 2. Stage host tsc-ext artifacts (<host> = darwin-arm64, linux-x64, etc.)
host=$(node -p 'process.platform + "-" + process.arch')
mkdir -p target/ext-$host
cp target/release/tsc-ext target/ext-$host/tsc-ext
# macOS: libts_ext_syntax.dylib | Linux: libts_ext_syntax.so | Windows: ts_ext_syntax.dll
cp target/release/libts_ext_syntax.dylib target/ext-$host/tsc-ext.node

# 3. Pack @effect-x/tsc + @effect-x/tsc-<host>
scripts/fork/npm-pack.sh --name @effect-x/tsc --package-version 0.1.0-local.1 \
  --ext "$host=target/ext-$host" target/npm-pkg target/release/tsrs

# 4. Run end-to-end tarball install & compilation verification
scripts/fork/npm-test.sh --name @effect-x/tsc target/npm-pkg target/npm-test
```

To pack multiple platforms into one `@effect-x/tsc` release set, pass `--also <os>-<arch>=<tsrs>`
and `--ext <os>-<arch>=<dir>` for each additional target platform.
