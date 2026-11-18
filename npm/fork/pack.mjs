// Assembles the fork's `@effect-x/tsc` (or local `typescript`) npm package set directly from
// tsrs's in-tree SDK (`npm/tsrs`) and bundled library definitions (`crates/tsrs_vfs/libs`),
// while keeping upstream `npm/build.mjs` untouched.
//
// Each platform package (`@effect-x/tsc-<os>-<arch>`) ships:
//   - `lib/tsc` (`tsc.exe` on Windows): a `noembed` build of `tsrs_cli`
//   - `lib/lib.*.d.ts`: TypeScript default libraries read next to the executable
//   - `lib/tsc-ext` (`tsc-ext.exe` on Windows) and `lib/tsc-ext.node`: content mapper CLI + N-API addon
//
// The main package (`@effect-x/tsc`) ships:
//   - `bin/tsc` and `bin/tsc-ext` launchers (with optional POSIX polyglot postinstall `lib/install.js`)
//   - `tsdk/tsgo` wrapper for VS Code SDK discovery
//   - `dist/`: TypeScript 7 JS API (`@effect-x/tsc/sync`, `/async`, `/ast`, `/vscode`, ...)
//   - `lib/ext/`: Sweetener + TypeGPU Node API (`@effect-x/tsc/ext`, `/unstable/mapped`, `/unplugin`)
//   - `typescript.contentMapper` manifest (`./bin/tsc-ext`, resolved natively to platform `lib/tsc-ext`)
//
// usage: node npm/fork/pack.mjs --exe <os>-<arch>=<tsc>... [--ext <os>-<arch>=<dir>...]
//          [--libs <dir>] [--dist <dir>] [--version <ts-version>] [--git-head <sha>]
//          --out <dir> [--native-bin] [--name <@effect-x/tsc|typescript>] [--package-version <v>]

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { readVersions } from "../../tools/release/version.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "..", "..");
const npmDir = path.join(repoRoot, "npm");
const tsrsTemplateDir = path.join(npmDir, "tsrs");
const versions = readVersions(repoRoot);

const { values: args } = parseArgs({
    options: {
        exe: { type: "string", multiple: true },
        ext: { type: "string", multiple: true },
        name: { type: "string", default: "@effect-x/tsc" },
        "package-version": { type: "string" },
        libs: { type: "string", default: path.join(repoRoot, "crates", "tsrs_vfs", "libs") },
        dist: { type: "string", default: path.join(tsrsTemplateDir, "dist") },
        version: { type: "string", default: versions.typescriptVersion },
        "git-head": { type: "string" },
        out: { type: "string" },
        "native-bin": { type: "boolean", default: false },
    },
    strict: true,
});

for (const req of ["exe", "git-head", "out"]) {
    if (!args[req]) throw new Error(`missing --${req}`);
}

const { libs, dist, version, "git-head": gitHead, out, name } = args;
if (name !== "typescript" && name !== "@effect-x/tsc") {
    throw new Error(`unknown --name ${name} (expected @effect-x/tsc or typescript)`);
}
if (!fs.existsSync(dist)) {
    throw new Error(`SDK dist directory does not exist (${dist}); build it first with \`node npm/build.mjs --sdk-only\``);
}

const asTypescript = name === "typescript";
const binName = "tsc";
const scope = asTypescript ? "" : name.slice(0, name.indexOf("/"));
const unscoped = asTypescript ? name : name.slice(name.indexOf("/") + 1);
const packageVersion = args["package-version"] ?? versions.version;

const platforms = args.exe.map(value => {
    const match = value.match(/^([a-z0-9]+)-([a-z0-9]+)=(.+)$/);
    const [nodeOs, nodeArch, exe] = match ? match.slice(1) : [process.platform, process.arch, value];
    return {
        nodeOs,
        nodeArch,
        exe,
        packageName: asTypescript
            ? `@typescript/typescript-${nodeOs}-${nodeArch}`
            : `${scope}/${unscoped}-${nodeOs}-${nodeArch}`,
    };
});

