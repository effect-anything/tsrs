// lib/getExePath.js of the fork package set (npm/fork/pack.mjs).
//
// The bin launcher (lib/tsc.js), the JS API, bin/tsc-ext, and postinstall (lib/install.js)
// import it as `#getExePath` or `./getExePath.js`.
//
// The native tsc of package <scope>/<name> is lib/tsc in <scope>/<name>-<platform>-<arch>
// (or `@typescript/typescript-<platform>-<arch>` when packed as `typescript`).
// getExePath("tsc-ext") resolves lib/tsc-ext in the same platform package.
import fs from "node:fs";
import module from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

export default function getExePath(exeName = "tsc") {
    if (exeName === "tsc") {
        const override = process.env.TSC_BINARY || process.env.TSRS_BINARY;
        if (override) {
            if (!fs.existsSync(override)) {
                throw new Error(`TSC_BINARY is set to ${override}, which does not exist.`);
            }
            return override;
        }
    }
    const __dirname = path.dirname(fileURLToPath(import.meta.url));
    const { name } = JSON.parse(fs.readFileSync(path.join(__dirname, "..", "package.json"), "utf8"));
    const platformPackageName = name === "typescript"
        ? `@typescript/typescript-${process.platform}-${process.arch}`
        : `${name}-${process.platform}-${process.arch}`;
    if (!platformPackageName.startsWith("@")) {
        throw new Error(`Unable to resolve a platform package for ${name}.`);
    }
    let exeDir;
    try {
        const packageJson = typeof import.meta.resolve === "function"
            ? fileURLToPath(import.meta.resolve(`${platformPackageName}/package.json`))
            : module.createRequire(import.meta.url).resolve(`${platformPackageName}/package.json`);
        exeDir = path.join(path.dirname(packageJson), "lib");
    }
    catch {
        throw new Error(`Unable to resolve ${platformPackageName}. Either your platform is unsupported, or you are missing the package on disk.`);
    }
    let exe = path.join(exeDir, process.platform === "win32" ? `${exeName}.exe` : exeName);
    if (process.platform === "win32" && exe.length >= 248) {
        exe = "\\\\?\\" + exe;
    }
    if (!fs.existsSync(exe)) {
        throw new Error(`Executable not found: ${exe}`);
    }
    return exe;
}
