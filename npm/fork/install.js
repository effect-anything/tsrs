// Postinstall of the fork's `typescript` and `@effect-x/tsc` packages: lib/install.js.
//
// On POSIX, rewrites `bin/tsc` as a sh and JS polyglot so `tsc` runs the native platform binary
// directly without paying Node startup overhead (~20 ms), while `node bin/tsc` still runs the JS
// launcher (`../lib/tsc.js`). Falls back to the JS launcher on Windows, under Yarn, outside
// `node_modules`, or if the native binary is absent or fails verification.
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import getExePath from "#getExePath";

const pkgDir = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const pkg = JSON.parse(fs.readFileSync(path.join(pkgDir, "package.json"), "utf8"));
const binPath = path.join(pkgDir, Object.values(pkg.bin)[0]);

function keepLauncherReason() {
    if (process.platform === "win32") return "Windows";
    if (/\byarn\//.test(process.env.npm_config_user_agent ?? "")) return "Yarn";
    if (!pkgDir.split(path.sep).includes("node_modules")) return "not installed in node_modules";
    return undefined;
}

function useNativeBin() {
    if (keepLauncherReason()) return;
    const exe = getExePath();
    const out = execFileSync(exe, ["--version"], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
    const expectedTs = pkg.tscVersion ?? pkg.tsrs?.typescriptVersion ?? pkg.version;
    const want = `Version ${expectedTs}`;
    if (out !== want && !out.startsWith(`${want} (`)) {
        throw new Error(`${exe} reports "${out}", expected "${want}"`);
    }
    const target = path.relative(fs.realpathSync(path.dirname(binPath)), fs.realpathSync(exe));
    const parts = pkgDir.split(path.sep);
    const nodeModules = parts.slice(0, parts.lastIndexOf("node_modules") + 1).join(path.sep);
    const npmLink = path.relative(path.join(nodeModules, ".bin"), binPath);
    for (const p of [target, npmLink]) {
        if (/["$`\\\n\r\u2028\u2029]/.test(p)) {
            throw new Error(`cannot quote the path ${JSON.stringify(p)}`);
        }
    }
    const resolve = [
        `p=$0; g="\${p%/*}/${npmLink}"`,
        'if [ -L "$p" ] && [ ! -L "$g" ] && [ "$p" -ef "$g" ]; then p=$g',
        "else case $p in */*) ;; *) p=./$p ;; esac",
        'while [ -L "$p" ]; do t=$(readlink "$p"); case $t in /*) p=$t ;; *) p=${p%/*}/$t ;; esac; done; fi',
    ].join("; ");
    const run = `t="\${p%/*}/${target}"; [ -x "$t" ] || exec node "$p" "$@"; exec "$t" "$@"`;
    const polyglot = `#!/bin/sh\n":" //; ${resolve}; ${run}\nimport "../lib/tsc.js";\n`;
    const tmp = `${binPath}.${process.pid}.tmp`;
    fs.rmSync(tmp, { force: true });
    try {
        fs.writeFileSync(tmp, polyglot);
        fs.chmodSync(tmp, 0o755);
        fs.renameSync(tmp, binPath);
    }
    catch (e) {
        try {
            fs.rmSync(tmp, { force: true });
        }
        catch {}
        throw e;
    }
}

try {
    useNativeBin();
}
catch (e) {
    console.warn(`${pkg.name}: ${path.relative(pkgDir, binPath)} keeps the Node launcher: ${e instanceof Error ? e.message : e}`);
}