const extDirs = new Map((args.ext ?? []).map(value => {
    const match = value.match(/^([a-z0-9]+-[a-z0-9]+)=(.+)$/);
    if (!match) throw new Error(`--ext wants <os>-<arch>=<dir>, not ${value}`);
    return [match[1], match[2]];
}));

if (asTypescript && extDirs.size > 0) throw new Error("--ext is for --name @effect-x/tsc only");
if (!asTypescript) {
    const tscSet = platforms.map(p => `${p.nodeOs}-${p.nodeArch}`).sort().join(" ");
    const extSet = [...extDirs.keys()].sort().join(" ");
    if (tscSet !== extSet) {
        throw new Error(`the tsc-ext platforms (--ext: ${extSet || "none"}) must match the tsc platforms (--exe: ${tscSet})`);
    }
}

const extExeName = nodeOs => (nodeOs === "win32" ? "tsc-ext.exe" : "tsc-ext");

function publishTag() {
    const match = packageVersion.match(/-(dev|alpha|beta|rc)(?:[.-]|$)/);
    if (match?.[1]) return match[1] === "dev" ? "next" : match[1];
    throw new Error(`Refusing to publish 'typescript' with the latest tag from non-release version ${packageVersion}.`);
}

function stripConditions(value) {
    if (Array.isArray(value)) return value.map(stripConditions);
    if (value == null || typeof value !== "object") return value;
    const copy = {};
    for (const [key, v] of Object.entries(value)) {
        if (key === "@typescript/source") continue;
        copy[key] = stripConditions(v);
    }
    const keys = Object.keys(copy);
    return keys.length === 1 && keys[0] === "default" ? copy.default : copy;
}

const writeJson = (file, value) => fs.writeFileSync(file, JSON.stringify(value, undefined, 4) + "\n");

const licenseBytes = fs.readFileSync(path.join(repoRoot, "LICENSE"));
const noticeBytes = fs.readFileSync(path.join(repoRoot, "NOTICE"));
function writeLicense(dir) {
    fs.writeFileSync(path.join(dir, "LICENSE"), licenseBytes);
    fs.writeFileSync(path.join(dir, "NOTICE.txt"), noticeBytes);
}

const template = JSON.parse(fs.readFileSync(path.join(tsrsTemplateDir, "package.json"), "utf8"));
const input = {
    ...template,
    version: packageVersion,
    files: [...new Set([...(template.files ?? []), "NOTICE.txt"])],
    bin: asTypescript
        ? { [binName]: `./bin/${binName}` }
        : { [binName]: `./bin/${binName}`, "tsc-ext": "./bin/tsc-ext" },
    exports: stripConditions(template.exports),
    imports: stripConditions(template.imports),
    gitHead,
    tsrs: {
        typescriptVersion: versions.typescriptVersion,
        typescriptCommit: versions.typescriptCommit,
    },
    publishConfig: asTypescript ? { access: "public", tag: publishTag() } : { access: "public" },
};
delete input.private;
delete input.scripts;
delete input.devDependencies;

if (asTypescript) {
    input.description = "TypeScript is a language for application scale JavaScript development";
    input.homepage = "https://www.typescriptlang.org/";
    input.keywords = ["TypeScript", "Microsoft", "compiler", "language", "javascript"];
    input.bugs = { url: "https://github.com/microsoft/TypeScript/issues" };
    input.repository = { type: "git", url: "https://github.com/microsoft/TypeScript.git" };
}
else {
    input.license = "Apache-2.0";
    input.author = "effect-anything";
    input.description = "A Rust port of the TypeScript 7 compiler with Sweetener and TypeGPU extensions";
    input.keywords = ["typescript", "tsc", "compiler", "rust", "sweetener", "typegpu"];
    input.homepage = "https://github.com/effect-anything/tsrs";
    input.bugs = { url: "https://github.com/effect-anything/tsrs/issues" };
    input.repository = { type: "git", url: "git+https://github.com/effect-anything/tsrs.git" };
}

fs.rmSync(out, { recursive: true, force: true });

