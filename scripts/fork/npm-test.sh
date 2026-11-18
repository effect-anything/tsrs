#!/usr/bin/env bash
# Installs an `npm-pack.sh` package set into a fresh project (macOS & Linux portable) and checks:
#   - `bin/tsc` is rewritten as the POSIX sh+ESM polyglot and runs without Node (`NODE_OPTIONS` poison test),
#     under `node bin/tsc`, via `lib/tsc.js`, and via `tsdk/tsgo`.
#   - `--version` and `--listFilesOnly` resolve `lib.es5.d.ts` in the platform package's `lib/` dir.
#   - Postinstall safety: a failed `chmod` leaves no `.tmp` file and keeps `bin/tsc`; a broken native
#     path falls back cleanly to the Node launcher; moving the project directory still works.
#   - For `--name @effect-x/tsc`:
#       1. `bin/tsc-ext` CLI (`tsc-ext expand`) works.
#       2. `@effect-x/tsc/ext` loads the native `lib/tsc-ext.node` addon (`Transformer`).
#       3. `@effect-x/tsc` root export and `@effect-x/tsc/ast` JS SDK load cleanly.
#       4. `tsc --runExternalCode` compiles a project combining `.sts` Sweetener macros and `"use gpu"`
#          operator overloads (`Symbol.operatorPlus`), producing matching output across direct, `.bin/tsc`,
#          `node bin/tsc`, and `lib/tsc.js` invocations.
#   - `pnpm` install verification (when `pnpm` is on PATH).
#
# usage: scripts/fork/npm-test.sh [--name @effect-x/tsc] <pkg-dir> <work-dir>

set -uo pipefail
usage() { sed -n '16,16p' "$0" >&2; exit 2; }

