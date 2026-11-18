#!/usr/bin/env bash
# Builds a local npm package set from tsrs's in-tree SDK (`npm/tsrs`) and bundled library
# definitions (`crates/tsrs_vfs/libs`), without touching upstream `npm/build.mjs`, and never publishes.
#
# usage:
#   scripts/fork/npm-pack.sh --name @effect-x/tsc --package-version <v> \
#     [--also <os>-<arch>=<tsrs>]... --ext <os>-<arch>=<dir>... <out-dir> <tsrs-noembed>
#
#   scripts/fork/npm-pack.sh [--package-version <v>] <out-dir> <tsrs-noembed>
#     Packs the local `typescript` + `@typescript/typescript-<host>` set for drop-in testing.
#
# Building the host inputs beforehand:
#   cargo build --release -p tsrs_cli --bin tsrs --features noembed
#   cargo build --release -p ts_ext_syntax --bin tsc-ext --lib
#   # Copy target/release/tsc-ext and target/release/libts_ext_syntax.{dylib,so} (or ts_ext_syntax.dll)
#   # as <ext-dir>/tsc-ext[.exe] and <ext-dir>/tsc-ext.node.
#
# Output:
#   With `--name @effect-x/tsc`: `<out-dir>/tsc`, `<out-dir>/tsc-<os>-<arch>`, and `effect-x-tsc-*.tgz`.
#   With `--name typescript` (default): `<out-dir>/typescript`, `<out-dir>/typescript-<host>`, and `*.tgz`.

set -euo pipefail
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
usage() { sed -n '5,20p' "$0" >&2; exit 2; }

resolve_abs() {
  node -p 'require("node:path").resolve(process.argv[1])' "$1"
}

resolve_real() {
  node -p 'require("node:fs").realpathSync(process.argv[1])' "$1"
}

name=typescript
also=()
ext=()
package_version=()
while [[ ${1:-} == --name || ${1:-} == --also || ${1:-} == --ext || ${1:-} == --package-version ]]; do
  [[ $# -ge 2 ]] || usage
  case $1 in
    --name) name=$2 ;;
    --also) also+=(--exe "$2") ;;
    --ext) ext+=(--ext "$2") ;;
    --package-version) package_version=(--package-version "$2") ;;
  esac
  shift 2
done

[[ $name == typescript || $name == @effect-x/tsc ]] || usage
[[ $name == typescript || ${#package_version[@]} == 2 ]] || usage
[[ $# == 2 ]] || usage

out_raw=$1
exe_raw=$2
mkdir -p "$out_raw"
out=$(resolve_real "$out_raw")
exe=$(resolve_real "$exe_raw")
[[ -x $exe ]] || { echo "not executable: $exe" >&2; exit 2; }

libs="$repo/crates/tsrs_vfs/libs"
[[ -f "$libs/lib.d.ts" ]] || { echo "missing bundled libs at $libs" >&2; exit 1; }

ts_version=$(node "$repo/npm/build.mjs" --print-version >/dev/null && node -e '
  import("./tools/release/version.mjs").then(m => console.log(m.readVersions(process.cwd()).typescriptVersion));
' --input-type=module)
crate_version=$(node "$repo/npm/build.mjs" --print-crate-version)

if [[ -f "$(dirname "$exe")/COMMIT" ]]; then
  git_head=$(cat "$(dirname "$exe")/COMMIT")
else
  git_head=$(git -C "$repo" rev-parse HEAD)
fi

build="$out/.build"
rm -rf "$build"
mkdir -p "$build/bin"

# Verify the binary is a `noembed` build: it reads lib.*.d.ts from its own directory.
cp "$libs"/*.d.ts "$build/bin/"
cp "$exe" "$build/bin/tsc"
chmod 0755 "$build/bin/tsc"
bin_real=$(resolve_real "$build/bin")

reported=$("$bin_real/tsc" --version)
want_prefix="Version $ts_version"
if [[ $reported != "$want_prefix" && $reported != "$want_prefix ("* ]]; then
  echo "$exe --version printed '$reported' (expected '$want_prefix...')" >&2
  exit 1
fi

echo > "$build/a.ts"
listed=$("$bin_real/tsc" --listFilesOnly --lib es5 "$build/a.ts")
if ! grep -Fqx "$bin_real/lib.es5.d.ts" <<< "$listed"; then
  echo "$exe is not a noembed build (--features noembed): it listed $(head -n 1 <<< "$listed")" >&2
  exit 1
fi

# Ensure `npm/node_modules` has `@types/node` for compiling the JS API (`npm/tsrs/src` -> `npm/tsrs/dist`).
if [[ ! -d "$repo/npm/node_modules/@types/node" && ! -d "$repo/node_modules/@types/node" ]]; then
  echo "installing npm SDK build dependencies (@types/node) via pnpm..."
  (cd "$repo" && pnpm install --frozen-lockfile --filter tsrs-npm-tooling)
fi

# Compile `npm/tsrs/src` -> `npm/tsrs/dist` using the verified `tsrs` binary itself.
node "$repo/npm/build.mjs" --sdk-only --tsc "$bin_real/tsc"

host=$(node -p 'process.platform + "-" + process.arch')
node "$repo/npm/fork/pack.mjs" \
  --exe "$host=$bin_real/tsc" \
  ${also[@]+"${also[@]}"} \
  ${ext[@]+"${ext[@]}"} \
  --libs "$libs" \
  --dist "$repo/npm/tsrs/dist" \
  --version "$ts_version" \
  --git-head "$git_head" \
  --out "$out/pkg" \
  --name "$name" \
  ${package_version[@]+"${package_version[@]}"} \
  --native-bin

pack_dir=${name##*/}
rm -rf "${out:?}/$pack_dir"
for old in "$out/$pack_dir"-* "$out/effect-x-$pack_dir"-*.tgz "$out/typescript-$pack_dir"-*.tgz; do
  [[ -e $old ]] && rm -rf "$old"
done

for d in "$out"/pkg/*; do
  mv "$d" "$out/"
  (cd "$out/$(basename "$d")" && npm pack --silent --pack-destination "$out" > /dev/null)
done
rmdir "$out/pkg"
rm -rf "$build"

shown_version=${package_version[1]:-$crate_version}
echo "packed $name@$shown_version (TypeScript $ts_version, tsrs $crate_version):"
ls -1 "$out"/*.tgz