// Assemble the main package (`@effect-x/tsc` or `typescript`).
const mainDir = path.join(out, unscoped);
fs.mkdirSync(mainDir, { recursive: true });

for (const entry of ["schemas", "vendor", "UPSTREAM.json"]) {
    const from = path.join(tsrsTemplateDir, entry);
    if (fs.existsSync(from)) {
        fs.cpSync(from, path.join(mainDir, entry), { recursive: true });
    }
}
fs.cpSync(dist, path.join(mainDir, "dist"), { recursive: true });

// `lib/`: JS launcher, version shim, `#getExePath`, optional postinstall, and `/ext` wrappers.
const mainLibDir = path.join(mainDir, "lib");
fs.mkdirSync(mainLibDir, { recursive: true });
const launcherSrc = fs.readFileSync(path.join(tsrsTemplateDir, "lib", "tsrs.js"), "utf8")
    .replaceAll("tsrs:", "tsc:");
fs.writeFileSync(path.join(mainLibDir, "tsc.js"), launcherSrc);
fs.copyFileSync(path.join(here, "getExePath.js"), path.join(mainLibDir, "getExePath.js"));
fs.writeFileSync(path.join(mainLibDir, "getExePath.d.ts"), "export default function getExePath(exeName?: string): string;\n");
fs.copyFileSync(path.join(tsrsTemplateDir, "lib", "version.cjs"), path.join(mainLibDir, "version.cjs"));
fs.copyFileSync(path.join(tsrsTemplateDir, "lib", "version.d.cts"), path.join(mainLibDir, "version.d.cts"));

// `tsdk/`: VS Code native SDK shim (`tsdk/tsgo`).
const mainTsdkDir = path.join(mainDir, "tsdk");
fs.mkdirSync(mainTsdkDir, { recursive: true });
writeJson(path.join(mainTsdkDir, "package.json"), { type: "commonjs" });
fs.writeFileSync(
    path.join(mainTsdkDir, "tsgo"),
    '#!/usr/bin/env node\n"use strict";\nimport("../lib/tsc.js").catch(e => {\n    console.error(`tsc: ${e && e.stack || e}`);\n    process.exit(1);\n});\n',
);
fs.chmodSync(path.join(mainTsdkDir, "tsgo"), 0o755);

// `bin/`: ESM launchers (no `bin/package.json` so postinstall can rewrite `bin/tsc` as sh+ESM polyglot).
const mainBinDir = path.join(mainDir, "bin");
fs.mkdirSync(mainBinDir, { recursive: true });
fs.writeFileSync(path.join(mainBinDir, binName), '#!/usr/bin/env node\nimport "../lib/tsc.js";\n');
fs.chmodSync(path.join(mainBinDir, binName), 0o755);

const mainPackage = {
    ...input,
    name,
    ...(asTypescript ? {} : { tscVersion: version }),
    optionalDependencies: Object.fromEntries(platforms.map(p => [p.packageName, packageVersion])),
};

if (asTypescript) {
    fs.copyFileSync(path.join(repoRoot, "README.md"), path.join(mainDir, "README.md"));
}
else {
    fs.copyFileSync(path.join(here, "tsc-readme.md"), path.join(mainDir, "README.md"));
    fs.writeFileSync(
        path.join(mainBinDir, "tsc-ext"),
        [
            "#!/usr/bin/env node",
            'import { spawnSync } from "node:child_process";',
            'import getExePath from "../lib/getExePath.js";',
            "",
            'const result = spawnSync(getExePath("tsc-ext"), process.argv.slice(2), { stdio: "inherit" });',
            "if (result.error) {",
            '    console.error(`tsc-ext: ${result.error.message}`);',
            "    process.exit(1);",
            "}",
            "if (result.signal) process.kill(process.pid, result.signal);",
            "process.exit(result.status ?? 1);",
            "",
        ].join("\n"),
    );
    fs.chmodSync(path.join(mainBinDir, "tsc-ext"), 0o755);

    const extJs = path.join(repoRoot, "crates", "ts_ext_syntax", "js");
    fs.cpSync(extJs, path.join(mainLibDir, "ext"), {
        recursive: true,
        filter: source => {
            const base = path.basename(source);
            return base !== "test" && !base.endsWith(".node");
        },
    });

    mainPackage.typescript = {
        contentMapper: {
            exec: ["./bin/tsc-ext"],
        },
    };
    mainPackage.exports = {
        ...mainPackage.exports,
        "./ext": { types: "./lib/ext/index.d.ts", default: "./lib/ext/index.cjs" },
        "./unstable/mapped": { types: "./lib/ext/api.d.mts", default: "./lib/ext/api.mjs" },
        "./unplugin": { types: "./lib/ext/unplugin.d.mts", default: "./lib/ext/unplugin.mjs" },
        "./unplugin/*": { types: "./lib/ext/unplugin/*.d.mts", default: "./lib/ext/unplugin/*.mjs" },
    };
    mainPackage.dependencies = { ...mainPackage.dependencies, unplugin: "^3.3.0" };
}