name=typescript
if [[ ${1:-} == --name ]]; then
  [[ $# -ge 2 ]] || usage
  name=$2
  shift 2
fi
[[ $# == 2 ]] || usage

host=$(node -p 'process.platform + "-" + process.arch')
case $name in
  typescript)
    plat="@typescript/typescript-$host"
    main_tgz_glob="typescript-[0-9]*.tgz"
    plat_tgz_glob="typescript-typescript-$host-*.tgz"
    ;;
  @effect-x/tsc)
    plat="@effect-x/tsc-$host"
    main_tgz_glob="effect-x-tsc-[0-9]*.tgz"
    plat_tgz_glob="effect-x-tsc-$host-*.tgz"
    ;;
  *) usage ;;
esac

pkg=$(node -p 'require("node:fs").realpathSync(process.argv[1])' "$1")
work=$(node -p 'require("node:path").resolve(process.argv[1])' "$2")
rm -rf "$work"
mkdir -p "$work/proj" "$work/out"
work=$(node -p 'require("node:fs").realpathSync(process.argv[1])' "$work")
proj="$work/proj"
out="$work/out"

shopt -s nullglob
plat_matches=("$pkg"/$plat_tgz_glob)
main_matches=("$pkg"/$main_tgz_glob)
shopt -u nullglob

if [[ ${#plat_matches[@]} != 1 || ${#main_matches[@]} != 1 ]]; then
  echo "expected 1 main tarball ($main_tgz_glob) and 1 platform tarball ($plat_tgz_glob) in $pkg" >&2
  ls -la "$pkg" >&2
  exit 2
fi
tgz=("${plat_matches[0]}" "${main_matches[0]}")

# For @effect-x/tsc, `unplugin` is a dependency in package.json; install offline/prefer-offline or omit external deps if offline.
echo '{"name":"npm-test","private":true,"type":"module"}' > "$proj/package.json"
(cd "$proj" && npm install --prefer-offline --no-audit --no-fund --silent "${tgz[@]}") || {
  echo "npm install failed" >&2
  exit 1
}

fails=0
check() {
  if [[ $2 == 0 ]]; then
    echo "ok   $1"
  else
    echo "FAIL $1${3:+: $3}"
    fails=$((fails + 1))
  fi
}

sha256_file() {
  node -e '
    const fs = require("node:fs");
    const crypto = require("node:crypto");
    console.log(crypto.createHash("sha256").update(fs.readFileSync(process.argv[1])).digest("hex"));
  ' "$1"
}

real_path() {
  node -p 'require("node:fs").realpathSync(process.argv[1])' "$1"
}

nm="$proj/node_modules"
bin=tsc
direct="$nm/$plat/lib/tsc"
want=$("$direct" --version)
native=0
grep -q '"postinstall"' "$nm/$name/package.json" && native=1
echo > "$out/a.ts"

bins() {
  local tag=$1 nm_dir=$2 lib_dir listed v env_args=() how=""
  local b="$nm_dir/$name/bin/$bin"
  if ((native)); then
    [[ -f $b && ! -L $b && -x $b && $(sed -n '1p;3p' "$b") == $'#!/bin/sh\nimport "../lib/tsc.js";' ]]
    check "$tag bin/$bin is the sh and JS polyglot" $? "$(ls -l "$b")"
    env_args=(NODE_OPTIONS=--require=/npm-test/no-node)
    how=" without Node"
  else
    [[ ! -L $b ]] && head -n 1 "$b" | grep -q node
    check "$tag bin/$bin is JS launcher" $?
  fi

  for x in "$b" "$nm_dir/.bin/$bin"; do
    [[ $(env ${env_args[@]+"${env_args[@]}"} "$x" --version 2>&1) == "$want" ]]
    check "$tag ${x#"$nm_dir"/} --version$how: $want" $?
  done

  v=$(node "$b" --version 2>&1)
  [[ $v == "$want" ]]
  check "$tag node $name/bin/$bin --version" $? "$v"

  v=$(node "$nm_dir/$name/tsdk/tsgo" --version 2>&1)
  [[ $v == "$want" ]]
  check "$tag node $name/tsdk/tsgo --version" $? "$v"

  local pkg_parent
  pkg_parent=$(dirname "$(real_path "$nm_dir/$name")")
  if [[ $name == @* ]]; then
    pkg_parent=$(dirname "$pkg_parent")
  fi
  lib_dir=$(real_path "$pkg_parent/$plat/lib")

  listed=$(cd "$out" && "$nm_dir/.bin/$bin" --listFilesOnly --lib es5 a.ts 2>&1)
  grep -Fqx "$lib_dir/lib.es5.d.ts" <<< "$listed"
  check "$tag .bin/$bin lists $lib_dir/lib.es5.d.ts" $? "$(head -n 1 <<< "$listed")"

  listed=$(cd "$out" && node "$b" --listFilesOnly --lib es5 a.ts 2>&1)
  grep -Fqx "$lib_dir/lib.es5.d.ts" <<< "$listed"
  check "$tag node bin/$bin lists $lib_dir/lib.es5.d.ts" $? "$(head -n 1 <<< "$listed")"
}

bins npm "$nm"

if ((native)); then
  b="$nm/$name/bin/$bin"
  before=$(sha256_file "$b")
  printf '%s\n' \
    'const fs = require("node:fs");' \
    'fs.chmodSync = () => { throw Object.assign(new Error("EPERM: chmod"), { code: "EPERM" }); };' \
    > "$work/chmod-fails.cjs"
  warn=$(node --require "$work/chmod-fails.cjs" "$nm/$name/lib/install.js" 2>&1)
  tmps=$(find "$nm/$name/bin" -name "$bin.*.tmp")
  [[ -z $tmps && $(sha256_file "$b") == "$before" && $warn == *EPERM* ]]
  check "failed chmod in postinstall leaves no temp file and keeps bin/$bin" $? "${tmps:-$warn}"

  cp -p "$b" "$work/bin.keep"
  node -e '
    const fs = require("node:fs");
    const f = process.argv[1];
    const s = fs.readFileSync(f, "utf8").replace(/t="\$\{p%\/\*\}\/[^"]*"/, "t=\"${p%/*}/../no-such-dir/tsc\"");
    fs.writeFileSync(f, s);
  ' "$b"
  v=$("$nm/.bin/$bin" --version 2>&1)
  [[ $v == "$want" ]] && grep -q "no-such-dir" "$b"
  check "broken native path falls back to Node launcher" $? "$v"
  cp -p "$work/bin.keep" "$b"
fi

[[ $(node "$nm/$name/lib/tsc.js" --version) == "$want" ]]
check "lib/tsc.js --version" $?

# Check SDK & extension exports and end-to-end compilation in `proj`.
if [[ $name == @effect-x/tsc ]]; then
  ext_help=$("$nm/.bin/tsc-ext" --help 2>&1)
  grep -q "expand" <<< "$ext_help"
  check ".bin/tsc-ext --help runs native tsc-ext" $?

  node_api_out=$(cd "$proj" && node -e '
    import("@effect-x/tsc").then(async root => {
      const ast = await import("@effect-x/tsc/ast");
      const { Transformer } = await import("@effect-x/tsc/ext");
      const t = new Transformer();
      const res = t.transform("syntax id:expr { rule { id($x:expr) } => { $x } }\nexport const n = id(42);\n", "/virtual/mod.sts");
      if (!root.typescriptVersion || typeof ast.SyntaxKind !== "object" || !res.changed || !res.code.includes("export const n = 42;")) {
        process.exit(1);
      }
      console.log("ok");
    });
  ' 2>&1)
  [[ $node_api_out == "ok" ]]
  check "@effect-x/tsc root, /ast SDK, and /ext native Transformer load" $? "$node_api_out"

  mkdir -p "$proj/src"
  cat > "$proj/src/macros.sts" <<'EOF'
export syntax twice:expr {
  rule { twice($x:expr) } => { [$x, $x] }
}
EOF
  cat > "$proj/src/app.sts" <<'EOF'
import { twice } from "./macros.sts" for syntax;
export const pair: [number, number] = twice(21);
EOF
  cat > "$proj/src/gpu.ts" <<'EOF'
declare global {
  interface SymbolConstructor {
    readonly operatorPlus: unique symbol;
  }
}
export class V2 {
  constructor(public x: number, public y: number) {}
  [Symbol.operatorPlus](a: V2, b: V2): V2 {
    return new V2(a.x + b.x, a.y + b.y);
  }
}
export function addV(a: V2, b: V2): V2 {
  "use gpu";
  return a + b;
}
EOF
  cat > "$proj/tsconfig.json" <<'EOF'
{
  "compilerOptions": {
    "target": "es2022",
    "module": "es2022",
    "rootDir": "src",
    "declaration": true,
    "sourceMap": true,
    "strict": true
  },
  "contentMappers": [
    { "package": "@effect-x/tsc", "extensions": [".sts", ".stsx"] }
  ],
  "include": ["src"]
}
EOF

  run_way() {
    local label=$1; shift
    (cd "$proj" && "$@" --runExternalCode -p tsconfig.json --outDir "$out/emit-$label" --pretty false > "$out/emit-$label.stdout" 2>&1)
    echo "exit $?" >> "$out/emit-$label.stdout"
  }
  run_way direct "$direct"
  run_way npm "$nm/.bin/$bin"
  run_way node node "$nm/$name/bin/$bin"
  run_way js node "$nm/$name/lib/tsc.js"

  grep -q "exit 0" "$out/emit-direct.stdout" && grep -q "__TYPEGPU_META__" "$out/emit-direct/gpu.js" && [[ -f "$out/emit-direct/app.d.sts.ts" && -f "$out/emit-direct/gpu.d.ts" ]]
  check "direct compile of .sts + use gpu succeeds and emits .js + .d.ts" $? "$(cat "$out/emit-direct.stdout")"

  for w in npm node js; do
    cmp -s "$out/emit-direct.stdout" "$out/emit-$w.stdout"
    check "$w stdout & exit code match direct" $?
    diff -rq "$out/emit-direct" "$out/emit-$w" > /dev/null 2>&1
    check "$w emitted files match direct" $?
  done

  mkdir -p "$nm/effect" "$proj/effect-src"
  cat > "$nm/effect/package.json" <<'EOF'
{ "name": "effect", "version": "3.19.0", "types": "./index.d.ts" }
EOF
  cat > "$nm/effect/index.d.ts" <<'EOF'
export declare const EffectTypeId: unique symbol;
export interface Variance<out A, out E, out R> {
  readonly _A: () => A;
  readonly _E: () => E;
  readonly _R: () => R;
}
export interface Effect<out A, out E = never, out R = never> {
  readonly [EffectTypeId]: Variance<A, E, R>;
}
export declare namespace Effect {
  export function succeed<A>(value: A): Effect<A>;
}
EOF
  cat > "$proj/effect-src/check.sts" <<'EOF'
import { Effect } from "effect";
syntax makeFloating:expr {
  rule { makeFloating() } => { Effect.succeed(1) }
}
makeFloating();
// @effect-diagnostics-next-line floatingEffect:off
makeFloating();
EOF
  cat > "$proj/tsconfig.effect.json" <<'EOF'
{
  "compilerOptions": {
    "target": "es2022",
    "module": "esnext",
    "moduleResolution": "bundler",
    "strict": true,
    "skipLibCheck": true,
    "noEmit": true,
    "plugins": [{ "name": "@effect/language-service" }]
  },
  "contentMappers": [
    { "package": "@effect-x/tsc", "extensions": [".sts", ".stsx"] }
  ],
  "include": ["effect-src"]
}
EOF
  effect_out=$(cd "$proj" && "$nm/.bin/$bin" --runExternalCode -p tsconfig.effect.json --pretty false 2>&1)
  effect_rc=$?
  [[ $effect_rc != 0 ]] && grep -q "effect-src/check.sts(5,1): error TS377001:" <<< "$effect_out" && ! grep -q "effect-src/check.sts(7," <<< "$effect_out"
  check "@effect/language-service plugin reports TS377001 on .sts and honors @effect-diagnostics directive" $? "$effect_out"
fi

# Moving the project directory keeps relative paths working.
mv "$proj" "$work/moved"
bins moved "$work/moved/node_modules"
mv "$work/moved" "$proj"

if command -v pnpm > /dev/null && [[ $name == typescript ]]; then
  pp="$work/pnpm"
  mkdir -p "$pp"
  echo "{\"name\":\"npm-test-pnpm\",\"private\":true,\"dependencies\":{\"$name\":\"file:${tgz[1]}\"}}" > "$pp/package.json"
  printf 'overrides:\n  "%s": "file:%s"\nallowBuilds:\n  "%s": true\n' "$plat" "${tgz[0]}" "$name" > "$pp/pnpm-workspace.yaml"
  if (cd "$pp" && pnpm install --offline --silent --store-dir "$work/pnpm-store" > "$work/pnpm.log" 2>&1); then
    bins pnpm "$pp/node_modules"
  else
    check "pnpm install" 1 "$(tail -n 3 "$work/pnpm.log")"
  fi
fi

if ((fails)); then
  echo "npm-test: FAIL ($fails)"
  exit 1
fi
echo "npm-test: PASS"
