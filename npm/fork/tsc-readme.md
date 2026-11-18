# @effect-x/tsc

A Rust port of the TypeScript 7 compiler (`tsc`), based on `oxc-project/tsrs`, with built-in
Sweetener hygienic macros (`.sts`, `.stsx`), operator overloading (`"use tsover"`), and TypeGPU
shader transforms (`"use gpu"`).

## Install & Usage

```sh
npm install -D @effect-x/tsc
npx tsc -p tsconfig.json
```

`@effect-x/tsc` accepts the same CLI flags as TypeScript 7 `tsc`. `tsc --version` prints the
TypeScript version it implements so `typesVersions` resolution matches upstream `tsc`.

## VS Code

Point VS Code's TypeScript 7 extension at either `@effect-x/tsc/tsdk` or the platform package's
`lib` directory in `.vscode/settings.json`:

```json
{ "js/ts.tsdk.path": "node_modules/@effect-x/tsc/tsdk" }
```

## Extensions: `tsc-ext`, Node API, and Bundler Unplugin

Register `@effect-x/tsc` as a content mapper in `tsconfig.json` and run `tsc --runExternalCode`:

```json
{
  "contentMappers": [
    {
      "package": "@effect-x/tsc",
      "extensions": [".sts", ".stsx"],
      "options": { "typegpu": { "autoNaming": true } }
    }
  ]
}
```

- **CLI (`tsc-ext`)**:
  - `tsc-ext expand <file>` prints the TypeScript code `tsc` type-checks (`--stage bundle` prints
    bundler output; `--trace`, `--diff`, and `--json` inspect macro expansion).
  - `tsc-ext eval 'code'`, `tsc-ext explain SWR4001`, and `tsc-ext cheatsheet [topic]`.
- **Node API (`@effect-x/tsc/ext`)**:
  - `import { Transformer, Project, formatDiagnostics } from "@effect-x/tsc/ext"`
- **Bundler Plugin (`@effect-x/tsc/unplugin`)**:
  - Vite, Rollup, Webpack, Rspack, and esbuild adapters via `@effect-x/tsc/unplugin/<bundler>`.

## License

Apache-2.0. Preserves upstream notices from `tsrs`, TypeScript (Microsoft Corporation), and Go.
See `NOTICE.txt`.