if (args["native-bin"]) {
    mainPackage.scripts = { postinstall: "node lib/install.js" };
    fs.copyFileSync(path.join(here, "install.js"), path.join(mainLibDir, "install.js"));
}

writeJson(path.join(mainDir, "package.json"), mainPackage);
writeLicense(mainDir);

// Assemble per-platform packages (`@effect-x/tsc-<os>-<arch>` or `@typescript/typescript-<os>-<arch>`).
const libNames = fs.readdirSync(libs).filter(f => f.endsWith(".d.ts")).sort();
if (!libNames.includes("lib.d.ts")) {
    throw new Error(`libs directory (${libs}) is missing lib.d.ts`);
}

for (const { nodeOs, nodeArch, exe, packageName } of platforms) {
    const platformDir = path.join(out, `${unscoped}-${nodeOs}-${nodeArch}`);
    const platformLibDir = path.join(platformDir, "lib");
    fs.mkdirSync(platformLibDir, { recursive: true });

    for (const libName of libNames) {
        fs.copyFileSync(path.join(libs, libName), path.join(platformLibDir, libName));
    }

    const exeName = nodeOs === "win32" ? "tsc.exe" : "tsc";
    fs.copyFileSync(exe, path.join(platformLibDir, exeName));
    fs.chmodSync(path.join(platformLibDir, exeName), 0o755);

    const extDir = extDirs.get(`${nodeOs}-${nodeArch}`);
    if (extDir) {
        for (const file of [extExeName(nodeOs), "tsc-ext.node"]) {
            const srcFile = path.join(extDir, file);
            if (!fs.existsSync(srcFile)) {
                throw new Error(`missing ${file} in --ext directory ${extDir}`);
            }
            fs.copyFileSync(srcFile, path.join(platformLibDir, file));
        }
        fs.chmodSync(path.join(platformLibDir, extExeName(nodeOs)), 0o755);
    }

    const platformPackage = {
        name: packageName,
        version: packageVersion,
        license: input.license,
        author: input.author,
        description: `The ${nodeOs}-${nodeArch} native binaries for ${name}`,
        repository: input.repository,
        bugs: input.bugs,
        homepage: input.homepage,
        engines: input.engines,
        preferUnplugged: true,
        files: ["lib", "NOTICE.txt"],
        exports: { "./package.json": "./package.json" },
        os: [nodeOs],
        cpu: [nodeArch],
        gitHead,
        publishConfig: input.publishConfig,
        tsrs: input.tsrs,
    };
    writeJson(path.join(platformDir, "package.json"), platformPackage);
    writeLicense(platformDir);
    fs.writeFileSync(
        path.join(platformDir, "README.md"),
        [
            `# \`${packageName}\``,
            "",
            `This package provides ${nodeOs}-${nodeArch} native binaries (\`tsc\`, \`tsc-ext\`, and \`tsc-ext.node\`) for [${name}](https://www.npmjs.com/package/${name}).`,
            "",
        ].join("\n"),
    );
}
